mod keymap;
mod ui;

use ui::inspector::section_head;
use ui::theme::*;
use ui::toolbar::{EXPORT_SLOT_W, SNAP_SLOT_W, VOLUME_SLOT_W, ZOOM_SLOT_W};
use ui::widgets::*;

use keymap::{ActionId, Keymap};

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use engine::audio::StreamInfo;
use engine::color::ColorParams;
use engine::decode::Backend;
use engine::eq::{Band, BandKind, EqParams};
use engine::export::{AUDIO_KBPS, DEFAULT_AUDIO_KBPS, ExportSettings, Format};
use engine::limiter::Limiter;
use engine::project::{Edge, Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::tonemap::Preset;
use engine::{Clip, Codec, ExportHandle, Frame, MediaBitrate, PlaybackSession};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, CursorStyle, Div, DragMoveEvent,
    FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, Point, RenderImage, ScrollDelta, ScrollHandle, ScrollWheelEvent,
    SharedString, Size, Stateful, TextAlign, TitlebarOptions, Window, WindowBounds, WindowOptions,
    canvas, div, img, point, prelude::*, px, relative, rgb, rgba, size,
};

/// ...and the narrowest it is drawn: a library row whose length the engine has
/// not measured yet has a landing place but no width, and a head marker says
/// where it goes where a zero-width box would say nothing.
const GHOST_MIN: f32 = 2.;

/// Fixed so the video region takes every pixel the window gains and the controls
/// never clip at 640x360.
const HEADER_H: f32 = 32.;
// 28 button row + 24 scrub strip + two 48 lanes + the timecode line + the gaps
// between them, with a few px of slack so a taller text line cannot push a lane
// off the bottom.
const PANEL_H: f32 = 220.;
/// How many lane rows are drawn before the lane column starts scrolling: past
/// this the panel would be taller than the picture it belongs under, and a
/// timeline that pushes the video off the window is not a timeline.
const LANES_MAX: usize = 6;
const LANE_H: f32 = 48.;
/// The lane header column: wide enough for `V1`/`A1` and fixed, so both lanes
/// and the ruler above them start at the same pixel and are the same width --
/// one x-to-time mapping for the whole timeline. `HEADER_GAP` is part of that
/// offset and is therefore shared by all three rows.
const HEADER_W: f32 = 40.;
const HEADER_GAP: f32 = 4.;
/// The label row inside a clip; a waveform paints under it, never through it.
const LABEL_H: f32 = 15.;
/// A clip narrower than this shows no name: two characters and an ellipsis say
/// nothing that the tint has not already said, and cost the picture a smear.
const LABEL_MIN_W: f32 = 36.;
/// Peak buckets per second of source. Fixed and modest on purpose: `peaks`
/// allocates one bucket per window, so a rate taken from anything the user can
/// influence is an allocation bomb -- and 40 is already finer than the pixels a
/// clip is ever given.
///
/// corner-cut: "finer than the pixels" stops being true once the timeline is
/// zoomed -- past ~25 ms per bucket the envelope reads as steps rather than as
/// a shape. Ceiling: [`View::max_zoom`]'s 8 frames across the bed. The upgrade
/// is a second, finer pass over the visible span only, cached like `waves` is.
const WAVE_BPS: u32 = 40;
/// Pixels per envelope column. Coarser than a pixel: the eye reads the shape,
/// and a path with a point per pixel is a path per repaint.
const WAVE_COL: f32 = 2.;
/// The most columns one envelope is ever built from. A waveform is drawn into
/// the slice of its box that is on the bed ([`visible_slice`]), and a bed is a
/// screen wide, so nothing on screen ever reaches this -- it is the backstop for
/// a box laid out wider than any screen, whose path would otherwise cost a point
/// per two pixels of a width nobody can see and stall the repaint that was
/// meant to draw the wave.
const WAVE_COLS_MAX: usize = 4096;
/// WCAG 2.5.8: nothing clickable is smaller than this. The scrub bar stays 6 px
/// to look at -- `RULER_HIT_H` is the strip that has to be hit.
const HIT_MIN: f32 = 24.;
/// The media library's column: a share of the window rather than a fixed width,
/// so it yields on a narrow one, and never more than a third of it -- the
/// picture is what this program is for and keeps the majority at every size.
/// The floor is what a file name and a timecode need to be readable at all.
const LIBRARY_FRAC: f32 = 0.2;
const LIBRARY_MIN_W: f32 = 120.;
const LIBRARY_MAX_W: f32 = 220.;
/// A library row: a name over its duration, two lines and a click target, so
/// `HIT_MIN` binds it like every other one.
const ROW_H: f32 = 32.;
/// The tint swatch down the left of a row: the same colour that source's clips
/// wear in the lanes, which is the whole of the panel<->timeline association.
const SWATCH_W: f32 = 4.;
const CONTROL_H: f32 = 28.;
/// The volume slider beside its button: a hundred steps across it, so a pixel
/// is finer than a step and the drag reads as continuous.
const VOLUME_W: f32 = 110.;
const RULER_HIT_H: f32 = HIT_MIN;
/// Wide enough for `HH:MM:SS:FF / HH:MM:SS:FF`, and fixed so changing digits
/// cannot push the layout around.
const TIME_W: f32 = 200.;
/// The keybindings card: a row per action, a title and a status line, inside a
/// 360 px tall window. The rows are click targets, so `HIT_MIN` binds them too.
/// Wider than the export card, and for the same reason that one is wider than
/// this used to be: at 320 the longest labels ("Remove the last video track (it
/// must be empty)") ran straight over the stroke printed at the other end of
/// their row. Every label in the registry fits beside its stroke here, and the
/// one that cannot -- a row waiting for a key to be pressed -- truncates rather
/// than overprinting. Still inside the 640 px floor.
const KEYS_W: f32 = 480.;
const KEYS_ROW_H: f32 = HIT_MIN;
/// How much of the row list is on screen at once; past this it scrolls. What
/// keeps the card inside the smallest window no matter how many actions the
/// editor grows -- ten rows fit here, and the eleventh is a scroll away.
const KEYS_ROWS_H: f32 = 10. * KEYS_ROW_H;
/// The same for the export card, which carries two summary lines and a button
/// under its list and so has less room: a section header, six codecs, the
/// container, five qualities and the destination are more rows than a 360 px
/// window holds. Eight on screen, which is the whole format section and its
/// header -- what a user picks first is never behind a scroll.
const EXPORT_ROWS_H: f32 = 8. * KEYS_ROW_H;
/// The export card is wider than the keybindings one: its rows carry a key, a
/// name *and* what the choice means, and the two summary lines under them state
/// the whole file. At `KEYS_W` every one of those wrapped to two lines, which is
/// the card the user called unfriendly. Still inside the 640 px floor with the
/// scrim showing either side of it.
const EXPORT_W: f32 = 420.;
/// The column the key of a row is printed in, wide enough for `0–9`: every row
/// in the export card says what picks it, so the card is drivable by keyboard
/// without a legend to memorise.
const EXPORT_KEY_W: f32 = 26.;
/// Everything in the export card that is *not* the row list: the title, the
/// status line, the head and tail of the summary, the button, the gaps between
/// them and the padding around the lot. What the list may be is the window
/// minus this -- and never less than [`EXPORT_ROWS_H`], which is the number
/// that makes the card fit the 360 px floor.
const EXPORT_FIXED_H: f32 = 17. + 28. + 15. + 30. + CONTROL_H + 4. + 10. + 24.;
/// The menu a right-click on a clip opens: wide enough for the longest label
/// beside the stroke that does the same thing, with the click targets `HIT_MIN`
/// binds like every other list here.
const MENU_W: f32 = 260.;
const MENU_ROW_H: f32 = HIT_MIN;
const MENU_PAD: f32 = 6.;

/// A cue's text, and the line it sits on. Fixed rather than a share of the
/// picture: the video region is 108 px tall at the 640x360 floor and a
/// proportional size there would be unreadable at exactly the size where it has
/// to be read.
const SUB_TEXT: f32 = 14.;
const SUB_LINE_H: f32 = 18.;
/// How far off the bottom of the picture the plate sits.
const SUB_BOTTOM: f32 = 8.;
/// The subtitle strip under the lanes: thin, because it is a picture of where
/// the cues are and nothing on it can be dragged -- `HIT_MIN` binds targets, and
/// this row has none.
const SUB_LANE_H: f32 = 16.;
/// A mark narrower than this is still drawn this wide: a one-frame cue on a
/// zoomed-out bed is worth a fraction of a pixel, and a mark nobody can see says
/// the track is empty. The silence preview's marks are floored by it too -- they
/// are the same kind of thing, a picture of where something is and no target.
const SUB_CUE_MIN_W: f32 = 2.;
/// How much of the subtitle list in the library column is on screen at once,
/// past which it scrolls -- the media list above it is what keeps the height.
const SUB_ROWS_H: f32 = 3. * ROW_H;
/// The row naming the file a block of tracks came out of. Shorter than a track
/// row because nothing on it is clicked -- `HIT_MIN` binds targets and a header
/// is a label, not a way in -- and drawn at all only where the column has the
/// height to show tracks under it ([`sub_headers_fit`]).
const SUB_HEAD_H: f32 = 18.;
/// How much of the subtitle list is on screen at the 640x360 floor: one row,
/// measured -- the section's own heading and the Add button under it take the
/// rest of the 84 px the column has there.
const SUB_ROWS_AT_FLOOR: f32 = ROW_H;
/// How much of a subtitle row's width the file's name in front of the label may
/// take. Half: which file and which language are both worth reading, and a name
/// given the whole row is a row where the language is what gets truncated.
const SUB_STEM_SHARE: f32 = 0.5;
/// Roughly how wide one character of an 11 px list row is. Generous on purpose:
/// the element truncates for real, and a budget that overshoots would have the
/// element cut the tail off after [`clip_middle`] had already kept it.
const LIST_CHAR_W: f32 = 6.;
/// The fewest characters a clipped name is cut to, however narrow the column
/// gets: past this there is nothing on either side of the gap to read.
const LIST_CLIP_MIN: usize = 5;
/// How long the export card's Subtitles line may get before it counts tracks
/// instead of naming them ([`subtitle_plan`]). Three lines of that row's value
/// box, at [`LIST_CHAR_W`] to a character: [`EXPORT_W`] less the row's padding
/// and gap, its tick and [`EXPORT_KEY_W`] key column, and the word "Subtitles"
/// in front of the value. Not a track count -- what walks the Destination row
/// off the bottom of the card is the *wrapping*, and thirty-five one-word
/// labels wrap less than three long ones.
const SUB_PLAN_CHARS: usize = (3.
    * (EXPORT_W - 12. - 12. - (10. + 8. + EXPORT_KEY_W + 8. + 9. * LIST_CHAR_W))
    / LIST_CHAR_W) as usize;

/// The one key name this file still spells out, and gpui's spelling of it: it
/// is the way out of a capture and out of the overlay, and both have to work
/// while the keymap itself is what is being changed -- so neither can go
/// through the keymap to find it.
const ESCAPE: &str = "escape";

/// What the header says with no timeline open, and what the window title reads
/// as a program name rather than as a file name.
const NO_FILE: &str = "no file open";

/// What a press of play says when there is nothing to play: no timeline at all
/// and an emptied one are the same answer to the user, so they are one line.
const NOTHING_TO_PLAY: &str = "NOTHING TO PLAY — put a clip on the timeline first";

/// Whether a press of play would have anything to play. No timeline at all and
/// one every clip has been taken off are the same state to a transport, and the
/// button and the key both have to give the same answer to it -- so there is
/// one of them, and it is free of the window so it can be checked without one.
fn nothing_to_play(session: Option<&PlaybackSession>) -> bool {
    session.is_none_or(PlaybackSession::is_empty)
}

/// What the monitoring output is set to. Two things, not one: the level the
/// user picked, and whether it is being held silent -- so unmuting comes back
/// to the level rather than to a guess.
///
/// The level counts steps rather than carrying an `f32`, because 5% at a time
/// down and back up again through a float would not land on the number it
/// started from, and the label would eventually read `79%`.
///
/// Volume and mute stay independent on purpose: turning the level down while
/// muted must not be what makes sound come out. Only the mute key unmutes, and
/// the button says both things at once ("Muted 80%") so neither is a surprise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Volume {
    steps: u8,
    muted: bool,
}

impl Volume {
    /// One step is one percent: fine enough that a drag along the slider reads
    /// as continuous, and still a count rather than a float.
    const MAX_STEPS: u8 = 100;

    /// 5% a press: twenty presses across the range, which is what the keys have
    /// always moved. The slider is what the finer grid is for.
    const KEY_STEP: u8 = 5;

    /// What the device is set to: mute wins, and the level is what it returns
    /// to. `0.0..=1.0`, which is the range the plugin's ABI accepts.
    fn gain(self) -> f32 {
        if self.muted { 0. } else { self.along() }
    }

    /// One press up or down, clamped at both ends -- saturating, so the count
    /// cannot wrap past silence into full volume.
    fn step(&mut self, up: bool) {
        self.steps = if up {
            self.steps.saturating_add(Self::KEY_STEP).min(Self::MAX_STEPS)
        } else {
            self.steps.saturating_sub(Self::KEY_STEP)
        };
    }

    /// Where the hand let go along the slider, 0..1 from silence to full. The
    /// grid is the same one the keys land on, so a drag to the top and a key
    /// held up reach the very same number -- and a drag never touches mute:
    /// asking for a level while muted is not asking for sound.
    fn set_along(&mut self, frac: f32) {
        self.steps = (frac.clamp(0., 1.) * f32::from(Self::MAX_STEPS)).round() as u8;
    }

    /// How full the slider is drawn, 0..1. The level and not the gain: a muted
    /// slider still shows what unmuting comes back to, exactly as the label
    /// does.
    fn along(self) -> f32 {
        f32::from(self.steps) / f32::from(Self::MAX_STEPS)
    }

    /// The level as a whole number, for the button's fixed rect: muting swaps
    /// the glyph beside it, never the width of the box.
    fn percent(self) -> u32 {
        u32::from(self.steps) * 100 / u32::from(Self::MAX_STEPS)
    }

    /// What the button reads. The level shows while muted too: it is what the
    /// next press of the mute key brings back.
    fn label(self) -> String {
        let percent = u32::from(self.steps) * 100 / u32::from(Self::MAX_STEPS);
        if self.muted {
            format!("Muted {percent}%")
        } else {
            format!("Vol {percent}%")
        }
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self {
            steps: Self::MAX_STEPS,
            muted: false,
        }
    }
}

/// The library's three categories, in the order the giants list them: the
/// pictures, the sound, and the words. A file is in exactly one of them, so a
/// tab is a question with an answer rather than a filter with a guess.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LibraryTab {
    Media,
    Audio,
    Text,
}

const LIBRARY_TABS: [LibraryTab; 3] = [LibraryTab::Media, LibraryTab::Audio, LibraryTab::Text];

impl LibraryTab {
    fn label(self) -> &'static str {
        match self {
            LibraryTab::Media => "Media",
            LibraryTab::Audio => "Audio",
            LibraryTab::Text => "Text",
        }
    }

    /// Whether a source belongs on this tab. Subtitles are not sources at all
    /// -- they are the [`Player::subtitle_section`] under the list -- so the
    /// Text tab holds no rows of its own and says so.
    fn holds(self, path: &Path) -> bool {
        match self {
            LibraryTab::Media => !engine::is_audio(path),
            LibraryTab::Audio => engine::is_audio(path),
            LibraryTab::Text => false,
        }
    }

    /// What an empty tab says instead of being a blank column.
    fn empty(self) -> &'static str {
        match self {
            LibraryTab::Media => "No video or stills yet — Import, or drop a file on the window",
            LibraryTab::Audio => "No sound yet — Import, or drop a file on the window",
            LibraryTab::Text => {
                "No subtitles yet — Add subtitles from a file, or drop an .srt on the window"
            }
        }
    }
}

/// What is known about a source's audio. Three states and not two, because a
/// file whose peaks have not come back yet must not be drawn as one that has no
/// audio at all: the first shows a bed, the second shows nothing.
#[derive(Clone)]
enum Wave {
    /// Asked for; the decode is running on a background thread.
    Loading,
    /// The file has no audio track. An answer, not a miss.
    Silent,
    /// The decode failed. Drawn as its own mark rather than as [`Self::Silent`]:
    /// "this file's sound could not be read" and "this file has no sound" look
    /// the same on a lane, and the first is a bug report waiting to happen.
    Failed,
    Peaks(Arc<Vec<(f32, f32)>>),
}

/// The import a worker is reading, as the line above the panel shows it. No
/// fraction anywhere: neither read reports how far into the file it has come,
/// so what is honest is the file's name, the stage, and two clocks -- one that
/// proves the window is answering, one that says the stage has not moved.
struct Import {
    path: PathBuf,
    /// When the worker started, for the elapsed clock.
    started: Instant,
    /// Written by the worker between its two reads, read here every repaint.
    stage: Arc<std::sync::atomic::AtomicU8>,
    /// The stage the line last saw and when it last changed. The pair is the
    /// whole stall detector: a stage older than [`IMPORT_STALL`] is what the
    /// honest wording is for.
    seen: ImportStage,
    since: Instant,
    /// Set by the Cancel beside the line ([`Player::cancel_import`]), read at
    /// the landing: what the worker read is dropped instead of joining the
    /// timeline.
    ///
    /// corner-cut: the *read* is not stopped -- a demuxer walk polls nothing, so
    /// the worker finishes into a result nobody takes, and the window is given
    /// back at the click either way. Ceiling: a cancelled cold 24 GB import
    /// still costs the disk its twenty seconds. Upgrade: a flag
    /// `engine::demux::Demuxer::open` polls between clusters, which is where an
    /// export's own cancel already lives ([`engine::ExportHandle::cancel`]).
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl Import {
    /// Reads the worker's stage and keeps the stall clock: it restarts when the
    /// stage actually changes and at no other time, which is what makes "has
    /// not moved in five seconds" a fact rather than a guess about the elapsed
    /// clock. Hands back how long the current stage has stood, so the line has
    /// one place to ask.
    fn poll(&mut self) -> f32 {
        let stage = ImportStage::from_u8(self.stage.load(std::sync::atomic::Ordering::Relaxed));
        if stage != self.seen {
            self.seen = stage;
            self.since = Instant::now();
        }
        self.since.elapsed().as_secs_f32()
    }
}

/// What a scan is *of*: the source, which of its audio streams, and the clip's
/// own `[in, out)` source frames. The range is part of the name because it is
/// part of the read -- a clip cut in half is a different, shorter decode than
/// the whole take was, and levels read for one are not the other's.
type ScanKey = (PathBuf, usize, u32, u32);

/// The silence scan a worker is running, as the card shows it. Same two clocks
/// as an [`Import`] and for the same reason -- one proves the window answers,
/// one says the read has stopped moving -- over a progress that *can* move:
/// a decode knows how far into the sound it has come, so the card says so.
struct SilenceScan {
    /// Source, stream and source range being scanned, which is the cache key
    /// the levels land under and what tells a second open of the same clip from
    /// a new one.
    key: ScanKey,
    /// When the worker started, for the elapsed clock.
    started: Instant,
    /// Written by the worker, read here every repaint. The cancel flag in it is
    /// this side's only word to a scan already running.
    progress: Arc<engine::silence::Progress>,
    /// The tenths-of-a-second mark the line last saw and when it last changed:
    /// the stall detector, exactly [`Import::poll`]'s.
    seen: u64,
    since: Instant,
}

impl SilenceScan {
    /// Reads the worker's mark and keeps the stall clock, restarting it only
    /// when the mark actually moves -- [`Import::poll`]'s contract, over a
    /// number instead of a stage.
    fn poll(&mut self) -> f32 {
        let scanned = self.progress.scanned.load(std::sync::atomic::Ordering::Relaxed);
        if scanned != self.seen {
            self.seen = scanned;
            self.since = Instant::now();
        }
        self.since.elapsed().as_secs_f32()
    }
}

/// A library row being dragged: the file and which of its audio streams that
/// row is, which is the whole of what a row names. Where it lands does not
/// change what is inserted.
struct AssetDrag(PathBuf, usize);

/// A clip already on the timeline being dragged: the lane it is on and its index
/// there, which is how every other edit names a clip. Unlike an [`AssetDrag`]
/// nothing is inserted -- the same clip changes lane and keeps the frames it
/// plays -- but where along the bed it is let go is exactly where it lands, less
/// the offset the hand grabbed it at ([`Player::grab`]).
#[derive(Clone, Copy)]
struct ClipDrag {
    lane: Lane,
    idx: usize,
    /// The clip that was picked up, so the drop can find it again: gpui freezes
    /// the payload for the whole gesture, and an edit made *during* one -- a
    /// stroke deletes, undoes or pastes, none of which a drag blocks -- ripples
    /// the indices under it. The index alone would then name a different take at
    /// the release, and the drag would move a clip nobody touched (see
    /// [`live_idx`]).
    clip: Clip,
}

/// A track *header* being dragged: which track the hand took hold of, to be
/// let go over the header of the one whose place it is to take
/// ([`Player::reorder_lane`]). The lane alone -- a track carries its own clips
/// wherever it goes, so unlike a [`ClipDrag`] there is nothing else to name.
#[derive(Clone, Copy)]
struct LaneDrag(Lane);

/// Where a header drag in flight would leave the track in the hand: the lane
/// whose slot it is about to take, and whether the line is drawn at that lane's
/// top edge (it is coming up from below) or its bottom one. The drop indicator
/// every editor draws between two tracks, and the header answer to the ghost a
/// clip drag lays on a lane ([`Ghost`]) -- stale between gestures, which costs
/// nothing because it is drawn only while one is live.
#[derive(Clone, Copy, PartialEq)]
struct LaneDrop {
    lane: Lane,
    above: bool,
}

/// Where the drag in flight would leave what it is carrying: the lane the
/// pointer is over, the snapped head the release will commit ([`landing`]), and
/// how long the thing is. Drawn on that lane as a translucent box the size of
/// the take, so a landing is *seen* before the release rather than discovered
/// after it -- the line ([`Player::snap_cue`]) marks the frame, this shows the
/// body. `refused` is a drop the lane cannot take -- a picture over an audio
/// track, a sound over a video one -- tinted rather than silent, because the
/// refusal is coming at the release either way ([`lane_refuses`],
/// [`Project::move_clip`]).
#[derive(Clone, Copy, PartialEq)]
struct Ghost {
    lane: Lane,
    start: u32,
    /// Timeline frames, which a speed has already been counted into: the box is
    /// as wide as the clip is *long where it lands*. Zero for a library row of
    /// unknown length, drawn as a head marker.
    frames: u32,
    /// The swatch of the file being carried ([`file_tint`]), so the
    /// ghost reads as the thing in the hand.
    tint: u32,
    refused: bool,
}

/// A clip edge being dragged: which end of which clip, and the timeline frame
/// the pointer has pulled it to. The box on screen is drawn from `to` while this
/// is set and the engine hears about it once, at the release
/// ([`Player::commit_trim`]) -- one edit, one undo step for the whole gesture,
/// exactly as an equalizer drag works.
#[derive(Clone, Copy)]
struct Trim {
    lane: Lane,
    idx: usize,
    edge: Edge,
    /// Already clamped by `PlaybackSession::trim_room`, so the width drawn from
    /// it is the width the release commits -- an edge stops under the pointer
    /// rather than snapping back after the fact.
    to: u32,
    /// The dragged clip's group, so its other halves' boxes follow the edge on
    /// screen exactly as the engine will move them.
    link: Option<u32>,
}

/// How wide a clip's edge is as a *target*: the strip at each end where a press
/// means "make this longer or shorter" instead of "move this to another lane".
/// Wide enough to hit, narrow enough that the middle of even a small box is
/// still the body.
const EDGE_W: f32 = 6.;

/// Whether a clip box this wide gets its two trim strips at all. Below three
/// handles wide the pair would occlude the whole box: every press on it would
/// trim, and the clip could not be selected, dragged to another lane or picked
/// up by its middle -- which is exactly what a jumpcut leaves behind
/// ([`Player::cut_silences`] manufactures a great many short clips). Above it,
/// what is left between the two strips is a handle's width of body in its own
/// right. A clip too short for its handles is trimmed by zooming in first: the
/// bed is a magnifier, and the strip grows with the box.
fn trims(width: f32) -> bool {
    width >= 3. * EDGE_W
}

/// How wide a clip's box is *drawn*, given the width its own length is worth
/// (`span`). Never under [`HIT_MIN`], even where that is wider than the clip is
/// long: zoomed far out a short take is worth a fraction of a pixel, and a box
/// nobody can put a pointer on is a clip that cannot be selected, dragged, given
/// a menu or reached at all -- which is strictly worse than one drawn a few
/// pixels too wide. The same call [`cue_box`] makes for a mark, and what every
/// editor draws.
///
/// A drawing only: [`Scale::time_at`] still reads the bed, so a press inside the
/// padding names the frame it points at, and the box's head is the clip's own.
fn clip_width(span: f32) -> f32 {
    span.max(HIT_MIN)
}

/// The sheet a card or a menu is painted on: the whole window, and the mouse
/// stops at it. Occluding is what tells gpui that nothing under this sheet is
/// hovered any more (`Hitbox::is_hovered`) -- without it the window carries on
/// hovering behind an open menu and pops *its* tooltip over the menu's items,
/// which is a card being painted over by the thing it covers.
///
/// Every card and every menu takes its sheet from here, so no surface can be
/// drawn over the top of one by having been given a plain scrim.
fn scrim() -> Div {
    div().absolute().inset_0().occlude()
}

/// The sheet a card with a *slider* in it takes instead: the same scrim, with
/// the window's own drag listeners on it as well.
///
/// A scrim occludes, and occluding is where gpui's hit test stops
/// (`Hitbox::is_hovered`, window.rs:788) -- so while a card is up the root is
/// not hovered anywhere behind it and its `on_mouse_move`/`on_mouse_up` hear
/// nothing at all. Every drag in this window is tracked from the root, because
/// each of them starts on a strip a few pixels wide that the pointer leaves at
/// once ([`Player::drag_move`]), so a card's handles were set by the press and
/// then frozen: the value never followed the hand and the release never wrote.
/// The scrim is the one surface above the occluder that covers the whole card,
/// so the same two listeners go here, and a drag that leaves the card is picked
/// up by the root's copy of them without a seam.
fn drag_scrim(cx: &mut Context<Player>) -> Div {
    scrim()
        .on_mouse_move(cx.listener(Player::drag_move))
        .on_mouse_up(MouseButton::Left, cx.listener(Player::drag_release))
}

/// A press that stops here. What every card's body hands its scrim: the scrim
/// closes the card on a press, and the card is painted after it, so this listener
/// runs first (gpui dispatches topmost-first, window.rs:3705) and a press meant
/// for a button never closes the card out from under its own click -- the rule
/// the menus already follow ([`Player::library_card`]).
fn swallow(_: &MouseDownEvent, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

/// How close to an edge a dragged clip has to be let go for it to land *on* it:
/// the snap every timeline has, in pixels rather than frames so that it feels
/// the same at every zoom. Narrower than [`EDGE_W`] -- a hand aiming between two
/// takes must still be able to leave a gap of a few frames there.
const SNAP_PX: f64 = 5.;

/// An open clip menu: which clip it was opened on, where it hangs, and whether
/// it has been turned over to show what that clip *is* instead of what can be
/// done to it. The lane and index are the ones the same click selected, so
/// every item acts on exactly the box under the pointer.
#[derive(Clone, Copy)]
struct ContextMenu {
    lane: Lane,
    idx: usize,
    at: Point<Pixels>,
    details: bool,
}

/// An open library menu: which row it was opened on -- the file and the stream,
/// the pair [`Player::selected_asset`] holds, so a list rebuilt under it (a
/// probe landing, a source going) cannot slide another row beneath the menu the
/// way a row *index* would -- where it hangs, and whether it has been turned
/// over to show what the file *is*.
#[derive(Clone)]
struct LibraryMenu {
    path: PathBuf,
    stream: usize,
    at: Point<Pixels>,
    details: bool,
}

/// An open choice list: which setting it offers and where it hangs. What a
/// button that stepped one value on per click used to be -- a setting with more
/// than two values is a list to look at, not a thing to click round. Placed by
/// [`menu_at`] and closed by a stroke, exactly like the two menus above it.
#[derive(Clone, Copy, PartialEq)]
struct Picker {
    of: Pick,
    at: Point<Pixels>,
    /// Which row the keyboard is on, from the value in force when the list
    /// opened. The list answers ↑↓ and enter as well as a click: a setting whose
    /// only door is a pointer is a setting half this editor's users cannot
    /// reach, which is the rule `FIXED` already writes down for every card.
    sel: usize,
}

/// Which setting an open list is offering. The fit policy names the clip it is
/// about, like the clip menu it opens from -- indices move under every edit, so
/// the list closes on the first stroke as that menu does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pick {
    Resolution,
    Fps,
    Fit(Lane, usize),
    /// What the export's *sound* is coded at. Opened from the card's Sound row,
    /// which is the only place it means anything.
    AudioRate,
    /// Which HDR-to-SDR rendition the project is watched and exported in
    /// ([`engine::tonemap::Preset`]). Opened from the panel, beside the two
    /// other settings that are the project's rather than the media's.
    Tone,
    /// Which palette the window is painted in ([`ui::theme`]). The one setting
    /// here that is nobody's project: it is the person's, so it outlives the
    /// timeline and every file opened in it.
    Theme,
}

/// One value a list offers, carrying everything picking it needs -- so a click
/// goes straight to the value rather than to a position in a list that was
/// built somewhere else.
/// `Eq` is off it for the rate: a frame rate is the `f64` the engine is told,
/// bit for bit (23.976023976... is not 23.976), and nothing here keys a map on a
/// choice -- comparing two is all a list row ever does.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Choice {
    Size(u32, u32),
    Fps(f64),
    Fit(Lane, usize, FitPolicy),
    AudioRate(u32),
    Tone(Preset),
    Theme(ui::theme::PaletteId),
}

/// One row of an open list: the value, its name, the small print beside it, and
/// whether it is the one in force.
type ChoiceRow = (Choice, SharedString, SharedString, bool);

/// What a library row's menu offers, in the order it lists them. Unlike the clip
/// menu's items none of these is a stroke -- there is no keyboard way to a row --
/// so the label and the hint are written here rather than read off the keymap.
#[derive(Clone, Copy, PartialEq)]
enum RowItem {
    Add,
    Remove,
    Reveal,
    Properties,
}

const ROW_ITEMS: [RowItem; 4] = [
    RowItem::Add,
    RowItem::Remove,
    RowItem::Reveal,
    RowItem::Properties,
];

impl RowItem {
    fn label(self) -> &'static str {
        match self {
            Self::Add => "Add at playhead",
            Self::Remove => "Remove from library",
            Self::Reveal => "Reveal in files",
            Self::Properties => "Properties",
        }
    }

    /// The dim right-hand column, where the clip menu prints the stroke: what
    /// the item will do to the timeline, so nothing here is a surprise.
    fn hint(self) -> &'static str {
        match self {
            // Short enough to sit beside its label inside `MENU_W`, the clip
            // menu's rule for a refusal: this column truncates, and a hint cut
            // off mid-word says less than a shorter one.
            Self::Add => "the whole file",
            Self::Remove => "nothing plays it",
            Self::Reveal => "file manager",
            Self::Properties => "…",
        }
    }
}

/// What the menu offers, in the order it lists them. Every one of these is an
/// action a stroke already reaches -- the menu is a second way *to* the actions
/// and never a second version of them -- so both the label and the hint come
/// out of the keymap registry and the two can never disagree.
const MENU_ITEMS: [ActionId; 14] = [
    ActionId::Cut,
    // The clipboard pair, which had no door but a chord: copy takes the clip the
    // menu names, and paste is the timeline's rather than this clip's -- the
    // same kind of global item the mute below already is.
    ActionId::Copy,
    ActionId::Paste,
    ActionId::Delete,
    ActionId::Lift,
    ActionId::Regroup,
    ActionId::Detach,
    ActionId::Group,
    ActionId::Equalizer,
    ActionId::Speed,
    // The scan is a clip card like the two above it -- opened on whichever half
    // was clicked -- and a card only a stroke could open is one a pointer never
    // finds.
    ActionId::Silence,
    ActionId::Color,
    ActionId::Fit,
    ActionId::ToggleMute,
];

/// One row of the actions card, in the order it lists them: a heading, then
/// every action the registry files under it, then the strokes the modal cards
/// answer themselves.
///
/// A list rather than a loop inside the render, so the card and
/// `every_action_is_on_the_actions_card` read the *same* order: an action that
/// reaches no row fails a test instead of quietly becoming pointer-unreachable.
enum KeyRow {
    Head(keymap::Category),
    /// Click its label to do it, click its stroke to change that stroke.
    Act(ActionId),
    /// An index into [`keymap::FIXED`]. Shown and never offered: nothing may
    /// unbind the way out of a card.
    Fixed(usize),
}

/// Every action, under its heading, and the card-local strokes beside them.
/// Generated from the registry -- [`ActionId::ALL`] in its own order, under
/// [`keymap::Category::ALL`] -- so an action added there is on the card the
/// moment it exists and there is no second list here to forget.
fn keys_rows() -> Vec<KeyRow> {
    let mut rows = Vec::new();
    for category in keymap::Category::ALL {
        rows.push(KeyRow::Head(category));
        rows.extend(
            ActionId::ALL
                .into_iter()
                .filter(|a| a.category() == category)
                .map(KeyRow::Act),
        );
        rows.extend(
            keymap::FIXED
                .iter()
                .enumerate()
                .filter(|(_, f)| f.category == category)
                .map(|(i, _)| KeyRow::Fixed(i)),
        );
    }
    rows
}

/// [`keys_rows`] with a search applied: the rows whose label or whose stroke
/// carries `needle`, and the heading each one lives under. A heading with
/// nothing left beneath it goes with them -- an empty "Playback" over a gap
/// reads as a list that lost its rows rather than as a search that found none.
///
/// Each row keeps the index it has in the unfiltered list, so an element id is
/// the same one before and after a keystroke: filtering is a look at the list,
/// and gpui's per-element state is keyed on that id.
///
/// Case-insensitive substring, on both columns: people look for an action by
/// what it does ("vol") and for a stroke by what they pressed ("ctrl").
fn keys_filter(needle: &str, keymap: &Keymap) -> Vec<(usize, KeyRow)> {
    let needle = needle.trim().to_lowercase();
    let rows = keys_rows().into_iter().enumerate();
    if needle.is_empty() {
        return rows.collect();
    }
    let hit = |label: &str, chord: &str| {
        label.to_lowercase().contains(&needle) || chord.to_lowercase().contains(&needle)
    };
    let mut out: Vec<(usize, KeyRow)> = Vec::new();
    // The heading above the row being looked at, until a row under it earns it
    // a place -- then it goes in once, ahead of that row.
    let mut pending: Option<(usize, KeyRow)> = None;
    for (i, row) in rows {
        match &row {
            KeyRow::Head(_) => pending = Some((i, row)),
            KeyRow::Act(action) => {
                if hit(action.label(), &keymap.display(*action)) {
                    out.extend(pending.take());
                    out.push((i, row));
                }
            }
            KeyRow::Fixed(f) => {
                let fixed = &keymap::FIXED[*f];
                if hit(fixed.label, &fixed.chord) {
                    out.extend(pending.take());
                    out.push((i, row));
                }
            }
        }
    }
    out
}

/// The character a stroke types into the actions card's search box, if it types
/// one. gpui reports a printable key as itself and the space bar by name
/// (platform.rs:866), and everything else -- the arrows, the function keys --
/// is a word this must not spell into the box letter by letter.
fn typed(key: &str) -> Option<char> {
    match key {
        "space" => Some(' '),
        _ => key
            .chars()
            .next()
            .filter(|c| c.is_ascii_graphic() && key.chars().count() == 1),
    }
}

/// The project resolutions [`Player::cycle_resolution`] offers, largest first.
/// A short list of the sizes people name; the media's own is cycled in beside
/// them, which is what makes the trip round come back to where it started.
const RESOLUTIONS: [(u32, u32); 5] = [
    (3840, 2160),
    (2560, 1440),
    (1920, 1080),
    (1280, 720),
    (854, 480),
];

/// The project frame rates the list offers, slowest first: the rates footage is
/// actually shot and delivered at, the NTSC ones written as the ratios they are
/// (`24000/1001`, not `23.976`) -- the engine conforms the timeline to the very
/// number it is handed, so a rate rounded here would be a rate no timescale can
/// name. The media's own is cycled in beside them
/// ([`frame_rate_ladder`]), which is what keeps the way back on the list.
const FRAME_RATES: [f64; 8] = [
    24_000. / 1001.,
    24.,
    25.,
    30_000. / 1001.,
    30.,
    50.,
    60_000. / 1001.,
    60.,
];

/// How far a band may be pushed either way, in dB. The engine clamps nothing
/// here -- it will filter whatever it is given -- so this is a UI decision:
/// past about this a peaking band stops sounding like tone and starts sounding
/// like a fault.
const EQ_GAIN_LIMIT: f32 = 12.;

/// One dB per keystroke, which is roughly the smallest step anyone hears on a
/// single band.
const EQ_STEP: f32 = 1.;

/// A twentieth of real time per keystroke, in the thousandths a [`Speed`] is
/// held in: fine enough to creep up on a rate, coarse enough that the whole
/// range is eighty presses and not eight hundred -- and it divides 1000, so
/// stepping from anywhere lands on exactly 1.00x on the way past.
const SPEED_STEP: i32 = 50;

/// The rates the card's buttons offer, so the ones people actually name are one
/// click and not a drag. Real time is among them: it is the reset.
const SPEED_PRESETS: [u16; 6] = [250, 500, 1000, 1500, 2000, 4000];

/// A rate from a number of thousandths that may have run off either end -- what
/// a keystroke and a drag both produce. Clamped, not refused: a hand pushing
/// past the limit means "as far as it goes", exactly as a trim does.
fn speed_at(permille: i32) -> Speed {
    Speed::from_permille(permille.clamp(0, i32::from(u16::MAX)) as u16)
}

/// The silence card's rows, in the order it lists them: how wide the apply
/// reaches, the threshold and the unit it is read in, the three durations a
/// scan is told, and the rate the speed-up plays at. What `silence_field`
/// indexes and what [`Player::nudge_silence`] moves.
const SILENCE_ROWS: usize = 7;

/// How wide a jumpcut reaches. A ripple used to be the whole timeline's
/// business and nothing else; it is a *choice* now, because a podcast track's
/// silences are not the music track's business -- and the choice has to be on
/// screen, because "everything after this moved" is not a thing to discover
/// afterwards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// The lanes of the take the scanned clip belongs to: its picture and its
    /// sound stay one take, and nothing else moves. The default, because a clip
    /// picked on screen is what a person means by "this".
    Take,
    /// That clip's lane, alone. Refused by the engine, by name, while the take
    /// has a half elsewhere -- detaching is how a person says they mean it.
    Track,
    /// Every lane, which is what a ripple always was and what the timeline-wide
    /// jumpcut still is.
    Everything,
}

/// The order the card cycles them in.
const SCOPES: [Scope; 3] = [Scope::Take, Scope::Track, Scope::Everything];

impl Scope {
    /// What the row says, given the lanes it works out to: the *names* of the
    /// tracks, because "this take" means nothing until it says which two.
    fn label(self, lanes: &[Lane]) -> String {
        let named = lanes
            .iter()
            .map(|l| l.label())
            .collect::<Vec<_>>()
            .join("+");
        match self {
            Scope::Take => format!("this take ({named})"),
            Scope::Track => format!("this track ({named})"),
            Scope::Everything => "every track".to_string(),
        }
    }
}

/// One press of a nudge key on each kind of row: a dB on the threshold, a
/// twentieth of a second on the three durations, and the speed card's own step
/// on the rate.
const SILENCE_DB_STEP: f32 = 1.;
const SILENCE_SECS_STEP: f64 = 0.05;

/// How far each of them may be pushed. UI decisions, all of them: the engine
/// takes any finite number, but a forgiveness of ten seconds finds nothing in a
/// talking head. The threshold reaches full scale, which calls a whole take
/// silent -- that is a thing someone may want to ask for (the preview on the
/// lane says what it would cost before anything is cut), so the top is 0 rather
/// than a number this card picked for them.
const SILENCE_DB_RANGE: (f32, f32) = (-80., 0.);
const SILENCE_SECS_RANGE: (f64, f64) = (0., 5.);

/// One press of a nudge key on the mix card, in dB: the step every fader and
/// the limiter's ceiling moves by. A whole decibel, the smallest move anyone
/// hears as a move -- and it lands on round numbers, so a track set by ear
/// still reads as a number a person would say.
const MIX_DB_STEP: f32 = 1.;

/// The mix card's rows below the faders: the limiter's ceiling and its switch.
/// One fader per audio track comes first, however many there are.
const MIX_MASTER_ROWS: usize = 2;

/// The speed-up rate is bounded *below* by real time: a "speed-up" that slows
/// the silence down would make the timeline longer, which is the one thing
/// neither button may do. The top is [`Speed::MAX`].
fn silence_rate(permille: i32) -> Speed {
    speed_at(permille.clamp(
        i32::from(Speed::NORMAL.permille()) + SPEED_STEP,
        i32::from(Speed::MAX.permille()),
    ))
}

/// The graph's frequency axis: the range an ear works in, and the range every
/// band a file can carry sits inside. Log-spaced, so an octave is an octave
/// wherever it falls.
const EQ_FREQ_LOW: f32 = 20.;
const EQ_FREQ_HIGH: f32 = 20_000.;

/// The curve box. Tall enough that 1 dB is a visible move at the ±12 dB axis,
/// and short enough that the card still fits a 360 px window.
const EQ_GRAPH_H: f32 = 132.;

/// How wide the equalizer card is allowed to get. It is the one card that is a
/// *graph*: every pixel across is frequency resolution, and at the 320 px the
/// other cards use, a third of an octave was a couple of pixels. Past this the
/// curve stops gaining anything and the card starts reading as a wall.
const EQ_W_MAX: f32 = 720.;

/// The gap the card leaves either side of it, so it reads as a card on a scrim
/// rather than as a second window: it takes the width it can get inside that.
const EQ_W_MARGIN: f32 = 32.;

/// How many bands one clip's equalizer may carry from this card. Ten because
/// the keyboard picks a band with a digit and a keyboard has ten of them --
/// past that a band would be reachable by pointer only. The engine itself caps
/// nothing (`EqParams::bands` is a plain `Vec`), so a file may still carry more
/// and this card will draw and edit every one it finds.
const EQ_BANDS_MAX: usize = 10;

/// One press of the frequency keys, as a factor: a sixth of an octave, so a
/// band walks the whole axis in about sixty presses and still lands close
/// enough to a named frequency to aim at one.
const EQ_FREQ_STEP: f32 = 1.122_462;

/// One press of the Q keys, as a factor, and the range they move in. Below the
/// bottom a peak is barely a peak any more; above the top it is a whistle on
/// one frequency. 0.707 -- the flat-shelf value, and the default -- sits inside
/// them, so nothing a file carries has to be dragged into range first.
const EQ_Q_STEP: f32 = 1.25;
const EQ_Q_LOW: f32 = 0.3;
const EQ_Q_HIGH: f32 = 12.;

/// How many points the curve is drawn from. One per ~3 px across the card:
/// past that the line is smooth and the extra biquad evaluations are wasted.
const EQ_CURVE_STEPS: usize = 96;

/// A band's handle on the curve. Only the dot -- what is *grabbed* is the whole
/// graph (the nearest band along the frequency axis), so the target is the box.
const EQ_HANDLE: f32 = 10.;

/// The frequencies the graph names, so the curve can be read as a curve *of
/// something*. The two ends label themselves at the edges.
const EQ_TICKS: [(f32, &str); 5] = [
    (20., "20 Hz"),
    (100., "100"),
    (1000., "1k"),
    (10000., "10k"),
    (20000., "20k"),
];

/// The gains the graph rules a line across, besides the 0 dB one it already
/// had: half way to each limit, so a boost can be read as "about six" without
/// counting pixels. The limits themselves are the box's own edges and are
/// named at the corners instead.
const EQ_DB_GRID: [f32; 2] = [6., -6.];


/// How many played samples one spectrum frame is transformed from. A power of
/// two ([`fft`] is radix-2) and the whole of the engine's tap: 1024 at 48 kHz
/// is a 47 Hz bin, fine enough that the bass end is a shape and short enough
/// (21 ms) that the analyser moves with the music.
const EQ_FFT: usize = 1024;

/// The level range the analyser is drawn across, floor to ceiling in dBFS: the
/// bottom of the box is silence and the top is a bin at -12 dBFS, which is
/// about where a mixed track's loudest band sits. A look, not a measurement --
/// the numbers on the axis are the curve's dB, never the analyser's.
const EQ_SPECTRUM_DB: (f32, f32) = (-96., -12.);




/// The colour card's four controls, in the order it lists them: what each is
/// called and the range it moves in. The order is `ColorParams`' own, which is
/// what [`color_band`] indexes.
const COLOR_BANDS: [(&str, f32, f32); 4] = [
    ("Brightness", -1., 1.),
    ("Contrast", 0., 2.),
    ("Saturation", 0., 2.),
    ("Tint (cool–warm)", -1., 1.),
];

/// A press of a nudge key: a fortieth of a band's range, so a slider crosses it
/// in forty presses and every stop is a number the file can write. A drag lands
/// on the same grid ([`Player::drag_color`]), so the pointer and the keyboard
/// cannot reach two different sets of values.
const COLOR_STEP: f32 = 0.05;

/// The card's width: a slider row is a label, the bar and the value, and the
/// longest label has to fit beside all three without truncating (measured
/// against "Tint (cool–warm)").
const COLOR_W: f32 = 460.;
/// How much of a slider row the bar itself gets -- what a drag is read against,
/// so it takes the width the two nudge buttons used to.
const COLOR_BAR_W: f32 = 240.;

/// The histogram's bins per channel. 64 is four codes of an 8-bit ramp per bin:
/// fine enough to see a grade tilt, coarse enough that a subsampled count is
/// not noise.
const HIST_BINS: usize = 64;

/// How many pixels of a frame the histogram reads. The stride is
/// `pixels / HIST_SAMPLES` (1920x1080 -> every 253rd pixel), which is a
/// thousandth of the frame and walks across columns rather than down one.
const HIST_SAMPLES: usize = 8_192;

/// The histogram box. Shorter than the equalizer's curve because four slider
/// rows stand under it and the card still has to fit a 360 px window.
const HIST_H: f32 = 96.;


/// Where the transport is. The one answer the button's glyph, its label, its
/// enablement, the play key and the repaint loop all read -- there is no play
/// flag anywhere else, because a second one is a second answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Transport {
    /// No timeline open. Nothing to play and the transport is dimmed.
    Stopped,
    Playing,
    Paused,
    /// Played out: the last frame is on screen, the decoder is finished, and the
    /// clock is still running past it -- which is exactly why "is the clock
    /// going" is not the same question as "is this playing".
    Ended,
}

impl Transport {
    /// The timeline is in motion: two bars on the button, and a repaint owed
    /// every vsync.
    fn is_playing(self) -> bool {
        matches!(self, Transport::Playing)
    }

    /// The play key and the transport button start over from the top rather
    /// than toggling -- the end of a timeline is where every NLE does this, and
    /// the button does it because the key already did.
    fn restarts(self) -> bool {
        matches!(self, Transport::Ended)
    }
}

/// What a session's own two answers mean: the clock, unless the timeline has
/// been played out -- `played_out` is the engine's end of stream with no frame
/// still waiting on the pump, and it wins, because past the end a running clock
/// is measuring wall time and not a picture.
fn transport(playing: bool, played_out: bool) -> Transport {
    match (played_out, playing) {
        (true, _) => Transport::Ended,
        (_, true) => Transport::Playing,
        (_, false) => Transport::Paused,
    }
}

struct Player {
    /// The timeline, once there is one. A run with no file opens without it and
    /// waits: the first media import or project load is what fills it, and
    /// until then every action that needs a timeline says so instead of acting.
    session: Option<PlaybackSession>,
    /// Timeline seconds -> frame index, so the clock can be compared to what
    /// the decoder hands over.
    fps: f64,
    name: SharedString,
    image: Option<Arc<RenderImage>>,
    /// The picture of the bitmap cue on screen, and which cue that is: the
    /// track it is a row of and where it starts on the timeline. A PGS display
    /// set is run-length and has to be walked into a canvas-sized buffer to be
    /// drawn ([`engine::subtitle::CueImage::rgba`]), which is a thing to do
    /// when the cue changes -- every few seconds -- and not at every repaint.
    ///
    /// The *track* is half the key because the four PGS tracks of a remux are
    /// one film's subtitles in four languages: they start at the same
    /// microsecond, and a picked row that only changed the language would go on
    /// showing the one before it.
    sub_image: Option<((usize, i64), Arc<RenderImage>)>,
    /// A frame that arrived before its time; shown on the tick it comes due.
    /// The pump's buffer, not transport state -- but a frame waiting here is
    /// what keeps a finished decoder from reading as [`Transport::Ended`] one
    /// tick early. See [`Player::transport`].
    held: Option<Frame>,
    /// A seek is waiting for its frame, and since when. Keeps the repaint loop
    /// alive while paused, which is the only way the new still ever reaches the
    /// screen; the instant is what a drag's samples are gated on
    /// ([`Player::flush_drag`]) and what says so in words when one open stands
    /// too long ([`seek_line`]).
    seek_since: Option<Instant>,
    /// The ruler's own box, recorded at prepaint: a mouse listener is handed
    /// the window position and nothing else.
    ruler: Rc<Cell<Bounds<Pixels>>>,
    /// How wide a second of timeline is drawn, and from which moment. Held here
    /// and nowhere else: every frame-to-pixel answer in the panel comes out of
    /// it, so the boxes, the playhead and the pointer cannot disagree.
    scale: Scale,
    /// A hand has moved the view itself -- a wheel scroll, a zoom about the
    /// pointer -- and until the playhead runs back into what it chose to look
    /// at, the view is the hand's and the follow keeps off it. Without this a
    /// notch during playback was undone by the very next frame: the follow
    /// centres a playhead that has left the bed, and a scroll away from it is
    /// exactly a playhead leaving the bed. Given back by [`Render`] the moment
    /// the head is on screen again, and by every transport ask (a seek, a
    /// play/pause) outright -- those are a person saying where to look.
    panned: bool,
    /// Which clip the edit keys act on: the lane it is in and its index there.
    /// The *clicked* half, not the group -- a group is what gets marked on
    /// screen, but Lift has to know which half it was aimed at. Indices move
    /// under every edit, so this is cleared by all of them.
    selected: Option<(Lane, usize)>,
    /// The clip menu a right-click opened, if one is up. Holds an index like
    /// `selected` does, so it is closed by anything that can move indices --
    /// every stroke, and every item of its own.
    context_menu: Option<ContextMenu>,
    /// The choice list a click on an enumerated setting opened, if one is up:
    /// the project resolution or a clip's fit policy, every value on screen at
    /// once. Closed by anything that closes the menus, and for the same reason.
    picker: Option<Picker>,
    /// The library menu a right-click on a row opened, if one is up. Names its
    /// row by file and stream rather than by position, so it acts on the row it
    /// was opened on however the list is rebuilt under it.
    library_menu: Option<LibraryMenu>,
    /// Which library row is picked: the file and the audio stream that row
    /// names, which is what an insert needs and what survives a row list being
    /// rebuilt. Its own selection and not the timeline's: Delete keeps acting
    /// on the clip that was clicked in a lane, whatever the library is showing.
    selected_asset: Option<(PathBuf, usize)>,
    /// Which category of the library is being looked at. A tab and not a
    /// filter box: the categories are what the media *is*, and every editor
    /// this one is measured against splits its pool the same way.
    library_tab: LibraryTab,
    /// What is known about each source's audio, taken once and kept. Keyed on
    /// the path *and stream* -- two streams of one file are two envelopes -- and
    /// the key is inserted the moment the decode is *started*: presence means
    /// "asked", so a repaint mid-decode cannot ask again.
    waves: HashMap<(PathBuf, usize), Wave>,
    /// Which audio streams each imported file has, as its header describes
    /// them: one library row per entry. Keyed and filled like `waves` --
    /// presence means "asked" -- and an empty list is a silent file, which is
    /// exactly one row and no stream tags.
    streams: HashMap<PathBuf, Vec<StreamInfo>>,
    /// What each source is coded at, read off its header once and kept: what a
    /// properties card says about a file's rate. Filled like `streams` --
    /// presence means "asked" -- and the inner `None` is "asked, not answered
    /// yet", which the card draws as an ellipsis: the probe walks a Matroska's
    /// clusters, so a big film answers in seconds rather than at once.
    bitrates: HashMap<PathBuf, Option<MediaBitrate>>,
    /// How big each still source's picture is, read from its header once and
    /// kept -- what a library row and its card say about a file that has no
    /// streams to describe. Filled like `streams`: presence means "asked", and
    /// `None` is a file with no picture to report (every source that is not an
    /// image, and one whose header would not read).
    sizes: HashMap<PathBuf, Option<(u32, u32)>>,
    /// Every frame of each source a decoder may be started from -- its sync
    /// points ([`engine::demux::sync_points`]), which are the frames a cut may
    /// be placed on for an export to *copy* the film instead of coding all of
    /// it again. Filled like `bitrates` and off the render thread for the same
    /// reason, and more so: the answer is that Matroska cluster walk. An empty
    /// list is a source with no grid to offer (an mp4, a still, a song), which
    /// must stay in the map or every repaint would ask again.
    syncs: HashMap<PathBuf, Vec<u32>>,
    /// Which decoder each source will run on, probed once at import and kept:
    /// the codec (`None` for a still) and the seat the engine picked for it.
    /// What a library row says *before* anything plays; the running answer is
    /// the session's own (`PlaybackSession::decode_backend`), which follows a
    /// fallback this cannot. Filled like `sizes`: presence means "asked", and
    /// `None` is a source with no decoder to name -- a song, or one the probe
    /// refused -- which must stay in the map or every repaint would ask again.
    decoders: HashMap<PathBuf, Option<(Option<Codec>, Backend)>>,
    /// What an export of the picked settings would open -- the picture's seat
    /// or the copy that means it opens none, and the sound's -- and what it was
    /// asked about: the settings, the canvas and the *cuts*, since where they
    /// land is what decides whether the picture is copied at all. The probe
    /// opens a real VA-API encoder (~100 ms) and reads every source's header,
    /// so it runs off the render thread and only while the export card is up.
    /// The inner `None` is "asked, not answered yet".
    export_seat: Option<(
        ExportSettings,
        (u32, u32),
        Vec<Clip>,
        Option<(Option<&'static str>, &'static str)>,
    )>,
    /// What this machine's GPU decodes and encodes, as the plugin answered it:
    /// asked once, off the render thread like `export_seat` and for the same
    /// reason (a VA-API init), and kept for the life of the process because the
    /// answer cannot change while we run. `None` is "not asked yet".
    hw_caps: Option<SharedString>,
    /// The copied clip. Frame ranges only, so it survives the clip it was taken
    /// from being deleted -- and it outlives the selection.
    clipboard: Option<Clip>,
    /// A drag that started on the ruler. Moves anywhere in the window scrub
    /// while it is set; the release commits the exact position.
    scrubbing: bool,
    /// A drag that started on a clip's edge, tracked on the root for
    /// `scrubbing`'s reason: a 6 px strip is not where the pointer stays. See
    /// [`Trim`].
    trim: Option<Trim>,
    /// How far into the clip the last press on a box landed, in timeline
    /// frames: what a drag lets go of is the *point that was grabbed*, so the
    /// head lands that much in front of the pointer and the clip does not jump
    /// under the hand. Recorded at the press because gpui hands the drop only
    /// the value being dragged, and stale between drags, which costs nothing --
    /// no drag starts without a press on the box it moves.
    grab: u32,
    /// Whether a drag or a trim is pulled onto the edges near it. On by
    /// default, because clips meeting exactly is what a timeline is for, and off
    /// by one stroke ([`ActionId::ToggleSnap`]) for the frame-by-frame placement
    /// no magnet may take away.
    snap: bool,
    /// Whether the cue under the playhead is drawn over the picture. On by
    /// default -- a subtitle imported and then invisible is an import nobody can
    /// tell happened -- and off by one stroke
    /// ([`ActionId::ToggleSubtitles`](keymap::ActionId::ToggleSubtitles)) for
    /// anyone watching the picture rather than reading it. The player's, not the
    /// project's: it changes nothing that is saved and nothing that is exported.
    subs_on: bool,
    /// Which subtitle track is the one on screen: an index into
    /// [`PlaybackSession::subtitles`], since a file may carry several and only
    /// one can be read at a time. Cleared with the timeline like every other
    /// index here -- track 2 of one project is not track 2 of the next.
    sub_track: usize,
    /// The frame the live gesture is about to land on, or `None` while it is
    /// over open bed: the line every lane draws so the snap is seen before it
    /// happens rather than discovered after the release. Stale between gestures,
    /// which costs nothing -- it is drawn only while one is live.
    snap_cue: Option<u32>,
    /// The box the same gesture is about to fill, or `None` while the pointer is
    /// over no lane: the shadow every proper editor draws under a drag. Set by
    /// the lane the pointer is actually over -- that is the one question the
    /// line above does not answer -- and drawn only while a drag is live, for
    /// [`Player::snap_cue`]'s reason.
    ghost: Option<Ghost>,
    /// The slot a track header being dragged is about to drop into, or `None`
    /// while the pointer is over no lane: the line drawn between two headers,
    /// for the reason [`Player::ghost`] draws a shadow -- where a gesture lands
    /// is seen before the release. Drawn only while a drag is live.
    lane_drop: Option<LaneDrop>,
    last_scrub: Instant,
    last_target: u32,
    /// The running export. While it owns the UI the editor is read-only.
    export: Option<ExportHandle>,
    /// The export above was cancelled and is only winding down. The editor is
    /// already free -- the worker took its own copy of the edit list -- but the
    /// handle is held until the worker settles, because its last act is to
    /// delete the output file and a second export must not be what it deletes.
    cancelling: bool,
    /// When the running export started, and how far it had come at each sample
    /// since, as `(elapsed, progress)` marks. The elapsed clock and the
    /// rolling-window estimate the progress line reads; see [`note_progress`].
    export_started: Option<Instant>,
    export_marks: Vec<(f32, f32)>,
    /// The file an import worker is reading right now, and the files waiting
    /// behind it in arrival order. Unlike an export, an import owns nothing:
    /// the editor stays live, the timeline keeps playing, and the only thing
    /// this holds is the line above the panel ([`Player::import_bar`]).
    importing: Option<Import>,
    imports: std::collections::VecDeque<PathBuf>,
    /// The file argv named, until its read lands. It goes through the queue
    /// above like any other file -- that is what puts the window on screen
    /// before a byte of a 25 GB film is read -- and this is what tells
    /// [`Player::take_import`] that this one is an *open* and not an import:
    /// it becomes the timeline, and the clock, the title and the export path
    /// come from it. Cleared the moment it lands, so a later drop of the very
    /// same path is an import like any other.
    opening: Option<PathBuf>,
    /// Where an export writes. Built once from the source path, which is not
    /// otherwise kept.
    export_path: PathBuf,
    /// Where the save action writes: the project this timeline was loaded from,
    /// or the one derived beside the media it started as. Saving twice
    /// overwrites the same file rather than making a second one.
    project_path: PathBuf,
    /// Which stroke means what, and what every shortcut on screen is called.
    /// The one place either question is answered.
    keymap: Keymap,
    /// How loud the monitoring is, and whether it is muted. Lives here rather
    /// than in the session so it survives closing one file and opening the
    /// next -- it is a setting of the player, not of the timeline, which is
    /// also why it is not written to the project file and cannot reach an
    /// export. [`Player::apply_volume`] is what pushes it at a session.
    volume: Volume,
    /// Where the volume slider was last painted, and whether a hand is on it --
    /// the speed bar's pair, for the speed bar's reason: the pointer moves
    /// arrive at the root, so the bar's own geometry has to be readable there.
    volume_bar: Rc<Cell<Bounds<Pixels>>>,
    volume_dragging: bool,
    /// The keybindings overlay is up. While it is, it owns the keyboard and the
    /// pointer: a stroke or a click meant for a row must not also cut the
    /// timeline.
    keys_open: bool,
    /// What has been typed into the card's search box, which is the card's own
    /// input exactly as the export card's digits are (nothing in it takes
    /// focus, so the root's key handler is the field). Emptied every time the
    /// card opens: a search is a look at the list, not a setting.
    keys_search: String,
    /// Where that list is scrolled to. Held here rather than left to the
    /// wheel alone: forty actions are four times what a 360 px window shows,
    /// and the rows past the fold have to be reachable from the keyboard that
    /// is already typing in the search box.
    keys_scroll: ScrollHandle,
    /// Where the lane column and the inspector's rows have been taken to. Read
    /// back at render, not only written by the wheel: the line each of them
    /// carries about what is below the fold is a count of what is *still* below
    /// it, and a scroll that nothing reads is a scroll the affordance cannot
    /// follow.
    lanes_scroll: ScrollHandle,
    inspector_scroll: ScrollHandle,
    /// And where the equalizer card's own body has been taken to. It is the
    /// tallest card in the column -- a graph with a row of numbers and a row of
    /// buttons under it -- and at the 360 px floor its title and its buttons
    /// were off both ends of the column with no way to reach them.
    eq_scroll: ScrollHandle,
    /// The export options card is up: what the export action opens now, so
    /// nothing is written until the card's own button says so. One card at a
    /// time -- opening either closes the other, since both are the whole window
    /// and two stacked scrims say nothing about which one is listening.
    export_open: bool,
    /// How the card lays its rows out, and where the formats this program
    /// cannot write are said. Two shapes of the same card, kept behind `g` and
    /// `r` so the choice between them can be made by looking at both rather
    /// than by argument: sections with headers against one flat list, and a
    /// collapsed "cannot write" footer against a dimmed row each. The defaults
    /// are grouped and collapsed -- the five dead rows used to eat the fold.
    /// Not persisted: this is a look, not a setting.
    export_grouped: bool,
    export_refusals_inline: bool,
    /// Which quality row the card has picked, and the megabits typed against
    /// the custom one. Kept across closes, so a second export offers what the
    /// first one chose.
    quality: Quality,
    custom_mbps: u32,
    /// The custom row's number *while it is being typed*, or `None` when nobody
    /// is typing one. A field with a caret in it and not a key capture: digits
    /// used to change the bitrate from anywhere in the card, with no caret to
    /// say where they were landing and nothing to look at before the number
    /// took effect. Nothing in this card takes gpui focus (the root keeps the
    /// keyboard), so the field is a modal state on the player exactly as a
    /// waiting rebind row is, and the root's handler is what types into it.
    mbps_edit: Option<NumberEdit>,
    /// What the *sound* is coded at, in kbps, for every format that encodes it
    /// -- the AAC inside a video export as much as an MP3. Kept across closes
    /// like the picture's quality, and starts at the figure this program wrote
    /// before the row existed ([`engine::export::DEFAULT_AUDIO_KBPS`]), so a
    /// user who never touches the row gets the file they always got.
    audio_kbps: u32,
    /// Which file the card will write. Kept across closes like the quality, and
    /// what [`Player::export_path`](Player) is named after.
    format: Format,
    /// The equalizer card is up on this clip -- the lane and index it was
    /// opened on. Held rather than re-read from `selected` every paint because
    /// the card is modal: while it is up nothing else can move an index, and
    /// the one edit it makes (`set_eq`) moves none.
    eq_open: Option<(Lane, usize)>,
    /// The curve the card is showing, which is the clip's own or the flat
    /// five-band default. Edited live and written at the clip once per gesture
    /// ([`Player::commit_eq`]): the project's equalizer table is append-only, so
    /// a write per pointer sample would be a table entry -- and an undo step --
    /// per pixel.
    eq_params: EqParams,
    /// Which band the keyboard moves, and which one a drag is holding.
    eq_band: usize,
    /// A handle on the curve is being dragged. Tracked on the root like
    /// `scrubbing`, for the same reason: a hand pulling a band to +12 dB runs
    /// off the top of the graph long before it lets go.
    eq_dragging: bool,
    /// The curve box, recorded at prepaint: gpui hands a mouse listener the
    /// window position only, so this is what a press and a drag are read
    /// against ([`frac_along`], [`frac_down`]).
    eq_graph: Rc<Cell<Bounds<Pixels>>>,
    /// Whether the analyser is drawn behind the curve. On by default -- what
    /// the curve is being *shaped against* is the point of drawing it -- and
    /// off with one press for anyone who would rather read the curve alone.
    /// Card state, not the project's: it changes nothing that plays.
    eq_spectrum: bool,
    /// The colour card is up on this clip -- the lane it is on and its index
    /// there. `None` when it is closed, which is the only place that state
    /// lives: the grade itself is the project's.
    color_open: Option<(Lane, usize)>,
    /// The speed card is up on this clip -- the lane it is on and its index
    /// there, exactly as the colour card's handle is. `None` when it is closed;
    /// the rate itself is the project's, so there is nothing else to hold.
    speed_open: Option<(Lane, usize)>,
    /// The speed bar's box, recorded at prepaint: a mouse listener is handed the
    /// window position only, so this is what a press and a drag are read against
    /// ([`frac_along`]).
    speed_bar: Rc<Cell<Bounds<Pixels>>>,
    /// The bar is being dragged. On the root like the colour card's, for the
    /// same reason: the pointer leaves a 4 px bar on the first move.
    speed_dragging: bool,
    /// The rate the hand is on, held back because the worker still owes a frame
    /// ([`Player::flush_drag`]). What the bar draws while it stands, so the
    /// handle stays under the hand even though the picture has not caught up.
    pending_speed: Option<Speed>,
    /// The mix card is up: every audio track's own volume and the master
    /// limiter, which are project settings and not any clip's -- so unlike the
    /// four clip cards there is no handle to hold, only whether it is open.
    mix_open: bool,
    /// Which of its rows the arrow keys move -- a fader, the limiter's ceiling
    /// or its switch. The card's own focus, since nothing in it takes gpui's
    /// (ledger:182).
    mix_field: usize,
    /// The silence card is up on this clip -- the lane it is on and its index
    /// there, exactly as the speed card's handle is.
    silence_open: Option<(Lane, usize)>,
    /// What a scan is told to look for, and how fast the speed-up button plays
    /// what it found. Kept across closes like the export card's quality: a
    /// second run offers what the first one settled on.
    silence: engine::silence::Settings,
    silence_factor: Speed,
    /// How wide the apply reaches ([`Scope`]). Kept across closes for the same
    /// reason, and never *widened* on anyone's behalf: the whole point of it is
    /// that a track nobody named does not move.
    silence_scope: Scope,
    /// Which of the card's [`SILENCE_ROWS`] the arrow keys move. The card's own
    /// focus, since nothing in it takes gpui's (ledger:182).
    silence_field: usize,
    /// Whether the threshold is *labelled* dBFS or dB. Display only, and the
    /// number is the same either way: the setting is a level below full scale,
    /// so 0 is the loudest sample a file can hold and -40 is forty decibels
    /// under it -- "dBFS" names that reference out loud, "dB" leaves it unsaid.
    /// No conversion is hiding behind the row (there is no reference here worth
    /// inventing, and a made-up SPL would be a lie about what was measured);
    /// what it changes is which of the two spellings a person reads.
    silence_dbfs: bool,
    /// What the last scan found, in timeline frames: what the lane draws marks
    /// over and what an apply acts on -- *exactly* the previewed set, never a
    /// second scan at the moment of the press.
    silence_marks: Vec<(u32, u32)>,
    /// The levels of every stretch scanned this session, kept so moving a
    /// threshold is arithmetic rather than another decode. Keyed by
    /// [`ScanKey`], and not one entry: two films on one timeline would
    /// otherwise evict each other, and the decode being paid twice is the fifty
    /// seconds this card exists to not spend.
    silence_levels: HashMap<ScanKey, Arc<Vec<f32>>>,
    /// The scan a worker is running for the card, if one is. `None` means the
    /// card is drawing numbers it already has.
    silence_scan: Option<SilenceScan>,
    /// Which of the card's four sliders the arrow keys and a drag move. The
    /// card's own focus, since nothing in it takes gpui's (ledger:182).
    color_band: usize,
    /// A slider is being dragged. Tracked on the root like `scrubbing` and the
    /// equalizer's drag, for the same reason: a 4 px bar is left by the pointer
    /// on the first move and its own listeners then stop firing.
    color_dragging: bool,
    /// Each slider's box, recorded at prepaint: a mouse listener is handed the
    /// window position only, so this is what a press and a drag are read against
    /// ([`frac_along`]). One per band, because the press picks the row it landed
    /// on and the drag then belongs to that row's range.
    color_bars: [Rc<Cell<Bounds<Pixels>>>; COLOR_BANDS.len()],
    /// The grade the hand is on, held back because the worker still owes a
    /// frame ([`Player::flush_drag`]): a live write into a busy worker only
    /// cancels the open the picture is already waiting for, so a bar-wide sweep
    /// would pay for forty of them and show one. What the sliders draw while it
    /// stands, and never lost -- the frame that lands writes it, and so does the
    /// release.
    pending_color: Option<ColorParams>,
    /// The frame on screen counted into `HIST_BINS` bins per channel -- the
    /// *graded* frame, because the grade is applied in the decode worker and
    /// what arrives here is already through it. Refilled by every pumped frame,
    /// which is what makes the colour card's graph move as a slider is dragged:
    /// each live write reseeks, and the reseek's frame is the next count.
    histogram: [[u32; HIST_BINS]; 3],
    /// The action whose row is waiting for a stroke. The next key that is
    /// neither escape nor a lone modifier becomes the whole of what reaches it.
    rebinding: Option<ActionId>,
    /// What the file actions have had to say, oldest first. A *queue* and not a
    /// slot: two imports that fail back to back used to be one message, because
    /// the second overwrote the first before a frame had drawn it -- the failure
    /// a user never learns about is the one that was answered by another
    /// failure. The front holds its own bar above the panel until it is answered
    /// -- any key retires it, so does a click on it -- the bar says how many are
    /// behind it, and answering it brings the next one up.
    notices: std::collections::VecDeque<SharedString>,
    /// What the last finished export wrote, so the notice can be the way to it.
    /// Only the [`EXPORT_DONE`] line reads it -- any later notice has replaced
    /// that text -- so a click never opens a file the bar is not naming.
    exported: Option<PathBuf>,
    /// What the compositor was last told this window is called. Setting a title
    /// is a protocol round trip and a repaint is sixty a second, so the title is
    /// pushed only when it is not this any more.
    titled: String,
    displayed: u32,
    dropped: u32,
    /// When the picture was last restarted at the clock for falling behind it
    /// ([`should_resync`]). The cool-down's only state.
    resynced: Option<Instant>,
    /// Wall clock of the first displayed frame -- the real-speed measurement.
    started: Option<Instant>,
    focus: FocusHandle,
}

impl Player {
    /// Catches the display up to the clock: everything already due is taken off
    /// the channel and only the last of them is shown, which *is* the
    /// drop-when-behind policy. A frame that is not due yet waits in `held`, and
    /// while the clock is paused *nothing* is due -- a repaint re-presents the
    /// frame already on screen, whatever asked for the repaint.
    fn pump(&mut self, window: &mut Window) {
        // Where the transport was before this drain, so the crossing into
        // `Ended` can be recognised as the one transition it is.
        let was = self.transport();
        // No timeline, nothing to catch up to: the window is showing its empty
        // state and there is no decoder to drain.
        let Some(session) = &mut self.session else {
            return;
        };
        let target = session.now() * self.fps;
        let mut newest: Option<Frame> = None;
        // A frame the screen is owed: a seek's landing, and the one readiness
        // signal there is ([`Player::reset_after_reseek`]).
        let owed = self.seek_since.is_some();
        // Paused, the clock is frozen and *nothing new is due*. Whatever the
        // decoder is still handing over is the backlog it was behind by when
        // the pause landed -- frames at a position the transport has already
        // left -- and taking one per repaint is what walked the picture on
        // after the sound had stopped, at exactly the rate the pointer was
        // moved over the timeline. Gated here, at the one place a frame ever
        // reaches the screen, rather than in the handlers that repaint: a
        // hover, a notice, a resize and a vsync are all the same event to this.
        // An owed frame is still taken, playing or not -- a scrub is paused by
        // definition, and its landing is the whole point of it.
        while session.is_playing() || owed {
            let frame = match self.held.take() {
                Some(frame) => frame,
                // Nothing waiting means either a clip boundary being rebuilt or
                // the real end of the timeline, and only the engine can tell
                // them apart -- `frame.index` is already a timeline index.
                None => match session.try_frame() {
                    Some(frame) => frame,
                    None => break,
                },
            };
            if f64::from(frame.index) <= target {
                self.dropped += u32::from(newest.is_some());
                newest = Some(frame);
            } else {
                self.held = Some(frame);
                break;
            }
        }

        // How far behind the master clock the picture just handed over is, in
        // seconds. Measured off a frame that really arrived and nothing else: a
        // clip boundary being reopened delivers nothing at all for hundreds of
        // milliseconds, and restarting *that* would only cancel the open it is
        // waiting on.
        let late = newest
            .as_ref()
            .map_or(0., |f| (target - f64::from(f.index)) / self.fps);

        if let Some(frame) = newest {
            self.displayed += 1;
            self.seek_since = None;
            self.started.get_or_insert_with(|| {
                eprintln!("first frame displayed (index {})", frame.index);
                Instant::now()
            });
            let buf = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
                .expect("frame buffer sized width*height*4");
            // Counted here rather than under `color_open`, because the card
            // opens on a frame that was pumped before it: gating this on the
            // card would leave its graph flat until something reseeked. A
            // thousandth of the pixels, against a conversion that just touched
            // all of them.
            self.histogram = histogram(buf.as_raw());
            let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
            if let Some(old) = self.image.replace(next) {
                // Every RenderImage gets a fresh id and its own atlas tile:
                // without this the sprite atlas grows for the whole video.
                let _ = window.drop_image(old);
            }
        }

        // Audio is the master clock and a decoder that cannot keep up with it
        // never gets back on its own: it hands over every frame in order,
        // whether or not its moment has passed, so what it is behind by only
        // grows -- a minute in, the picture is seconds behind what is being
        // heard, and that is the whole of "the video can't catch the audio".
        // Past `LATE_RESYNC` the backlog is abandoned and the picture restarted
        // at the clock, which touches neither the sound nor the clock
        // (`PlaybackSession::resync_picture`), so nothing the ear is following
        // moves. Never on a frame a seek owed: that one is late by however long
        // its own reopen took, and answering it with another reopen is a loop.
        //
        // corner-cut: on a machine that cannot decode the file in real time at
        // all this settles into one restart per `RESYNC_GAP` -- in sync, and
        // stuttering, which is the honest picture of what that machine can do.
        // The upgrade path is dropping late frames *inside* the worker (skip
        // the convert and the send for anything already past due), which needs
        // the deadline shared with it.
        if !owed && session.is_playing() && should_resync(late, self.resynced) {
            eprintln!("picture {late:.3}s behind the clock: restarting it there");
            session.resync_picture();
            self.held = None;
            self.resynced = Some(Instant::now());
        }

        if self.transport() == Transport::Ended {
            // A seek whose worker never produced a frame (vanished file) would
            // otherwise repaint at vsync forever. Held clear for as long as the
            // state does, not just on the crossing: nothing else is coming.
            self.seek_since = None;
            if was != Transport::Ended {
                // Ended is a *stopped* transport, so the clock stops with it,
                // on the out point the timecode and the playhead have been
                // showing all along. Nothing else ever stopped it: past the
                // last frame wall time takes over and `now()` walks off the end
                // of the timeline for as long as the window is left open -- and
                // the playhead is what a cut, a paste, an insert and the
                // analyser all act at, so every one of them was aiming into
                // empty space (measured: a 5 s timeline recognised its end at
                // clock 17.5 s under a slow renderer). End of stream is left
                // set, so this is still `Ended` and the next press restarts.
                if let Some(session) = &mut self.session {
                    session.halt_at_end();
                }
                let elapsed = self.started.map_or(0.0, |t| t.elapsed().as_secs_f64());
                eprintln!(
                    "eof after {elapsed:.3}s wall: {} frames displayed, {} dropped, clock {:.3}s",
                    self.displayed,
                    self.dropped,
                    self.session.as_ref().map_or(0., PlaybackSession::now)
                );
            }
        }
    }

    /// Where the transport is, asked of the session rather than remembered:
    /// end of stream is the engine's own flag (any seek clears it, which is why
    /// an edit past the end revives the picture) and so is the clock. A held
    /// frame is one still owed to the screen, so the end is not the end yet.
    fn transport(&self) -> Transport {
        let Some(session) = &self.session else {
            return Transport::Stopped;
        };
        transport(
            session.is_playing(),
            session.is_eos() && self.held.is_none(),
        )
    }

    /// A frame owed to the screen after a reseek, and the buffered one dropped:
    /// what stops the picture from staying frozen on the old last frame. The
    /// end-of-stream flag itself is the engine's and its own seek clears it --
    /// edits reseek inside the engine and still owe this.
    fn reset_after_reseek(&mut self) {
        self.held = None;
        // Restarted on every reseek, not only on the first: what it measures is
        // the open now standing, which is what a person is waiting on.
        self.seek_since = Some(Instant::now());
        // A seek is a person saying where to look, so it takes the view back
        // from an earlier scroll: the frame asked for is the one to be shown.
        self.panned = false;
        // An edit moves the indices a drag in flight is holding -- a stroke
        // during one is exactly that -- and an edge committed against a moved
        // index would trim a clip nobody grabbed. Dropping it is the whole fix:
        // nothing has been written yet.
        self.trim = None;
        // ...and the shadow a drag is drawn under promises a landing on a lane
        // this edit has just reshaped. The next move of the drag draws it
        // again; until then it says nothing.
        self.ghost = None;
    }

    /// What an action does, wherever it was asked for -- a stroke, or the clip
    /// menu item that names the same action. One table, so the two can never
    /// come to mean different things.
    fn act(&mut self, action: ActionId, cx: &mut Context<Self>) {
        // Two doors, one oracle. This used to be the asymmetry the whole
        // toolbar was built on: the buttons dimmed themselves off
        // [`enable`] while the keyboard walked straight past it, so with no
        // file open `s` toggled the snap and `v` added a track while the very
        // same controls sat dim and *dead* to the pointer. Whatever refuses the
        // button refuses the key, in the oracle's own words -- and a refusal
        // that is silent from the keyboard is a bug the same size.
        match self.enable(action, None) {
            Enable::Yes => {}
            // A state refusal is spoken: the thing exists and cannot happen
            // *now*, which is exactly what a silent key press fails to say.
            Enable::No(why) => {
                self.notify_user(format!("{} — {why}", action.label()).into());
                cx.notify();
                return;
            }
            // A class refusal is not: the action does not exist for what is in
            // front of the user, and `esc` with nothing exporting must not
            // answer with a line about exports.
            Enable::Hidden(_) => return,
        }
        match action {
            ActionId::Play => self.toggle_or_restart(cx),
            ActionId::StepBack => self.step(-1, cx),
            ActionId::StepForward => self.step(1, cx),
            // A second is however many frames this timeline runs at.
            ActionId::JumpBack => self.step(-(self.fps.round() as i64), cx),
            ActionId::JumpForward => self.step(self.fps.round() as i64, cx),
            // The ends, as a step nothing can be far enough from.
            ActionId::GoStart => self.step(i64::MIN, cx),
            ActionId::GoEnd => self.step(i64::MAX, cx),
            // Not a step at all: the grid these land on is the *source's*, and
            // where the next one is depends on the file rather than on the rate.
            ActionId::PrevSyncPoint => self.jump_sync(false, cx),
            ActionId::NextSyncPoint => self.jump_sync(true, cx),
            ActionId::Export => self.open_export(cx),
            ActionId::Save => self.save_project(cx),
            ActionId::Copy => self.copy_selected(),
            ActionId::Paste => self.paste(cx),
            ActionId::Cut => self.cut(cx),
            ActionId::Regroup => self.regroup(cx),
            ActionId::Detach => self.detach(cx),
            ActionId::Group => self.group(cx),
            ActionId::Select => self.select_under_playhead(cx),
            ActionId::SelectNext => self.select_step(true, cx),
            ActionId::SelectPrev => self.select_step(false, cx),
            ActionId::Delete => self.delete_selected(cx),
            ActionId::Lift => self.lift_selected(cx),
            ActionId::Color => self.open_color(cx),
            ActionId::Fit => self.cycle_fit(cx),
            ActionId::Resolution => self.cycle_resolution(cx),
            // The playhead is what a key zoom is aimed at: it is the one place
            // on the timeline the user is certainly looking at, and keeping it
            // still is what every editor does.
            ActionId::ZoomIn => self.zoom(ZOOM_STEP, None, cx),
            ActionId::ZoomOut => self.zoom(1. / ZOOM_STEP, None, cx),
            ActionId::ZoomFit => self.zoom_fit(cx),
            ActionId::Undo => self.undo(cx),
            ActionId::AddVideoLane => self.add_lane(LaneKind::Video, cx),
            ActionId::AddAudioLane => self.add_lane(LaneKind::Audio, cx),
            // The last track of that kind: the one the add key put there, so the
            // two strokes undo each other press for press. Any other track goes
            // through the × in its own header.
            ActionId::RemoveVideoLane => self.remove_last_lane(LaneKind::Video, cx),
            ActionId::RemoveAudioLane => self.remove_last_lane(LaneKind::Audio, cx),
            // The same chooser the + S button opens, and the picked row -- the
            // one the panel draws highlighted -- for the removal: the × on any
            // other row is that row's own door, and both doors are one call.
            ActionId::AddSubtitleTrack => self.pick_and_add_subtitles(cx),
            ActionId::RemoveSubtitleTrack => self.remove_subtitle_track(self.sub_track, cx),
            ActionId::ToggleMute => self.set_volume(|volume| volume.muted = !volume.muted, cx),
            ActionId::VolumeUp => self.set_volume(|volume| volume.step(true), cx),
            ActionId::VolumeDown => self.set_volume(|volume| volume.step(false), cx),
            ActionId::Equalizer => self.open_eq(cx),
            ActionId::Speed => self.open_speed(cx),
            ActionId::Silence => self.open_silence(cx),
            ActionId::Mix => self.open_mix(None, cx),
            ActionId::ToggleSnap => self.toggle_snap(cx),
            ActionId::ToggleSubtitles => self.toggle_subtitles(cx),
            // The keyboard's door to the same list the toolbar button opens.
            // At the window's corner, since a stroke names no place -- and
            // [`menu_at`] keeps it on screen from there.
            ActionId::Theme => self.open_picker(Pick::Theme, Point::default(), cx),
            // Nothing to cancel while nothing is exporting; the export guard in
            // the key handler is what answers this one while there is.
            ActionId::CancelExport => {}
            ActionId::ShowActions => self.show_actions(cx),
        }
    }

    /// Says something to the user. The one door: every message in this editor
    /// comes through here, so "queued rather than overwritten" is a property of
    /// the field and not of seventy call sites remembering to be polite.
    ///
    /// A repeat of what is already at the back is dropped -- holding a key that
    /// refuses would otherwise fill the queue with one sentence, and the count
    /// on the bar would be a count of how long the key was held.
    fn notify_user(&mut self, message: SharedString) {
        push_notice(&mut self.notices, message);
    }

    /// Answers the message on the bar and brings up the next one. Whether there
    /// was one to answer, because a key that dismissed a notice owes a repaint
    /// and a key that dismissed nothing does not.
    fn dismiss_notice(&mut self) -> bool {
        self.notices.pop_front().is_some()
    }

    /// The magnet off and on again, in words: a snap that stops working
    /// silently reads as a bug, and one that starts working silently reads as
    /// one too. The line goes with it -- nothing is being promised any more.
    fn toggle_snap(&mut self, cx: &mut Context<Self>) {
        self.snap = !self.snap;
        self.snap_cue = None;
        self.ghost = None;
        self.notify_user(match self.snap {
            true => "SNAP ON — drags land on clip edges, the playhead and the start".into(),
            false => "SNAP OFF — drags land exactly where the hand leaves them".into(),
        });
        cx.notify();
    }

    /// The actions card, from its key, from the panel button, or from its own
    /// row: open, with an empty search box -- a card that opens showing the
    /// last search would hide most of the list for a reason nobody remembers.
    fn show_actions(&mut self, cx: &mut Context<Self>) {
        self.keys_open = true;
        self.keys_search.clear();
        self.scroll_keys(None);
        self.rebinding = None;
        // One card at a time, the rule the other cards follow.
        self.export_open = false;
        cx.notify();
    }

    /// Moves the actions card's row list by `by` pixels, or puts it back at the
    /// top (`None`). Back to the top after every keystroke that changes the
    /// search: a filtered list is shorter than the offset a scrolled one left
    /// behind, and a card showing the empty space past its last row reads as a
    /// search that found nothing.
    ///
    /// Clamped to what there is to scroll, so the list cannot be pushed off
    /// either end -- `max_offset` is what the last paint measured, which is the
    /// only place that number exists.
    fn scroll_keys(&self, by: Option<f32>) {
        let at = match by {
            Some(by) => (f32::from(self.keys_scroll.offset().y) + by)
                .clamp(-f32::from(self.keys_scroll.max_offset().height), 0.),
            None => 0.,
        };
        self.keys_scroll.set_offset(point(px(0.), px(at)));
    }

    /// The cues over the picture, off and on. Says which it is now *and* what is
    /// on screen while they are on: a toggle whose answer is invisible when the
    /// playhead happens to sit between two cues would read as broken.
    fn toggle_subtitles(&mut self, cx: &mut Context<Self>) {
        self.subs_on = !self.subs_on;
        // Named with its film here too: a notice saying "SUBTITLES ON — eng"
        // over a timeline holding two films' eng tracks names neither.
        let label = self
            .session
            .as_ref()
            .and_then(|session| sub_pick_name(session.subtitles(), self.sub_track))
            .unwrap_or_else(|| "nothing imported".to_string());
        self.notify_user(
            match self.subs_on {
                true => format!("SUBTITLES ON — {label}"),
                false => format!("SUBTITLES OFF — {label} is still on the timeline"),
            }
            .into(),
        );
        cx.notify();
    }

    /// The subtitle track the overlay and the strip are showing: the one a
    /// library row picked, or the first there is. `None` with no timeline and
    /// for an index left over from one that is gone.
    fn subtitle_track(&self) -> Option<&engine::subtitle::SubtitleTrack> {
        self.session.as_ref()?.subtitles().get(self.sub_track)
    }

    /// Whether the editor can be asked for `action` right now, and why not when
    /// it cannot. `on` is the clip the question is about -- the one a clip menu
    /// was opened on -- and `None` asks about the marked clip instead, which is
    /// what a menu that hangs over no clip in particular means by "this one".
    ///
    /// The player's half of [`enable`]: it reads the state, the table decides.
    fn enable(&self, action: ActionId, on: Option<(Lane, usize)>) -> Enable {
        enable(action, self.ctx(on))
    }

    /// The state every one of those questions is asked against, read off the
    /// player once: [`menu_items`] filters a whole menu with it, so the rows a
    /// menu draws and the answers it dims them by come from the same reading.
    fn ctx(&self, on: Option<(Lane, usize)>) -> Ctx {
        let Some(session) = &self.session else {
            return Ctx::default();
        };
        let clip = on
            .or(self.selected)
            .and_then(|(lane, idx)| session.lane_clips(lane).get(idx).map(|clip| (*clip, lane)));
        Ctx {
            clip,
            image: clip.is_some_and(|(clip, _)| {
                session
                    .sources()
                    .get(clip.source)
                    .is_some_and(|s| engine::is_image(&s.path))
            }),
            playhead: frame_at(session.now(), self.fps),
            timeline: true,
            clipboard: self.clipboard.is_some(),
            subtitles: !session.subtitles().is_empty(),
            playable: !nothing_to_play(Some(session)),
            exporting: self.exporting().is_some(),
        }
    }

    /// The same reading for a library row: whether this file can join this
    /// timeline -- the very answer the list greys the row by, so the menu over a
    /// row and the row under it cannot disagree -- and how many clips play it.
    /// [`Player::ctx`] for the other panel.
    fn row_ctx(&self, path: &Path, stream: usize) -> RowCtx {
        let placed = self.session.as_ref().map_or(0, |session| {
            let of_row = session
                .sources()
                .iter()
                .position(|s| s.path == path && s.audio_stream == stream);
            of_row.map_or(0, |idx| {
                session
                    .lanes()
                    .into_iter()
                    .flat_map(|lane| session.lane_clips(lane))
                    .filter(|c| c.source == idx)
                    .count()
            })
        });
        let sources = self
            .session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources);
        RowCtx {
            timeline: self.session.is_some(),
            exporting: self.exporting().is_some(),
            usable: library_rows(
                sources,
                &self.streams,
                &self.decoders,
                self.timeline_audio(),
                |path| {
                    self.session
                        .as_ref()
                        .map_or(0, |session| session.file_frames(path))
                },
            )
            .iter()
            .any(|row| row.path == path && row.stream == stream && row.unusable.is_none()),
            placed,
        }
    }

    /// The one place a clip becomes *the* selected one: a click, a right-click
    /// that opens the menu, and every selection key go through here, so what a
    /// keyboard marks and what a pointer marks are the same state marked the
    /// same way (group and all -- see [`marked`]).
    fn select(&mut self, target: (Lane, usize), cx: &mut Context<Self>) {
        self.selected = Some(target);
        cx.notify();
    }

    /// Every clip the playhead is over, one per lane, in the order the lanes are
    /// drawn -- video first, which is the order [`PlaybackSession::lanes`] comes
    /// in. What the select key walks.
    fn under_playhead(&self) -> Vec<(Lane, usize)> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let now = session.now();
        session
            .lanes()
            .into_iter()
            .filter_map(|lane| Some((lane, session.lane_clip_at(lane, now)?)))
            .collect()
    }

    /// Selects the clip under the playhead, and on a repeat press the next
    /// lane's -- so one key reaches every clip the playhead is over, which is
    /// what makes selection (and everything that acts on a selection: delete,
    /// lift, the equalizer, the grade) reachable with no pointer at all.
    fn select_under_playhead(&mut self, cx: &mut Context<Self>) {
        let under = self.under_playhead();
        let Some(&first) = under.first() else {
            self.notify_user("NOTHING UNDER THE PLAYHEAD — move it onto a clip first".into());
            cx.notify();
            return;
        };
        // Where the current selection sits in that walk decides what "again"
        // means; a selection off the playhead starts the walk over.
        let next = self
            .selected
            .and_then(|sel| under.iter().position(|&clip| clip == sel))
            .map_or(first, |at| under[(at + 1) % under.len()]);
        self.select(next, cx);
    }

    /// Walks the selection along its own lane, wrapping at either end. Nothing
    /// selected means nothing to walk from, so it selects under the playhead
    /// exactly as the select key does: either key can start as well as continue.
    fn select_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let clips = self
            .selected
            .zip(self.session.as_ref())
            .map_or(0, |((lane, _), session)| session.lane_clips(lane).len());
        match (self.selected, clips) {
            // An empty lane is a selection nothing can be stepped from -- as is
            // no selection at all, and the playhead answers both.
            (Some((lane, idx)), len) if len > 0 => {
                let next = if forward {
                    (idx + 1) % len
                } else {
                    (idx + len - 1) % len
                };
                self.select((lane, next), cx);
            }
            _ => self.select_under_playhead(cx),
        }
    }

    /// Cycles the fit policy of the clip the picture is coming from -- the
    /// clicked one when it is a video clip, else the composite's own, exactly as
    /// the colour card picks its target. A whole card for one four-valued
    /// setting would be a card to close; a stroke that cycles it and says what
    /// it landed on is the same setting with nothing to dismiss.
    ///
    /// Only means anything when the clip is not the project's size -- a clip
    /// that already fills the canvas looks the same under all four -- so the
    /// notice says the size it is placing, not just the word.
    fn cycle_fit(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &mut self.session else {
            self.notify_user("no timeline to fit — open a file first".into());
            cx.notify();
            return;
        };
        let target = self
            .selected
            .filter(|(lane, _)| lane.kind == LaneKind::Video)
            .or_else(|| session.video_clip_at(session.now()));
        let Some((lane, idx)) = target else {
            self.notify_user("no clip under the playhead to fit".into());
            cx.notify();
            return;
        };
        let next = next_fit(session.fit_of(lane, idx));
        self.apply_fit(lane, idx, next, cx);
    }

    /// One clip's fit policy set, whichever asked: the stroke that steps to the
    /// next one and the list that names one outright come through here, so they
    /// cannot differ in what they do or in what they say they did.
    fn apply_fit(&mut self, lane: Lane, idx: usize, fit: FitPolicy, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_fit(lane, idx, fit)
        {
            let (w, h) = session.resolution();
            self.notify_user(format!("FIT POLICY: {} on {w}x{h}", fit_label(fit)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The scale against the bed it is drawn on and the timeline it is drawn
    /// from: every clamp, zoom and scroll is worked out through this, and the
    /// bed is measured off the ruler's probe rather than remembered, so a
    /// resized window is a resized view on the very next answer.
    fn view(&self) -> View {
        View {
            scale: self.scale,
            bed: f32::from(self.ruler.get().size.width),
            duration: self.drawn_duration(),
            fps: self.fps,
        }
    }

    /// Magnifies the timeline about a point that stays put: `anchor` is how many
    /// pixels along the bed to hold still (a ctrl+wheel holds the pointer), and
    /// with none it is the playhead -- so the frame being worked on is still the
    /// frame on screen after the zoom. Clamped at both ends by [`View`]: out
    /// stops at the whole timeline on the bed, in at a handful of frames.
    fn zoom(&mut self, factor: f32, anchor: Option<f32>, cx: &mut Context<Self>) {
        let view = self.view();
        let at = self.playhead(view.duration);
        let anchor = anchor.unwrap_or_else(|| self.scale.px_at(at).clamp(0., view.bed));
        self.scale = view.zoomed(factor, anchor);
        // The view a hand chose. A zoom about the playhead leaves it on the
        // bed, so this is given back on the very next frame and only a zoom
        // that took the head off screen -- ctrl+wheel away from it -- holds.
        self.panned = true;
        cx.notify();
    }

    /// All the way back out: the whole timeline across the bed, and the one
    /// thing that reads the timeline's own length to decide how wide a second
    /// is drawn.
    fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        self.scale = self.view().fit();
        cx.notify();
    }

    /// Slides the view along the timeline by `notches` of the wheel, later in
    /// time for a positive one and [`SCROLL_NOTCH_SHARE`] of the bed each. The
    /// scale is untouched: this is the timeline's scrollbar, and the only thing
    /// on the panel that moves what is on screen without magnifying it.
    fn scroll_view(&mut self, notches: f32, cx: &mut Context<Self>) {
        let view = self.view();
        // Nothing painted yet: there is no bed to measure a notch against, and
        // a start moved against a zero width would be a jump to the head.
        if view.bed <= 0. {
            return;
        }
        self.scale = view.scrolled(notches * view.bed * SCROLL_NOTCH_SHARE);
        // The one gesture whose whole purpose is to look away from the
        // playhead: while playing it wins over the follow, which is what every
        // editor does with a scroll during playback.
        self.panned = true;
        cx.notify();
    }

    /// One notch of the wheel anywhere over the timeline -- the ruler or a
    /// lane's bed alike, since a hand aims at the clip it is working on and not
    /// at the strip above it. Ctrl zooms about the pointer, bare scrolls the
    /// view along: the mapping Premiere, Movavi and CapCut share, and the one
    /// the user named.
    ///
    /// The anchor is measured off the ruler's probe wherever the pointer is,
    /// because that probe *is* the bed's x-to-time mapping ([`HEADER_W`]) and
    /// every lane is drawn through the same one.
    fn timeline_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let d = wheel_delta(event);
        if d == 0. {
            return;
        }
        let factor = match d > 0. {
            true => ZOOM_STEP,
            false => 1. / ZOOM_STEP,
        };
        match event.modifiers.control {
            true => {
                let anchor = px_along(event.position.x, self.ruler.get());
                self.zoom(factor, Some(anchor), cx);
            }
            // Up is back towards the head of the timeline, the way a wheel up
            // is back towards the top of a page.
            false => self.scroll_view(-d.signum(), cx),
        }
    }

    /// Cycles the *project's* resolution through [`RESOLUTIONS`], starting from
    /// the media's own -- the one size that must stay reachable, since a project
    /// moved off it has no other way back (the resolution is not an undo step).
    /// Every clip is recomposed onto it, so this is what makes "the project
    /// resolution and the media's are different things" a thing a user can see.
    fn cycle_resolution(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &mut self.session else {
            self.notify_user("no timeline to resize — open a file first".into());
            cx.notify();
            return;
        };
        let (width, height) = next_resolution(session.resolution(), session.native_resolution());
        self.apply_resolution(width, height, cx);
    }

    /// The project resized, whichever asked: the stroke that steps to the next
    /// size and the list that names one outright come through here.
    fn apply_resolution(&mut self, width: u32, height: u32, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_resolution(width, height)
        {
            self.notify_user(format!("PROJECT: {width}x{height}").into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The project cut at another rate: the list names one and this is where it
    /// happens, the way [`apply_resolution`](Self::apply_resolution) is for a
    /// size. The whole timeline is conformed to it by the engine
    /// ([`PlaybackSession::set_frame_rate`]) -- same seconds, same footage --
    /// and the rate the app itself counts frames in follows, since every
    /// timecode, ruler mark and step key here is measured in it.
    fn apply_frame_rate(&mut self, fps: f64, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_frame_rate(fps)
        {
            self.fps = session.meta().frame_rate;
            self.notify_user(format!("PROJECT: {} fps", fps_label(fps)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The project's HDR media shown another way: the list names a rendition and
    /// this is where it happens, the way [`apply_resolution`](Self::apply_resolution)
    /// is for a size. The engine remaps the frame under the playhead at once
    /// ([`PlaybackSession::set_tone`]), so the picture on screen is the picked
    /// one before the notice has faded -- and an SDR project is unmoved, which
    /// is what the notice says rather than pretending something happened.
    fn apply_tone(&mut self, preset: Preset, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_tone(preset)
        {
            self.notify_user(format!("HDR: {} — affects HDR media", tone_label(preset)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Opens a choice list on a setting, where it was asked for. One floating
    /// thing at a time: the click that opens it is the click that closes
    /// whatever menu it was opened from.
    fn open_picker(&mut self, of: Pick, at: Point<Pixels>, cx: &mut Context<Self>) {
        // On the row that is in force, so the first ↑ or ↓ steps off the
        // current value rather than off the top of the list.
        let sel = self
            .choices(of)
            .iter()
            .position(|(.., picked)| *picked)
            .unwrap_or(0);
        self.context_menu = None;
        self.library_menu = None;
        self.picker = Some(Picker { of, at, sel });
        cx.notify();
    }

    /// A row of the open list was picked. Closes the list first -- the rule
    /// every menu item here follows -- then does exactly what the stroke for
    /// that setting does, through the same door.
    fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.picker = None;
        match choice {
            Choice::Size(w, h) => self.apply_resolution(w, h, cx),
            Choice::Fps(fps) => self.apply_frame_rate(fps, cx),
            Choice::Fit(lane, idx, fit) => self.apply_fit(lane, idx, fit, cx),
            Choice::Tone(preset) => self.apply_tone(preset, cx),
            // In force for the next paint -- every token is read through
            // [`ui::theme::palette`], so one store repaints the whole window --
            // and kept for the next launch. A file that could not be written is
            // said out loud: the difference between "picked" and "picked for
            // good" is the user's to know.
            Choice::Theme(id) => {
                ui::theme::set(id);
                if let Err(e) = ui::theme::save(id) {
                    let path = ui::theme::config_path();
                    self.notify_user(
                        format!("THEME COULD NOT BE KEPT — {} — {e}", path.display()).into(),
                    );
                }
                cx.notify();
            }
            // The same field the row's key steps, set outright: a list picks a
            // value, it does not step to one.
            Choice::AudioRate(kbps) => {
                self.audio_kbps = kbps;
                cx.notify();
            }
        }
    }

    /// Every value the open list offers, in the order it lists them. Empty
    /// without a timeline, which is the state where nothing here has a value to
    /// offer -- and where the surfaces that open the list are dimmed anyway.
    fn choices(&self, of: Pick) -> Vec<ChoiceRow> {
        // The palette is not the project's, so it is offered before the
        // timeline is asked about: an empty window is painted too, and its
        // Theme button is live there like the snap beside it.
        if of == Pick::Theme {
            return ui::theme::PaletteId::ALL
                .into_iter()
                .map(|id| {
                    (
                        Choice::Theme(id),
                        id.label().into(),
                        id.detail().into(),
                        id == ui::theme::active(),
                    )
                })
                .collect();
        }
        let Some(session) = &self.session else {
            return Vec::new();
        };
        match of {
            Pick::Resolution => {
                resolution_choices(session.resolution(), session.native_resolution())
            }
            Pick::Fps => fps_choices(session.meta().frame_rate, session.native_frame_rate()),
            Pick::Fit(lane, idx) => {
                fit_choices(lane, idx, session.fit_of(lane, idx), session.resolution())
            }
            Pick::AudioRate => audio_rate_choices(self.audio_kbps),
            Pick::Tone => tone_choices(session.tone()),
            // Answered above, with or without a timeline.
            Pick::Theme => Vec::new(),
        }
    }

    /// Opens the colour card on the clip a grade would go on: the clip that was
    /// clicked when it is a video one, and otherwise the clip the picture is
    /// coming from -- the one the engine's own compositing rule picks, which is
    /// what a person means by "this shot". The fallback stands even now that a
    /// selection key exists: a grade asked for with nothing selected still means
    /// the shot on screen.
    fn open_color(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("no timeline to grade — open a file first".into());
            cx.notify();
            return;
        };
        let target = self
            .selected
            .filter(|(lane, _)| lane.kind == LaneKind::Video)
            .or_else(|| session.video_clip_at(session.now()));
        match target {
            Some(clip) => {
                self.color_open = Some(clip);
                self.color_band = 0;
                self.color_dragging = false;
                // A sample the last card held back belongs to the clip it was
                // dragged on, and this may be another one.
                self.pending_color = None;
                // One card at a time, the rule both the others already follow.
                self.keys_open = false;
                self.export_open = false;
                self.context_menu = None;
            }
            None => self.notify_user("no clip under the playhead to grade".into()),
        }
        cx.notify();
    }

    /// What the card's clip is graded by right now -- the identity for one
    /// nobody has graded, which is what the sliders start at. A sample a drag is
    /// still holding wins over the clip's own: it is what the hand has asked
    /// for, so it is what the sliders show and what the next sample builds on.
    fn color_params(&self) -> ColorParams {
        if let Some(params) = self.pending_color {
            return params;
        }
        self.color_open
            .zip(self.session.as_ref())
            .and_then(|((lane, idx), session)| session.color_of(lane, idx).copied())
            .unwrap_or_default()
    }

    /// Puts `params` on the card's clip, or takes the grade off when they are
    /// the identity -- a slider walked back to the middle leaves the clip
    /// ungraded rather than carrying a do-nothing entry, which is what keeps an
    /// untouched project byte-identical. The engine reseeks on the edit, so the
    /// frame on screen repaints through the new grade; this only owes the flags
    /// that reseek clears.
    fn set_color(&mut self, params: ColorParams, cx: &mut Context<Self>) {
        self.write_color(params, false, cx);
    }

    /// Both writes: `live` is the one that takes no undo step, which is what
    /// every sample *inside* a drag goes through
    /// (`PlaybackSession::set_color_live`). Either way the engine reseeks, so
    /// the picture -- and the histogram counted off it -- is regraded at once.
    fn write_color(&mut self, params: ColorParams, live: bool, cx: &mut Context<Self>) {
        // Any write supersedes a held sample, whichever way it arrived -- a key,
        // a reset, or the flush that took this one out of the stash.
        self.pending_color = None;
        let Some((lane, idx)) = self.color_open else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        let grade = Some(params).filter(|p| !p.is_identity());
        let took = match live {
            true => session.set_color_live(lane, idx, grade),
            false => session.set_color(lane, idx, grade),
        };
        if took {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Moves the picked slider by `steps` of [`COLOR_STEP`], clamped to that
    /// band's range. One edit, so one undo step per press.
    fn nudge_color(&mut self, steps: f32, cx: &mut Context<Self>) {
        let mut params = self.color_params();
        let (_, low, high) = COLOR_BANDS[self.color_band];
        let value = band_mut(&mut params, self.color_band);
        *value = (*value + steps * COLOR_STEP).clamp(low, high);
        self.set_color(params, cx);
    }

    /// Where the pointer sits along a slider, as that band's value: the left end
    /// of the bar is the bottom of its range and the right end the top. Called
    /// on every pointer sample, so the grade -- and the picture, and the
    /// histogram over it -- moves under the hand.
    ///
    /// `first` is the press: it takes the undo step the whole gesture rolls back
    /// to, and every sample after it is live. That is why it writes even when
    /// the value did not change -- without that snapshot the rest of the drag
    /// would be unundoable.
    ///
    /// Values land on the [`COLOR_STEP`] grid the keys use, which also bounds
    /// one drag to forty-odd entries in the project's colour table.
    ///
    /// Samples crossed while the worker still owes a frame are held rather than
    /// written ([`stash_or_write`]): a reopen costs half a second on a big film,
    /// so a bar-wide sweep that wrote every step would queue forty opens, cancel
    /// thirty-nine of them and freeze the window for the sum. What is written is
    /// one grade per frame the worker actually delivers.
    fn drag_color(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        let (_, low, high) = COLOR_BANDS[self.color_band];
        let along = frac_along(x, self.color_bars[self.color_band].get());
        let value = color_snap(low + along * (high - low)).clamp(low, high);
        let mut params = self.color_params();
        let at = band_mut(&mut params, self.color_band);
        if *at == value && !first {
            return;
        }
        *at = value;
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_color, params, first, busy) {
            Some(params) => self.write_color(params, !first, cx),
            // The sliders draw off the held sample, so the handle goes on
            // following the hand while the picture catches up.
            None => cx.notify(),
        }
    }

    /// Opens the speed card on the clip whose rate is to change: the selected
    /// one, or -- with nothing selected -- the clip the picture is coming from,
    /// which is what a person means by "this shot". Either half of a take will
    /// do: a rate applies to the whole group, so opening it on the sound and
    /// opening it on the picture are the same card.
    fn open_speed(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("no timeline to re-time — open a file first".into());
            cx.notify();
            return;
        };
        match self
            .selected
            .or_else(|| session.video_clip_at(session.now()))
        {
            Some(clip) => {
                self.speed_open = Some(clip);
                self.speed_dragging = false;
                // The colour card's rule: a held sample is the last clip's.
                self.pending_speed = None;
                // One card at a time, the rule the other four follow.
                self.keys_open = false;
                self.export_open = false;
                self.eq_open = None;
                self.color_open = None;
                self.close_silence();
                self.context_menu = None;
            }
            None => self.notify_user("no clip under the playhead to re-time".into()),
        }
        cx.notify();
    }

    /// What the card's clip plays at right now -- real time for one nobody has
    /// touched, which is where the bar starts.
    fn card_speed(&self) -> Speed {
        if let Some(speed) = self.pending_speed {
            return speed;
        }
        self.speed_open
            .zip(self.session.as_ref())
            .map_or(Speed::NORMAL, |((lane, idx), session)| {
                session.speed_of(lane, idx)
            })
    }

    /// Writes a rate at the card's clip and its whole group -- one undo step for
    /// the lot ([`engine::PlaybackSession::set_speed`]). The engine reseeks, so
    /// the picture runs at the new rate and the sound is resampled from the next
    /// chunk on; a refusal (a slower clip would run into its neighbour) comes
    /// back in the engine's own words and *names* the clip in the way, because
    /// "it did not fit" is not something a person can go and fix.
    fn set_speed(&mut self, speed: Speed, cx: &mut Context<Self>) {
        self.write_speed(speed, false, cx);
    }

    /// Both writes: `live` is the one that takes no undo step, which is what
    /// every sample *inside* a drag goes through -- so a drag from 1.00x to
    /// 2.00x is one undo press and lands back where the hand picked it up, and
    /// the whole linked group comes back with it.
    fn write_speed(&mut self, speed: Speed, live: bool, cx: &mut Context<Self>) {
        // The colour card's rule: a write supersedes whatever a drag was holding.
        self.pending_speed = None;
        let Some((lane, idx)) = self.speed_open else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        if speed != session.speed_of(lane, idx) {
            let wrote = match live {
                true => session.set_speed_live(lane, idx, speed),
                false => session.set_speed(lane, idx, speed),
            };
            match wrote {
                Ok(()) => self.reset_after_reseek(),
                Err(e) => self.notify_user(e.to_string().into()),
            }
        }
        cx.notify();
    }

    /// One [`SPEED_STEP`] per keystroke, clamped to what a [`Speed`] can hold.
    fn nudge_speed(&mut self, steps: i32, cx: &mut Context<Self>) {
        let at = i32::from(self.card_speed().permille()) + steps * SPEED_STEP;
        self.set_speed(speed_at(at), cx);
    }

    /// Where the pointer sits along the bar, as a rate: the left end is
    /// [`Speed::MIN`] and the right end [`Speed::MAX`], on the same
    /// [`SPEED_STEP`] grid the keys move on -- so a drag can land on exactly
    /// 1.00x and the same drag twice is one entry, not forty.
    /// `first` is the press: it takes the undo step the whole gesture rolls back
    /// to, and every sample after it is live -- the colour card's rule, for the
    /// colour card's reason.
    fn drag_speed(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        let along = frac_along(x, self.speed_bar.get());
        let lo = f32::from(Speed::MIN.permille());
        let hi = f32::from(Speed::MAX.permille());
        let raw = lo + along * (hi - lo);
        // Snapped to the grid, then to real time itself when it is within half a
        // step of it: 1.00x is the one value a hand must be able to hit, and
        // nothing about the bar's geometry guarantees a pixel lands on it.
        let stepped = (raw / SPEED_STEP as f32).round() as i32 * SPEED_STEP;
        // Held back while the worker is busy, the colour card's way and for a
        // sharper reason: a live rate also restarts the sound, so a sweep that
        // wrote every step would restart it forty times.
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_speed, speed_at(stepped), first, busy) {
            Some(speed) => self.write_speed(speed, !first, cx),
            None => cx.notify(),
        }
    }

    /// Writes what a slider drag held back, now that the worker has delivered.
    /// The gate is the frame that landed and never a timer: a 100 ms tick
    /// ([`SCRUB_GAP`]) says nothing about a reopen that costs half a second, and
    /// a drag gated on one would still queue opens nobody sees.
    ///
    /// Called again by the release, where readiness is beside the point: the
    /// value the hand let go on is owed whatever the worker is doing, and a
    /// gesture may not end on a sample that was dropped.
    fn flush_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(params) = self.pending_color.take() {
            self.write_color(params, true, cx);
        }
        if let Some(speed) = self.pending_speed.take() {
            self.write_speed(speed, true, cx);
        }
    }

    /// Opens the silence card on the clip to be scanned: the selected one, or
    /// -- with nothing selected -- the clip the picture is coming from, which is
    /// the rule the speed card follows and what a person means by "this shot".
    /// Either half of a take will do: both halves of an A/V take name the same
    /// file and play the same source frames, which is the whole of what a scan
    /// is of ([`ScanKey`]).
    ///
    /// The card is up on the next frame whatever the file is: a still is
    /// refused by name here, where the answer costs a look at the path, and
    /// everything the decoder has to open the file to know -- a track that is
    /// not there, a read that fails -- is refused the same way when the scan
    /// lands, because a fifty-second decode is not a thing to open a card
    /// behind ([`Player::start_silence_scan`]).
    fn open_silence(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("no timeline to scan — open a file first".into());
            cx.notify();
            return;
        };
        match self
            .selected
            .or_else(|| session.video_clip_at(session.now()))
            .map(|clip| audio_half(session, clip))
        {
            Some((lane, idx)) => {
                let found = self.session.as_ref().and_then(|session| {
                    let clip = *session.lane_clips(lane).get(idx)?;
                    Some((session.sources().get(clip.source)?.clone(), clip))
                });
                // A still is asked *before* the decoder is: handing a png to the
                // mp4 demuxer answers "a box with a larger size than it", which
                // is a true sentence about a container and nothing a person can
                // act on. A picture has no sound for the same reason a silent
                // video has none, so it is refused in the same words.
                let Some((source, clip)) = found else {
                    cx.notify();
                    return;
                };
                if engine::is_image(&source.path) {
                    self.notify_user(unscannable(lane, idx, &source.path).into());
                    cx.notify();
                    return;
                }
                self.silence_open = Some((lane, idx));
                self.silence_field = 0;
                // One card at a time, the rule the other four follow.
                self.keys_open = false;
                self.export_open = false;
                self.eq_open = None;
                self.color_open = None;
                self.speed_open = None;
                self.context_menu = None;
                // The clip's own range, not the file's: the scan reads what this
                // clip plays and nothing else, so a take cut in half costs half
                // the decode and finds only what is still on the timeline.
                let key = (
                    source.path.clone(),
                    source.audio_stream,
                    clip.in_frame,
                    clip.out_frame,
                );
                match scan_plan(
                    self.silence_levels.contains_key(&key),
                    self.silence_scan.as_ref().map(|scan| &scan.key),
                    &key,
                ) {
                    ScanPlan::Marks => self.scan_silences(),
                    ScanPlan::Start => self.start_silence_scan(key, cx),
                    ScanPlan::Wait => {}
                }
            }
            None => self.notify_user("no clip under the playhead to scan".into()),
        }
        cx.notify();
    }

    /// Opens the mix card. `lane` is the row it lands on -- the track whose
    /// header was clicked -- and `None` starts at the top, which is what the
    /// stroke means.
    ///
    /// Nothing here is a clip's, so nothing is refused for want of a selection:
    /// a timeline with no audio track at all still has a limiter to set, and a
    /// fader on an empty track is the level the next take lands at.
    fn open_mix(&mut self, lane: Option<Lane>, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.mix_open = true;
        self.mix_field = lane
            .and_then(|lane| self.mix_lanes().iter().position(|&l| l == lane))
            .unwrap_or(0);
        // One card at a time, the rule the other five follow.
        self.keys_open = false;
        self.export_open = false;
        self.eq_open = None;
        self.color_open = None;
        self.speed_open = None;
        self.close_silence();
        self.context_menu = None;
        cx.notify();
    }

    /// The audio tracks the card shows a fader for, top to bottom: *every* one
    /// of them, empty ones included -- what the timeline lays out, not what the
    /// mixer happens to open (`Project::audio_lanes` leaves an empty track out,
    /// and a fader that disappeared when a track was cleared would be a setting
    /// nobody could reach).
    fn mix_lanes(&self) -> Vec<Lane> {
        self.session.as_ref().map_or_else(Vec::new, |session| {
            session
                .lanes()
                .into_iter()
                .filter(|l| l.kind == LaneKind::Audio)
                .collect()
        })
    }

    /// Moves the row the card has picked: a fader by [`MIX_DB_STEP`], the
    /// ceiling by the same, and the switch either way (a ring of two, like the
    /// silence card's unit row).
    ///
    /// Every one of them goes through the session, which hands it straight to
    /// the running mixer: what the ear hears while the arrow is held is the mix
    /// that is being set, and nothing is rebuilt to make that true -- no reseek,
    /// so no `reset_after_reseek` and no blink in the picture behind the card
    /// ([`engine::PlaybackSession::set_lane_gain_db`]).
    fn nudge_mix(&mut self, steps: i32, cx: &mut Context<Self>) {
        let lanes = self.mix_lanes();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match lanes.get(self.mix_field) {
            Some(&lane) => {
                let at = session.lane_gain_db(lane) + steps as f32 * MIX_DB_STEP;
                session.set_lane_gain_db(lane, at);
            }
            None => {
                let limiter = session.limiter();
                let at = match self.mix_field - lanes.len() {
                    0 => Limiter {
                        on: limiter.on,
                        ..limiter
                    }
                    .with_ceiling(limiter.ceiling_db + steps as f32 * MIX_DB_STEP),
                    _ => Limiter {
                        on: !limiter.on,
                        ..limiter
                    },
                };
                session.set_limiter(at);
            }
        }
        cx.notify();
    }

    /// Closes it and drops the preview with it: marks left on the lane after
    /// the card is gone would name frames the next edit has already moved.
    fn close_silence(&mut self) {
        self.silence_open = None;
        self.silence_marks.clear();
        self.cancel_silence_scan();
    }

    /// Tells the worker nobody is waiting any more. It gives up at its next
    /// chunk and the levels it had are dropped: half a track is not an answer,
    /// and the flag stays set on the [`Arc`] the landing closure holds, which is
    /// how that closure knows to keep its hands off the card.
    fn cancel_silence_scan(&mut self) {
        if let Some(scan) = self.silence_scan.take() {
            scan.progress
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Hands the decode to a worker and returns at once -- the card is drawn by
    /// the very next frame, saying it is scanning. Fifty-one seconds on a 25 GB
    /// film is what this used to cost on the render thread, with the card
    /// marked open and nothing on screen.
    ///
    /// Whatever was scanning is cancelled first: one card, one scan, and the
    /// clip that has just been asked about is the one worth the disk.
    ///
    /// Only the clip's own `[in, out)` is read -- source frames over the
    /// project's rate, the same seconds [`engine::Project`] hands the decoder
    /// for playback -- so half a take is half a wait.
    fn start_silence_scan(&mut self, key: ScanKey, cx: &mut Context<Self>) {
        self.cancel_silence_scan();
        self.silence_marks.clear();
        let progress = Arc::new(engine::silence::Progress::default());
        let range = source_secs(&key, self.fps);
        let scan = cx.background_executor().spawn({
            let (key, progress) = (key.clone(), Arc::clone(&progress));
            async move { engine::silence::levels_with_progress(&key.0, key.1, range, &progress) }
        });
        let now = Instant::now();
        self.silence_scan = Some(SilenceScan {
            key: key.clone(),
            started: now,
            progress: Arc::clone(&progress),
            seen: 0,
            since: now,
        });
        cx.spawn(async move |this, cx| {
            let landed = scan.await;
            this.update(cx, |this, cx| {
                // Cancelled means the card moved on or closed: the levels are a
                // prefix of a track nobody asked about any more.
                if progress.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                this.silence_scan = None;
                match landed {
                    Ok(Some(levels)) => {
                        this.silence_levels.insert(key.clone(), Arc::new(levels));
                        this.scan_silences();
                    }
                    // A source with no audio track is not one long silence: it
                    // is a clip this card has nothing to say about, named so the
                    // user knows which one it meant.
                    Ok(None) => {
                        if let Some((lane, idx)) = this.silence_open {
                            this.notify_user(unscannable(lane, idx, &key.0).into());
                        }
                        this.close_silence();
                    }
                    Err(e) => {
                        this.close_silence();
                        this.notify_user(format!("SCAN FAILED: {e}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps the scanning line's stall clock, for [`Player::poll_import`]'s
    /// reason: sampled once per frame rather than while drawing.
    fn poll_silence(&mut self) {
        if let Some(scan) = &mut self.silence_scan {
            scan.poll();
        }
    }

    /// Applies the settings to levels already in hand and replaces the preview
    /// -- never stacks on it. Arithmetic only: the decode is
    /// [`Player::start_silence_scan`]'s and happens once per source, so every
    /// run here is numbers already read, which is what makes moving a threshold
    /// feel like moving a slider. A source still being scanned has no marks yet
    /// and says so on the card.
    ///
    /// Changes nothing about the project: a preview is not an edit, and no undo
    /// step is spent until a button is pressed.
    fn scan_silences(&mut self) {
        let Some((lane, idx)) = self.silence_open else {
            return;
        };
        self.silence_marks.clear();
        // Copied out before anything is written back: the cache below lives on
        // the same struct the session does.
        let Some((clip, source)) = self.session.as_ref().and_then(|session| {
            let clip = *session.lane_clips(lane).get(idx)?;
            Some((clip, session.sources().get(clip.source)?.clone()))
        }) else {
            return;
        };
        // Nothing read yet: the worker is running and the card is drawing its
        // line. The marks arrive with the levels.
        let Some(levels) = self
            .silence_levels
            .get(&(
                source.path.clone(),
                source.audio_stream,
                clip.in_frame,
                clip.out_frame,
            ))
            .cloned()
        else {
            return;
        };
        self.silence_marks = engine::silence::timeline_regions(
            &clip,
            self.fps,
            &engine::silence::regions(&levels, self.silence),
        );
    }

    /// Moves the picked row by `steps` and re-runs the scan against it, so the
    /// marks on the lane are always what the numbers on the card say.
    fn nudge_silence(&mut self, steps: i32) {
        let secs = |at: f64| {
            (at + f64::from(steps) * SILENCE_SECS_STEP)
                .clamp(SILENCE_SECS_RANGE.0, SILENCE_SECS_RANGE.1)
        };
        match self.silence_field {
            // Round either way, like the fit policy's cycle: three choices are
            // a ring, not a range.
            0 => {
                let at = SCOPES.iter().position(|&s| s == self.silence_scope);
                let step = steps.rem_euclid(SCOPES.len() as i32) as usize;
                self.silence_scope = SCOPES[(at.unwrap_or(0) + step) % SCOPES.len()];
            }
            1 => {
                self.silence.threshold_db = (self.silence.threshold_db
                    + steps as f32 * SILENCE_DB_STEP)
                    .clamp(SILENCE_DB_RANGE.0, SILENCE_DB_RANGE.1)
            }
            // Two spellings of the same level, so either arrow flips it -- a
            // ring of two, like the scope row's.
            2 => self.silence_dbfs = !self.silence_dbfs,
            3 => self.silence.min_silence = secs(self.silence.min_silence),
            4 => self.silence.padding = secs(self.silence.padding),
            5 => self.silence.min_keep = secs(self.silence.min_keep),
            _ => {
                self.silence_factor =
                    silence_rate(i32::from(self.silence_factor.permille()) + steps * SPEED_STEP)
            }
        }
        // Neither the scope nor the rate is part of the scan, but re-running is
        // cheap (the levels are cached) and one path is one place for the marks
        // to come from.
        self.scan_silences();
    }

    /// Which lanes an apply reaches, as the card's scope row says it: the
    /// lanes of the take the scanned clip belongs to, that clip's lane alone,
    /// or every lane there is.
    ///
    /// The take's lanes are the ones carrying its group id -- a link is one
    /// span on however many lanes, so "the take" is exactly the set of lanes
    /// that would otherwise be pulled apart. Nothing widens behind the user's
    /// back: [`Project::cut_regions`] refuses a scope that would split a take,
    /// and this row is how the user says the take instead.
    fn silence_lanes(&self) -> Vec<Lane> {
        let (Some((lane, idx)), Some(session)) = (self.silence_open, self.session.as_ref()) else {
            return Vec::new();
        };
        match self.silence_scope {
            Scope::Track => vec![lane],
            Scope::Everything => session.lanes(),
            Scope::Take => match session.lane_clips(lane).get(idx).and_then(|c| c.link) {
                None => vec![lane],
                Some(id) => session
                    .lanes()
                    .into_iter()
                    .filter(|&l| {
                        l == lane || session.lane_clips(l).iter().any(|c| c.link == Some(id))
                    })
                    .collect(),
            },
        }
    }

    /// What an apply acts on: the previewed set and the lanes it reaches, or
    /// nothing at all with a notice saying so in the numbers that found
    /// nothing.
    fn previewed(&mut self) -> Option<(Vec<(u32, u32)>, Vec<Lane>)> {
        if self.silence_marks.is_empty() {
            self.notify_user(
                format!(
                    "no silence under {:.0} dBFS lasting {:.2} s — raise the threshold or forgive less",
                    self.silence.threshold_db, self.silence.min_silence
                )
                .into(),
            );
            return None;
        }
        Some((self.silence_marks.clone(), self.silence_lanes()))
    }

    /// What an apply says afterwards: which tracks it reached, and -- when that
    /// was not all of them -- that the rest were left where they were. The
    /// scope is a choice, so the confirmation has to name the choice.
    fn silence_reach(&self, lanes: &[Lane]) -> String {
        let named = lanes
            .iter()
            .map(|l| l.label())
            .collect::<Vec<_>>()
            .join("+");
        match self.silence_scope {
            Scope::Everything => "on every track".to_string(),
            _ => format!("on {named} — other tracks untouched"),
        }
    }

    /// Cuts every previewed silence out of the lanes the scope names, rippling
    /// each hole closed -- one edit and **one** undo press however many there
    /// were ([`engine::PlaybackSession::cut_regions`]). Tracks outside the
    /// scope do not move; a scope that would take half a take with it comes
    /// back refused in the engine's own words, naming both halves.
    fn cut_silences(&mut self, cx: &mut Context<Self>) {
        let Some((regions, lanes)) = self.previewed() else {
            cx.notify();
            return;
        };
        let saved = f64::from(regions.iter().map(|&(_, len)| len).sum::<u32>()) / self.fps;
        let (count, reach) = (regions.len(), self.silence_reach(&lanes));
        let Some(session) = self.session.as_mut() else {
            cx.notify();
            return;
        };
        match session.cut_regions(&regions, &lanes) {
            Ok(()) => {
                self.close_silence();
                // Every hole closed moves the clips after it up a place, so the
                // selection now names a different clip than the one that is
                // highlighted -- dropped here as after every other edit that
                // moves indexes (a delete, a paste, an undo).
                self.selected = None;
                self.reset_after_reseek();
                self.notify_user(
                    format!(
                        "{count} SILENCES CUT {reach} — {} shorter, {} takes it back",
                        secs_label(saved),
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Plays them fast instead of cutting them, closing the room each one no
    /// longer needs. One undo press like the cut, and the same scope; the
    /// refusals (a clip lapping over a silence, a scope that would split a
    /// take) come back in the engine's own words and name the lane and frame,
    /// and the card stays up so the numbers that produced it are still on
    /// screen.
    fn speed_silences(&mut self, cx: &mut Context<Self>) {
        let Some((regions, lanes)) = self.previewed() else {
            cx.notify();
            return;
        };
        let (count, rate) = (regions.len(), self.silence_factor);
        let reach = self.silence_reach(&lanes);
        let Some(session) = self.session.as_mut() else {
            cx.notify();
            return;
        };
        match session.speed_regions(&regions, rate, &lanes) {
            Ok(()) => {
                self.close_silence();
                // Splitting each silence out and closing the room it no longer
                // needs moves indexes exactly as the cut does: the selection
                // goes with them.
                self.selected = None;
                self.reset_after_reseek();
                self.notify_user(
                    format!(
                        "{count} SILENCES AT {rate} {reach} — {} takes it back",
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Whether a card owns the window. While one does the timeline under it is
    /// out of reach, so a right-click there opens no menu -- the same rule the
    /// key handler and the drop target already follow.
    /// Whether anything at all is drawn over the window -- a card, a menu or an
    /// open list. What the hover labels stand aside for ([`OVERLAID`]): a
    /// tooltip belongs to the surface the pointer is on, and while one of these
    /// is up that surface is behind it.
    fn overlaid(&self) -> bool {
        self.modal()
            || self.context_menu.is_some()
            || self.library_menu.is_some()
            || self.picker.is_some()
    }

    fn modal(&self) -> bool {
        self.keys_open
            || self.export_open
            || self.eq_open.is_some()
            || self.color_open.is_some()
            || self.speed_open.is_some()
            || self.silence_open.is_some()
            || self.mix_open
            || self.exporting().is_some()
    }

    /// The pointer's way out of whatever card is up: what every scrim's press
    /// calls, so `esc` is *a* way out and never the only one. One list, and the
    /// same one [`Player::modal`] reads -- a card that can be counted there but
    /// not closed here is a card a hand alone cannot shut, which is what
    /// `every_card_closes_without_the_keyboard` fails on.
    ///
    /// Every card at once because only one is ever up (`export_open`): closing
    /// "the" card and closing all of them are the same act.
    fn close_card(&mut self) {
        self.keys_open = false;
        self.keys_search.clear();
        self.rebinding = None;
        self.export_open = false;
        // The two things typed *into* the export card go with it: a field left
        // open would take the next keystroke for a card that is gone.
        self.mbps_edit = None;
        self.picker = None;
        self.eq_open = None;
        self.eq_dragging = false;
        self.color_open = None;
        self.speed_open = None;
        // Marks and a running scan go with this one, which is why it is a call
        // and not an assignment ([`Player::close_silence`]).
        if self.silence_open.is_some() {
            self.close_silence();
        }
        self.mix_open = false;
    }

    /// Which of [`Repeat`]'s three the window is in, for the hold gate at the
    /// top of the key handler. Not [`Player::modal`]: that asks whether an
    /// overlay is up at all, and here the cards with sliders in them are
    /// exactly the ones that answer differently from the keys menu and the
    /// export card.
    fn repeat_scope(&self) -> Repeat {
        // A number being typed is a value under the arrows, exactly as a card's
        // slider is -- so a held arrow runs it. Asked before the export card
        // below, which otherwise repeats nothing.
        if self.mbps_edit.is_some() {
            Repeat::Card
        } else if self.rebinding.is_some()
            || self.keys_open
            || self.export_open
            || self.exporting().is_some()
        {
            Repeat::Nothing
        } else if self.eq_open.is_some()
            || self.color_open.is_some()
            || self.speed_open.is_some()
            || self.silence_open.is_some()
            || self.mix_open
        {
            Repeat::Card
        } else {
            Repeat::Keymap
        }
    }

    /// Opens the equalizer on the selected clip. Audio only, and it says so
    /// rather than opening a card of bands that would reach nothing: a video
    /// clip carries no sound of its own here (the sound is the audio lane's),
    /// and the model would take the setting without anything ever playing it.
    fn open_eq(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let refusal = match (self.selected, &self.session) {
            (_, None) => Some("NO TIMELINE — open a file first".to_string()),
            (None, _) => Some(format!(
                "NOTHING SELECTED — click an audio clip or press {}, then ask again",
                self.keymap.display(ActionId::Select)
            )),
            (Some((lane, _)), _) if lane.kind != LaneKind::Audio => Some(
                "NOT AN AUDIO CLIP — the equalizer works on the sound, so pick a clip in an audio lane".to_string(),
            ),
            _ => None,
        };
        if let Some(refusal) = refusal {
            self.notify_user(refusal.into());
            cx.notify();
            return;
        }
        let (lane, idx) = self.selected.expect("checked above");
        let session = self.session.as_ref().expect("checked above");
        // What the clip already plays through, or the flat default -- so the
        // card opens on the curve that is in force and a reopen shows the last
        // drag rather than a fresh set of zeroes.
        self.eq_params = session
            .eq_of(lane, idx)
            .cloned()
            .unwrap_or_else(EqParams::default_layout);
        self.eq_band = 0;
        self.eq_dragging = false;
        self.eq_open = Some((lane, idx));
        // One card at a time, the rule the other two already follow.
        self.keys_open = false;
        self.export_open = false;
        self.context_menu = None;
        cx.notify();
    }

    /// Writes what the card is showing at its clip: one undo step, one entry in
    /// the append-only equalizer table, so this is called once per *gesture* --
    /// the end of a drag, a keystroke -- and never per pointer sample.
    ///
    /// A curve that moves nothing is stored as *no* equalizer at all, which is
    /// what keeps a clip nobody has touched on the identity path through
    /// playback and export (`engine::eq::EqParams::is_identity`).
    fn commit_eq(&mut self, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.eq_open else {
            return;
        };
        let params = (!self.eq_params.is_identity()).then(|| self.eq_params.clone());
        if let Some(session) = &mut self.session {
            session.set_eq(lane, idx, params);
        }
        // `set_eq` reseeks inside the engine -- that is what makes the change
        // audible at once -- and a reseek is what these flags are about.
        self.reset_after_reseek();
        cx.notify();
    }

    /// Changes the picked band in place and says whether anything moved. Every
    /// edit of a band goes through here -- the drag, each key, each stepper
    /// button -- so the card has exactly one place that clamps a band into what
    /// the graph can draw, and no caller has to remember the limits.
    fn set_band(&mut self, change: impl FnOnce(&mut Band)) -> bool {
        let Some(band) = self.eq_params.bands.get_mut(self.eq_band) else {
            return false;
        };
        let was = *band;
        change(band);
        band.freq_hz = band.freq_hz.clamp(EQ_FREQ_LOW, EQ_FREQ_HIGH);
        band.gain_db = band.gain_db.clamp(-EQ_GAIN_LIMIT, EQ_GAIN_LIMIT);
        band.q = band.q.clamp(EQ_Q_LOW, EQ_Q_HIGH);
        *band != was
    }

    /// The keyboard's and the buttons' version of a drag: one step on the picked
    /// band, committed straight away -- neither has a release to wait for.
    fn nudge_band(&mut self, change: impl FnOnce(&mut Band), cx: &mut Context<Self>) {
        if self.set_band(change) {
            self.commit_eq(cx);
        }
    }

    /// Where the pointer sits in the graph, as the picked band's frequency and
    /// gain: across is the frequency axis and down is the gain one, so the
    /// handle follows the hand both ways rather than sliding up a rail. Called
    /// on every pointer sample, so the curve bends under it; the write is the
    /// release's ([`commit_eq`](Player::commit_eq)).
    fn drag_band(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let bounds = self.eq_graph.get();
        let gain = (0.5 - frac_down(at.y, bounds)) * 2. * EQ_GAIN_LIMIT;
        let freq = eq_freq(frac_along(at.x, bounds));
        if self.set_band(|b| {
            b.gain_db = gain;
            b.freq_hz = freq;
        }) {
            cx.notify();
        }
    }

    /// A band added beside the picked one, at the frequency with the most room
    /// around it ([`inserted_band`]), and picked so the next keystroke moves the
    /// band that was just made. Refused rather than silently ignored at the cap.
    fn add_band(&mut self, cx: &mut Context<Self>) {
        if self.eq_params.bands.len() >= EQ_BANDS_MAX {
            self.notify_user(
                format!(
                    "EQUALIZER FULL — {EQ_BANDS_MAX} bands is all this card holds; move one instead"
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let band = inserted_band(&self.eq_params.bands, self.eq_band);
        self.eq_band = (self.eq_band + 1).min(self.eq_params.bands.len());
        self.eq_params.bands.insert(self.eq_band, band);
        self.commit_eq(cx);
    }

    /// Takes the picked band out. The last one stays: an equalizer of no bands
    /// is a card with nothing to edit, and flattening is what "off" means here.
    fn remove_band(&mut self, cx: &mut Context<Self>) {
        if self.eq_params.bands.len() <= 1 {
            self.notify_user("LAST BAND — flatten it instead (r), or close the card".into());
            cx.notify();
            return;
        }
        self.eq_params.bands.remove(self.eq_band);
        self.eq_band = self.eq_band.min(self.eq_params.bands.len() - 1);
        self.commit_eq(cx);
    }

    /// Which band a press on the graph grabs: the nearest one along the
    /// frequency axis, so the whole box is the handle rather than a 10 px dot
    /// -- and a press that misses every dot still moves the band it is under.
    fn nearest_band(&self, x: Pixels) -> usize {
        let at = frac_along(x, self.eq_graph.get());
        self.eq_params
            .bands
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (eq_x(a.freq_hz) - at)
                    .abs()
                    .total_cmp(&(eq_x(b.freq_hz) - at).abs())
            })
            .map_or(0, |(i, _)| i)
    }

    /// Jumps the timeline.
    fn seek(&mut self, t: f64, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        session.seek(t);
        self.reset_after_reseek();
        cx.notify();
    }

    /// The keyboard's seek: whole frames along the timeline, through the same
    /// door a ruler click uses -- so a step while playing keeps playing, exactly
    /// as a click does. It starts from the frame the transport is showing, which
    /// past the end is the last one, and that is what lets a step back off EOS
    /// revive the picture (the engine's seek leaves [`Transport::Ended`]). Both
    /// ends clamp, so the two go-to actions are this same step asked for more
    /// frames than the timeline has. Selection is untouched: a seek is not an
    /// edit, and nothing it does moves a clip index.
    fn step(&mut self, frames: i64, cx: &mut Context<Self>) {
        let ended = self.transport() == Transport::Ended;
        let Some(session) = &self.session else {
            return;
        };
        let last = ((session.timeline_duration() * self.fps).round() as i64 - 1).max(0);
        let now = match ended {
            true => last,
            false => i64::from(frame_at(session.now(), self.fps)),
        };
        let target = now.saturating_add(frames).clamp(0, last);
        self.seek(target as f64 / self.fps, cx);
    }

    /// Splits the clip under the playhead. Metadata only: the timeline->source
    /// mapping is unchanged, so nothing reseeks and no flag is touched.
    fn cut(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // Snapped to the source's own grid where one is within reach
        // ([`Player::cut_frame`]): a cut a third of a second off a sync point
        // looks identical on the bed and turns an export that copies its
        // picture in minutes into one that codes every frame of it for hours.
        // The playhead goes with it -- what was cut has to be where the line
        // is, or the next stroke acts a few frames from where it looks.
        let Some(session) = &self.session else {
            return;
        };
        let now = frame_at(session.now(), self.fps);
        let at = self.cut_frame(now);
        if at != now {
            self.seek(f64::from(at) / self.fps, cx);
        }
        if let Some(session) = &mut self.session {
            session.cut_at(f64::from(at) / self.fps);
        }
        self.selected = None;
        cx.notify();
    }

    /// Rejoins whatever meets under the playhead and puts it back in one group
    /// -- the inverse of [`Player::cut`], and metadata only like it. The engine
    /// decides what is joinable; a refusal is worded here, because `false` is
    /// all it says and a key that looks broken is worse than one that explains
    /// itself.
    fn regroup(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if let Some(session) = &mut self.session {
            if session.regroup_at(session.now()) {
                self.selected = None;
            } else {
                self.notify_user(
                    "NOTHING TO REGROUP — put the playhead where two clips meet, on frames that were cut apart"
                        .into(),
                );
            }
        }
        cx.notify();
    }

    /// Takes the selected clip out of its group, so the picture and the sound
    /// under it are edited apart from here on: each half selects, moves, trims
    /// and is removed alone, and both draw outlined instead of tinted. The
    /// selection stays -- the half that was clicked is still the half in hand.
    fn detach(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match (&mut self.session, self.selected) {
            (Some(session), Some((lane, idx))) => {
                if !session.ungroup(lane, idx) {
                    self.notify_user(
                        "NOTHING DETACHED — that clip is not grouped with another".into(),
                    );
                }
            }
            (Some(_), None) => {
                self.notify_user("NOTHING DETACHED — click the take to take apart first".into())
            }
            (None, _) => {}
        }
        cx.notify();
    }

    /// Puts the selected clip back in a group with the clip covering exactly the
    /// same frames on another track -- the way back from [`Player::detach`], and
    /// the way to group a picture with sound it was never opened with. The
    /// partner is not clicked because there is nothing to choose: a group id
    /// names one span, so only a clip covering these very frames could join it,
    /// and the engine words what to do when none does.
    fn group(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let partner = match (&self.session, self.selected) {
            (Some(session), Some((lane, idx))) => span_partner(session, lane, idx),
            _ => None,
        };
        match (&mut self.session, self.selected, partner) {
            (Some(session), Some((lane, idx)), Some((other, o_idx))) => {
                if let Err(e) = session.group(lane, idx, other, o_idx) {
                    self.notify_user(format!("NOT GROUPED — {e}").into());
                }
            }
            (Some(_), Some(_), None) => {
                self.notify_user(
                    "NOTHING TO GROUP WITH — no clip on another track covers exactly these frames"
                        .into(),
                )
            }
            (Some(_), None, _) => {
                self.notify_user("NOTHING GROUPED — click one of the halves first".into())
            }
            (None, ..) => {}
        }
        cx.notify();
    }

    /// Drops the selected clip and closes the hole: a whole take goes, both
    /// lanes of it, and everything after it moves up. A half with no take under
    /// it in the video lane -- what a lift leaves behind -- has nothing to
    /// ripple, so that one is lifted instead. The engine reseeks itself, so all
    /// this owes is the flag reset.
    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let selected = self.selected.take();
        // Whichever lane it was clicked in: the index is that lane's own, and
        // the ripple cuts the clip's span out of every lane -- a group covers
        // one span, so deleting a take by its audio half is the same edit as by
        // its picture. What is not a whole take is lifted instead, which is what
        // reaches a clip on an added track ([`whole_take`]).
        let deleted = match (&mut self.session, selected) {
            (Some(session), Some((lane, idx))) => match whole_take(session, lane, idx) {
                true => session.delete_clip(lane, idx),
                false => session.lift_clip(lane, idx),
            },
            _ => false,
        };
        if selected.is_some() && !deleted {
            self.notify_user("NOTHING DELETED — that clip is no longer there".into());
        }
        if deleted {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Lifts the selected half out and leaves the hole: black picture there if
    /// it was the video lane, silence if it was the audio one, and nothing else
    /// moves. What Delete is not.
    fn lift_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match (&mut self.session, self.selected.take()) {
            (Some(session), Some((lane, idx))) => {
                if session.lift_clip(lane, idx) {
                    self.reset_after_reseek();
                } else {
                    self.notify_user("NOTHING LIFTED — that half is no longer there".into());
                }
            }
            (Some(_), None) => {
                self.notify_user("NOTHING LIFTED — click the half to remove first".into())
            }
            (None, _) => {}
        }
        cx.notify();
    }

    /// Copies the selected clip. Nothing on screen changes, so no notify.
    fn copy_selected(&mut self) {
        let session = self.session.as_ref();
        // Out of the lane it was clicked in: the audio half of a group is a
        // different clip from the video one, and copying the wrong lane's
        // frames is a paste of the wrong thing.
        if let Some(clip) = self
            .selected
            .and_then(|(lane, idx)| session?.lane_clips(lane).get(idx).copied())
        {
            self.clipboard = Some(clip);
        }
    }

    /// Starts a peak decode -- and a stream probe -- for every source that has
    /// arrived since the last
    /// repaint. One call from the render rather than three at the doors,
    /// because argv, an import and a project load are all doors and only this
    /// one is guaranteed to run after each of them.
    ///
    /// The decode itself runs on a background thread, like the file chooser:
    /// whole-file audio decode is ~1 s for a half-hour source, and on the render
    /// path that is the window not painting for a second. The lane draws a bed
    /// meanwhile and the repaint comes with the peaks. The entry is written
    /// *before* the spawn, so the sixty repaints that happen while a decode runs
    /// start no further ones.
    fn cache_media(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        // How big each still is, for the row that has to say so. Inline, unlike
        // the two below: an image header is a few bytes off the front of the
        // file, where a sample table is a parse and a decode is a second.
        for path in unseen_paths(session.sources(), &self.sizes) {
            let size = engine::is_image(&path)
                .then(|| engine::image_size(&path).ok())
                .flatten();
            self.sizes.insert(path, size);
        }
        // Which audio streams each file has, for the library's rows. Header
        // only, but a big file's sample tables are not free to parse, so it
        // goes off the render thread like the peaks do.
        for path in unseen_paths(session.sources(), &self.streams) {
            self.streams.insert(path.clone(), Vec::new());
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::AudioSession::probe_streams(&path).unwrap_or_default() }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.streams.insert(path, probed);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // What each file is coded at, for the card that says so. Header and
        // sample table only, but a Matroska indexes no samples and its open
        // walks every cluster header -- 6.7 s on a 12.9 GB film -- so this of
        // all of them cannot be on the render thread.
        for path in unseen_paths(session.sources(), &self.bitrates) {
            self.bitrates.insert(path.clone(), None);
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::probe_bitrate(&path) }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.bitrates.insert(path, Some(probed));
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // Where this file's own groups of pictures begin, for the cut that
        // wants to land on one ([`Player::sync_frames`]). The heaviest probe
        // here -- a Matroska's whole cluster walk, seconds on a film -- and the
        // one nothing waits for: until it answers, the snap is the clip-edge
        // snap it always was.
        for path in unseen_paths(session.sources(), &self.syncs) {
            self.syncs.insert(path.clone(), Vec::new());
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::demux::sync_points(&path) }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.syncs.insert(path, probed);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // Which decoder each file will run on, for the row that says so before
        // a frame of it plays. Off the render thread like the streams above: a
        // stream the plugin takes costs one VA-API init (~90 ms) to answer.
        for path in unseen_paths(session.sources(), &self.decoders) {
            self.decoders.insert(path.clone(), None);
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                // A song and a source no decoder here takes are both `None`:
                // the row says nothing about them rather than guessing, and
                // import refused the second at the door anyway.
                async move { engine::decode::probe(&path).ok() }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.decoders.insert(path, probed);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        for key in unseen_sources(session.sources(), &self.waves) {
            self.waves.insert(key.clone(), Wave::Loading);
            let decoded = cx.background_executor().spawn({
                let (path, stream) = key.clone();
                async move {
                    engine::waveform::peaks(&path, stream, WAVE_BPS)
                        .map(|peaks| peaks.map(|peaks| Arc::new(normalise(peaks))))
                        .inspect_err(|e| eprintln!("waveform: {}: {e}", path.display()))
                }
            });
            cx.spawn(async move |this, cx| {
                let decoded = decoded.await;
                this.update(cx, |this, cx| {
                    this.waves.insert(
                        key,
                        match decoded {
                            Ok(Some(peaks)) => Wave::Peaks(peaks),
                            // No audio track: an answer, and not worth asking
                            // about again.
                            Ok(None) => Wave::Silent,
                            // A file whose sound we could not read is not a
                            // silent one, and a lane that drew it as silent is
                            // how a broken decode passes for a design choice.
                            Err(_) => Wave::Failed,
                        },
                    );
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Probes what an export would open, once per (settings, resolution, cuts)
    /// and only while the export card is up -- it opens the very VA-API encoder
    /// the export would and asks [`engine::export::planned_seats`] the very
    /// question the export asks itself, which is what makes the card's line a
    /// measurement instead of a promise, and also what makes it too slow for
    /// the render thread. Written before the spawn, like the probes above, so
    /// the repaints during it start no second one.
    ///
    /// The cuts are in the key because they are in the answer: moving one onto
    /// a sync point is exactly what turns "SW encode" into "copy", and a card
    /// that kept the old line would be lying about the file it is about to
    /// write.
    fn cache_export_seat(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let settings =
            export_settings(self.quality, self.custom_mbps, self.format, self.audio_kbps);
        if !self.export_open {
            return;
        }
        // A format with no picture has no seat to probe -- and the *last*
        // format's is not its answer: cleared rather than left standing, or
        // picking MP3 after AV1 would read "SW encode (rav1e) · MP3 · SW
        // (rusty_mp3)", which names an encoder that will not run.
        if !settings.format.has_video() {
            self.export_seat = None;
            return;
        }
        // The timeline an export would be started with, owned so the probe can
        // run on a worker -- and the clips beside it, which are what tells this
        // that the question has changed.
        let (project, meta) = session.export_snapshot();
        let clips: Vec<Clip> = session
            .lanes()
            .into_iter()
            .flat_map(|lane| session.lane_clips(lane).to_vec())
            .collect();
        // Cloned rather than copied: the settings carry the picked subtitle
        // rows, which is a `Vec` ([`engine::export::ExportSettings`]).
        let key = (settings.clone(), (meta.width, meta.height), clips);
        if self
            .export_seat
            .as_ref()
            .is_some_and(|(asked, size, cuts, _)| (asked, size, cuts) == (&key.0, &key.1, &key.2))
        {
            return;
        }
        self.export_seat = Some((key.0.clone(), key.1, key.2.clone(), None));
        let probed = cx.background_executor().spawn(async move {
            engine::export::planned_seats(&project, &meta, &settings)
        });
        cx.spawn(async move |this, cx| {
            let probed = probed.await;
            this.update(cx, |this, cx| {
                // Only if the card is still asking the same question: a format
                // changed while the plugin opened has a probe of its own.
                if let Some(seat) = this.export_seat.as_mut().filter(|(asked, size, cuts, _)| {
                    (asked, size, cuts) == (&key.0, &key.1, &key.2)
                }) {
                    seat.3 = Some(probed);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asks the plugin what this machine's GPU can do, once, the first time the
    /// export card is up to show it. Off the render thread for `cache_export_seat`'s
    /// reason: the plugin initialises VA-API to answer, and a driver that is
    /// slow to load must not be a frame the user waits for.
    fn cache_hw_caps(&mut self, cx: &mut Context<Self>) {
        if !self.export_open || self.hw_caps.is_some() {
            return;
        }
        // Written before the spawn, exactly as the probes above are, so the
        // repaints during it start no second one.
        self.hw_caps = Some("asking the driver…".into());
        let asked = cx
            .background_executor()
            .spawn(async move { engine::caps::hardware() });
        cx.spawn(async move |this, cx| {
            let line = asked.await;
            this.update(cx, |this, cx| {
                this.hw_caps = Some(line.into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The group id of the clicked clip, which is what marks the other half.
    fn selected_link(&self) -> Option<u32> {
        let (lane, idx) = self.selected?;
        self.session.as_ref()?.lane_clips(lane).get(idx)?.link
    }

    /// Drops the copied clip in at the playhead. The engine reseeks itself, so
    /// like a delete this owes the flag reset -- and the selection, whose index
    /// the insert has just moved.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let pasted = match (&mut self.session, self.clipboard) {
            (Some(session), Some(clip)) => session.paste_at(session.now(), clip),
            _ => false,
        };
        if pasted {
            self.selected = None;
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// A clip let go at window `x` over lane `to`: it lands with its head where
    /// the hand is carrying it ([`Player::drop_frame`]), on the track it was
    /// dropped on, taking its whole take with it -- one undo step for the
    /// gesture. Dropped back where it was picked up it is not an edit at all, so
    /// nothing is said about it. The engine reseeks, so all this owes is the
    /// flag reset -- and the selection, whose index was that lane's own and now
    /// names a different clip there.
    fn move_clip(&mut self, from: Lane, idx: usize, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let (Some((start, _)), Some(was)) = (
            self.drop_frame(from, idx, x),
            self.session
                .as_ref()
                .and_then(|session| session.lane_clips(from).get(idx).map(|c| c.start)),
        ) else {
            return;
        };
        let moved = self
            .session
            .as_mut()
            .is_some_and(|session| session.move_clip_to(from, idx, to, start));
        let (kind, lanes) = match from.kind {
            LaneKind::Video => ("picture", "video"),
            LaneKind::Audio => ("sound", "audio"),
        };
        match moved {
            true => {
                self.selected = None;
                self.reset_after_reseek();
            }
            // The three ways a drag is refused, told apart by what the
            // front-end already knows: a lane's kind, and where the clip was.
            // Everything else that could refuse (a clip that is not there)
            // cannot be dragged.
            false if from.kind != to.kind => {
                self.notify_user(
                    format!(
                        "NOT ON {} — that is a {kind} clip; drop it on a {lanes} lane",
                        to.label()
                    )
                    .into(),
                )
            }
            // Picked up and put back down where it was: a click, and a click
            // says nothing.
            false if from == to && start == was => {}
            false => {
                self.notify_user(
                    format!(
                        "NOT MOVED — another clip already covers those frames on {}",
                        to.label()
                    )
                    .into(),
                )
            }
        }
        cx.notify();
    }

    /// Where a clip let go at window `x` over lane `to` wants its head: the
    /// frame under the pointer, less however far into the box the hand grabbed
    /// it (so the clip does not jump under the pointer), pulled onto a
    /// neighbouring edge when it lands within [`SNAP_PX`] of one. `None` when
    /// there is no such clip to move. The engine has the last word on where it
    /// may actually go -- this is the ask, not the answer.
    ///
    /// corner-cut: the bed now runs past the last frame whenever the timeline is
    /// shorter than the view ([`Scale::time_at`] clamps at the head only), so a
    /// clip *can* be dragged out there. Zoomed in against the far end it cannot:
    /// the scroll clamp pins the bed's right edge to the duration, and the
    /// pointer has no pixel past it. The upgrade is to let the scroll clamp
    /// leave a screen of empty bed after the end, the way every NLE does.
    fn drop_frame(&self, from: Lane, idx: usize, x: Pixels) -> Option<(u32, Option<u32>)> {
        let clip = self.session.as_ref()?.lane_clips(from).get(idx).copied()?;
        let marks = self.snap_targets(Some((from, idx)));
        Some(landing(
            self.frame_under(x),
            self.grab,
            clip.frames(),
            self.snap,
            self.snap_frames(),
            &marks,
        ))
    }

    /// The same answer for a library row on its way down: nothing is in the hand
    /// yet, so there is no grab offset to take off and no length to snap by --
    /// the file's own is not known until the engine has placed it -- and only
    /// its head lands. Asked by the line, by the ghost and by the drop itself
    /// ([`Player::insert_source`]), so all three name one frame.
    fn place_frame(&self, x: Pixels) -> (u32, Option<u32>) {
        let marks = self.snap_targets(None);
        landing(
            self.frame_under(x),
            0,
            0,
            self.snap,
            self.snap_frames(),
            &marks,
        )
    }

    /// Which index the clip in the hand is at *now*: [`live_idx`] against the
    /// lane the drag named, since a stroke during the gesture moves the indices
    /// gpui froze into the payload. Both halves of a drag ask it -- the line
    /// drawn in flight and the drop that commits -- so the promise and the
    /// landing are made about one clip.
    fn dragged(&self, drag: &ClipDrag) -> Option<usize> {
        let session = self.session.as_ref()?;
        live_idx(session.lane_clips(drag.lane), drag.idx, drag.clip)
    }

    /// The line while the clip is still in the hand: the very answer
    /// [`Player::drop_frame`] will commit, worked out on every move of the drag,
    /// so what the eye was promised is where the release puts it. A pointer that
    /// has wandered off the bed promises nothing.
    fn preview_drop(&mut self, from: Lane, idx: usize, x: Pixels, cx: &mut Context<Self>) {
        let cue = self.drop_frame(from, idx, x).and_then(|(_, cue)| cue);
        self.set_cue(cue, x, cx);
    }

    /// The same line for a library row on its way to a lane: it goes down at
    /// the frame it is let go on ([`Player::place_frame`]), so that frame is
    /// what snaps and what is drawn.
    fn preview_place(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let cue = self.place_frame(x).1;
        self.set_cue(cue, x, cx);
    }

    /// The shadow the clip in the hand would fill, on the lane the pointer is
    /// over: its head where [`Player::drop_frame`] says the release will put it
    /// -- the same call the drop makes, so the box drawn and the box committed
    /// are one answer -- and its own length at this zoom. A lane of the other
    /// kind refuses the drop ([`Project::move_clip`]), and the shadow says so
    /// before the release does.
    fn preview_ghost(&mut self, drag: &ClipDrag, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        let ghost = self
            .dragged(drag)
            .and_then(|idx| self.drop_frame(drag.lane, idx, x))
            .map(|(start, _)| Ghost {
                lane: to,
                start,
                frames: drag.clip.frames(),
                tint: self.clip_tint(drag.clip.source),
                refused: drag.lane.kind != to.kind,
            });
        self.set_ghost(ghost, cx);
    }

    /// The line the track in the hand would drop into, on the row the pointer
    /// is over: at that row's top edge when the header is coming up from below
    /// and at its bottom edge when it is going down, which is the slot
    /// [`Player::reorder_lane`] commits to at the release. Nothing at all over
    /// its own row, where a release changes nothing.
    fn preview_lane_drop(&mut self, from: Lane, onto: Lane, cx: &mut Context<Self>) {
        let lanes = self
            .session
            .as_ref()
            .map_or_else(Vec::new, PlaybackSession::lanes);
        let at = |lane: Lane| lanes.iter().position(|&l| l == lane);
        let next = match (at(from), at(onto)) {
            (Some(i), Some(j)) if i != j => Some(LaneDrop {
                lane: onto,
                above: j < i,
            }),
            _ => None,
        };
        // Only when it has actually changed: a drag move fires on every painted
        // frame, and a redraw per frame that draws the same line is a redraw
        // for nothing.
        if self.lane_drop != next {
            self.lane_drop = next;
            cx.notify();
        }
    }

    /// The line taken back down again, by the row that drew it and by no other:
    /// the pointer has been carried off `lane`, so the slot it was promising is
    /// no longer the one a release would commit to.
    fn forget_lane_drop(&mut self, lane: Lane, cx: &mut Context<Self>) {
        if self.lane_drop.is_some_and(|d| d.lane == lane) {
            self.lane_drop = None;
            cx.notify();
        }
    }

    /// The same shadow for a library row: its head at [`Player::place_frame`],
    /// which is where the drop inserts it, and the file's own length for its
    /// width -- the length the library row already reports. A file this lane
    /// cannot hold ([`lane_refuses`]) is tinted as refused, which is the answer
    /// the release would give in words.
    fn preview_ghost_asset(&mut self, path: &Path, to: Lane, x: Pixels, cx: &mut Context<Self>) {
        let ghost = Ghost {
            lane: to,
            start: self.place_frame(x).0,
            frames: self
                .session
                .as_ref()
                .map_or(0, |session| session.file_frames(path)),
            // A path with no source entry has no colour of its own, and the
            // shadow wears the lane's own instead of borrowing another file's.
            tint: file_tint(self.sources(), path).unwrap_or(BG_RAISED()),
            refused: lane_refuses(path, to).is_some(),
        };
        self.set_ghost(Some(ghost), cx);
    }

    /// Sets the shadow, or takes it away, repainting only when it moved -- the
    /// listeners below run it on every pointer sample of a drag. Cleared by the
    /// root and set again by the lane under the pointer, in that order (gpui
    /// runs the capture phase parent-first), so a pointer over no lane at all
    /// leaves nothing drawn.
    fn set_ghost(&mut self, ghost: Option<Ghost>, cx: &mut Context<Self>) {
        if ghost != self.ghost {
            self.ghost = ghost;
            cx.notify();
        }
    }

    /// The swatch a clip from source `n` wears: [`source_tint`] over the first
    /// source entry naming that *file*, since two audio streams of one file are
    /// two sources and one colour. Every box on a lane and every ghost a drag
    /// draws asks this, so the shadow is recognisably the thing in the hand.
    fn clip_tint(&self, source: usize) -> u32 {
        self.sources()
            .get(source)
            .and_then(|entry| file_tint(self.sources(), &entry.path))
            .unwrap_or_else(|| source_tint(source))
    }

    fn sources(&self) -> &[Source] {
        self.session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources)
    }

    /// Sets the line, or takes it away, and repaints only when it moved: a
    /// pointer dragged off the bed (up to the library, say) is not promising a
    /// landing any more.
    fn set_cue(&mut self, cue: Option<u32>, x: Pixels, cx: &mut Context<Self>) {
        let bed = self.ruler.get();
        let cue = cue.filter(|_| x >= bed.left() && x <= bed.right());
        if cue != self.snap_cue {
            self.snap_cue = cue;
            cx.notify();
        }
    }

    /// Every edge this timeline offers a gesture: [`snap_marks`] over all of its
    /// lanes, so a clip meets a take one track over as readily as one beside it.
    fn snap_targets(&self, skip: Option<(Lane, usize)>) -> Vec<u32> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let lanes = session.lanes();
        let clips: Vec<&[Clip]> = lanes.iter().map(|&lane| session.lane_clips(lane)).collect();
        let skip = skip.and_then(|(lane, idx)| Some((lanes.iter().position(|&l| l == lane)?, idx)));
        snap_marks(&clips, skip, frame_at(session.now(), self.fps))
    }

    /// Where a gesture at `raw` lands and the mark that pulled it there, with
    /// the switch honoured: snapping off, nothing moves and no line is drawn.
    fn snap_to(&self, raw: u32, len: u32, marks: &[u32]) -> (u32, Option<u32>) {
        snap_cue(self.snap, raw, len, self.snap_frames(), marks)
    }

    /// Every timeline frame that is a *source* sync point: each clip's own
    /// grid ([`Player::syncs`]), moved onto the frames the clip plays it at.
    /// Ascending, because the clips are and each grid is.
    ///
    /// This is the difference between an export that copies its picture and one
    /// that decodes and re-codes every frame of a feature film. A cut anywhere
    /// else leaves the copy path with a region that begins between two sync
    /// points -- pictures whose references are not in the file -- and the whole
    /// export falls back to the encoder ([`engine::export`] states the rule).
    ///
    /// Only clips at their own speed, and only video lanes: a re-timed clip is
    /// resampled pictures, which is not a copy at any cut, and a sound lane has
    /// no groups of pictures to begin with.
    fn sync_frames(&self) -> Vec<u32> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let sources = session.sources();
        let mut marks = Vec::new();
        for lane in session.lanes() {
            if lane.kind != LaneKind::Video {
                continue;
            }
            for clip in session.lane_clips(lane) {
                let Some(keys) = sources
                    .get(clip.source)
                    .and_then(|entry| self.syncs.get(&entry.path))
                    .filter(|_| clip.speed.is_normal())
                else {
                    continue;
                };
                marks.extend(
                    keys.iter()
                        .filter(|&&key| key >= clip.in_frame && key < clip.out_frame)
                        .map(|&key| clip.start + (key - clip.in_frame)),
                );
            }
        }
        marks.sort_unstable();
        marks
    }

    /// The frame a cut asked for at `raw` really lands on: the nearest source
    /// sync point within the snap's own tolerance, or `raw` itself where the
    /// magnet is off, where nothing is near enough, or where the source has no
    /// grid to offer (the walk has not answered yet, or the file is not one
    /// this project can copy at all).
    ///
    /// The same tolerance the clip-edge snap uses, so one switch and one
    /// distance govern every landing on this timeline.
    fn cut_frame(&self, raw: u32) -> u32 {
        if !self.snap {
            return raw;
        }
        let tol = self.snap_frames();
        self.sync_frames()
            .into_iter()
            .filter(|mark| mark.abs_diff(raw) <= tol)
            .min_by_key(|mark| mark.abs_diff(raw))
            .unwrap_or(raw)
    }

    /// Whether the playhead is standing exactly on one: what the timeline's own
    /// line says out loud, so "a cut here is copied" is on screen before the cut
    /// rather than discovered in the export card afterwards.
    ///
    /// Asked every repaint, so it walks the *playhead* into each clip's source
    /// and looks it up in that source's own sorted grid -- where
    /// [`Player::sync_frames`] builds the whole list, which is a film's worth of
    /// marks to allocate and sort sixty times a second.
    fn on_sync_point(&self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let now = frame_at(session.now(), self.fps);
        let sources = session.sources();
        session.lanes().into_iter().any(|lane| {
            lane.kind == LaneKind::Video
                && session.lane_clips(lane).iter().any(|clip| {
                    clip.speed.is_normal()
                        && (clip.start..clip.start + (clip.out_frame - clip.in_frame))
                            .contains(&now)
                        && sources
                            .get(clip.source)
                            .and_then(|entry| self.syncs.get(&entry.path))
                            .is_some_and(|keys| {
                                keys.binary_search(&(clip.in_frame + (now - clip.start))).is_ok()
                            })
                })
        })
    }

    /// Puts the playhead on the sync point before or after it -- the keyboard's
    /// half of placing a cut where the export can copy it, and the only way to
    /// reach one exactly on a timeline zoomed out to a whole film, where one
    /// pixel is seconds.
    fn jump_sync(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let now = frame_at(session.now(), self.fps);
        let marks = self.sync_frames();
        let mark = match forward {
            true => marks.iter().find(|&&mark| mark > now).copied(),
            false => marks.iter().rev().find(|&&mark| mark < now).copied(),
        };
        match mark {
            Some(mark) => self.seek(f64::from(mark) / self.fps, cx),
            // Said rather than swallowed: the two most likely reasons are a walk
            // that has not answered yet and a source with no grid at all, and a
            // key that does nothing looks broken either way.
            None => self.notify_user(match marks.is_empty() {
                true => "NO SYNC POINTS — this source has no keyframe grid to jump by (or it is \
                         still being read)"
                    .into(),
                false => "NO SYNC POINT THAT WAY — the playhead is past the last one".into(),
            }),
        }
    }

    /// [`SNAP_PX`] in timeline frames at the scale the bed is drawn at: the bed's
    /// own width drops out of it, since a pixel is now worth the same stretch of
    /// timeline wherever the view sits.
    fn snap_frames(&self) -> u32 {
        self.scale.snap_frames(self.fps)
    }

    /// Opens the clip menu on the box under the pointer, from the right button
    /// wherever it was pressed on that box -- its middle or one of its edge
    /// strips, which cover the middle's own listener. Selecting first is part of
    /// it: every item acts on the clip the menu names.
    fn open_menu(&mut self, lane: Lane, idx: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.modal() {
            return;
        }
        self.select((lane, idx), cx);
        self.context_menu = Some(ContextMenu {
            lane,
            idx,
            at,
            details: false,
        });
        cx.notify();
    }

    /// A press on a clip's edge: the start of the drag that changes how much of
    /// its source it plays. It selects the clip as a press anywhere else on the
    /// box does -- the edge strip covers the box's own listener (`occlude`), so
    /// this is the only one that fires there.
    fn start_trim(&mut self, lane: Lane, idx: usize, edge: Edge, cx: &mut Context<Self>) {
        if self.modal() || self.exporting().is_some() {
            return;
        }
        let Some(clip) = self
            .session
            .as_ref()
            .and_then(|session| session.lane_clips(lane).get(idx).copied())
        else {
            return;
        };
        self.select((lane, idx), cx);
        self.trim = Some(Trim {
            lane,
            idx,
            edge,
            // Where the edge already is: a press that never moves is not an
            // edit, and `Project::trim` refuses exactly that.
            to: match edge {
                Edge::Start => clip.start,
                Edge::End => clip.end(),
            },
            link: clip.link,
        });
        cx.notify();
    }

    /// Where the pointer has pulled the edge to, clamped to the room the engine
    /// says that edge has. Along the same bed the ruler is measured on and
    /// against the same duration the boxes are drawn to, so the edge tracks the
    /// pointer exactly.
    fn trim_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(trim) = self.trim else {
            return;
        };
        // The edge is pulled onto the same marks a whole clip is, by itself:
        // there is no other end travelling with it, so it snaps at length zero.
        let marks = self.snap_targets(Some((trim.lane, trim.idx)));
        let (at, cue) = self.snap_to(self.frame_under(x), 0, &marks);
        let Some((lo, hi)) = self
            .session
            .as_ref()
            .and_then(|session| session.trim_room(trim.lane, trim.idx, trim.edge))
        else {
            return;
        };
        let to = at.clamp(lo, hi);
        // The line only stands where the edge actually stopped: a mark the
        // engine's own room clamped away was never reached.
        self.set_cue(cue.filter(|_| to == at), x, cx);
        self.trim = Some(Trim { to, ..trim });
        cx.notify();
    }

    /// The timeline frame a pointer at window x is on: along the same bed the
    /// ruler is measured on, through the same [`Scale`] every box is drawn
    /// through, so a zoomed-in panel answers with the frame under the pointer
    /// and not with the one that would have been there unzoomed. The one
    /// question a trim, a grab and a drop all ask.
    fn frame_under(&self, x: Pixels) -> u32 {
        frame_at(
            self.scale.time_at(px_along(x, self.ruler.get())),
            self.fps,
        )
    }

    /// The release: the whole drag reaches the engine as one edit, so it is one
    /// undo step. The selection survives it -- a trim inserts and removes
    /// nothing, so every index a lane had still names the clip it named.
    fn commit_trim(&mut self, cx: &mut Context<Self>) {
        let Some(trim) = self.trim.take() else {
            return;
        };
        let trimmed = self
            .session
            .as_mut()
            .is_some_and(|session| session.trim_clip(trim.lane, trim.idx, trim.edge, trim.to));
        if trimmed {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The clip as the drag is showing it: an edge under the pointer moves its
    /// own box, and the boxes of the halves linked to it, before anything is
    /// committed. Display only -- the project is not touched until the release.
    fn trimmed(&self, lane: Lane, idx: usize, clip: Clip) -> Clip {
        let Some(trim) = self.trim.filter(|t| {
            (t.lane, t.idx) == (lane, idx) || (t.link.is_some() && t.link == clip.link)
        }) else {
            return clip;
        };
        let still = self.session.as_ref().is_some_and(|session| {
            session
                .sources()
                .get(clip.source)
                .is_some_and(|s| engine::is_image(&s.path))
        });
        trimmed_clip(clip, trim.edge, trim.to, still)
    }

    /// How long the timeline is *drawn* as: its own length, and while a tail is
    /// being dragged the furthest that tail may reach. A bed that ends exactly
    /// at the last frame has nowhere to put a pointer that means "longer", so
    /// without this the last clip on the timeline could be pulled in and never
    /// let back out.
    ///
    /// Scroll room only, now that a second is an absolute number of pixels
    /// ([`Scale`]): the extra length loosens [`View::settled`]'s clamp, which is
    /// where the pixels past the last frame come from, and moves no box by a
    /// pixel. It is still the *only* headroom at the tail -- zoomed in against
    /// the end, that clamp pins the bed's right edge to the duration and an
    /// End-trim of the last clip would have nowhere to be dragged to. What it
    /// must not do is be read as a length anyone is told: the timecode reads
    /// `PlaybackSession::timeline_duration` for exactly that reason.
    fn drawn_duration(&self) -> f64 {
        let Some(session) = &self.session else {
            return 0.;
        };
        let duration = session.timeline_duration();
        match self.trim {
            Some(trim) if trim.edge == Edge::End => {
                let (_, hi) = session
                    .trim_room(trim.lane, trim.idx, trim.edge)
                    .unwrap_or((0, 0));
                duration.max(f64::from(hi) / self.fps)
            }
            _ => duration,
        }
    }

    /// Where the playhead is, as the panel draws it: pinned to the out point
    /// once playback is done, and clamped to the drawn duration otherwise -- a
    /// tail being dragged draws past the timeline it is about to become.
    fn playhead(&self, duration: f64) -> f64 {
        if self.transport() == Transport::Ended {
            duration
        } else {
            self.session
                .as_ref()
                .map_or(0., PlaybackSession::now)
                .clamp(0., duration)
        }
    }

    /// The one way a library row reaches the timeline: the Add button and a row
    /// dragged onto a lane both come here, so there is a single answer to what
    /// "add this source" does. The whole source goes in as one grouped take at
    /// `at` -- the frame the pointer let it go on, or the playhead for the
    /// button, which names no place. It is the same insert a paste makes, so
    /// everything after it moves along rather than being painted over. Reseeks
    /// like every other edit, and drops the timeline's selection with it: the
    /// insert has just moved the indices it pointed at.
    fn insert_source(
        &mut self,
        path: &Path,
        stream: usize,
        onto: Option<Lane>,
        at: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        // The lane the pointer named cannot hold this kind of file: refused by
        // name, in the same words the ghost was tinted by on the way down
        // ([`lane_refuses`]). The Add button names no lane and so is never
        // refused here -- where a file goes when nobody says is the engine's
        // choice, in `place_stream_at`, not one made twice here.
        if let Some(why) = onto.and_then(|lane| lane_refuses(path, lane)) {
            self.notify_user(why.into());
            cx.notify();
            return;
        }
        // The engine's own length for the file, noted when the import took it
        // in: a row that has never been on a lane is placeable at its full
        // length, which is the whole point of an import that only fills the
        // library.
        let fps = self.fps;
        let placed = match &mut self.session {
            // Seconds, because that is what the engine's own door takes: the
            // frame the pointer named goes back through the same rate every box
            // on the bed is drawn at, so it lands on the frame it was let go on
            // rather than a neighbouring one.
            Some(session) => {
                let at = at.map_or_else(|| session.now(), |frame| f64::from(frame) / fps);
                session.place_stream_at(at, path, stream, onto)
            }
            None => Ok(false),
        };
        match placed {
            Ok(true) => {
                self.selected = None;
                self.reset_after_reseek();
            }
            // The engine's own words: a stream that cannot join this timeline
            // says which property disagrees, exactly as a refused import does.
            Err(e) => self.notify_user(format!("NOTHING ADDED — {e}").into()),
            Ok(false) => {
                self.notify_user("NOTHING ADDED — that file could not be placed here".into())
            }
        }
        cx.notify();
    }

    /// Takes a library row's file out of the list, which is the one thing a row
    /// can lose. Refused in the engine's own words while clips still play from
    /// it -- and those words name the lanes holding them, so the refusal says
    /// what to delete first. The list itself is the report that it worked: the
    /// row is gone from it.
    fn remove_source(&mut self, path: &Path, stream: usize, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let removed = self
            .session
            .as_mut()
            .map(|session| session.remove_source(path, stream));
        let text = match removed {
            Some(Ok(idx)) => {
                // The picked row may be the one that just went, and the engine
                // reseeks, so this owes the flag reset like every other edit.
                if self.selected_asset.as_ref() == Some(&(path.to_path_buf(), stream)) {
                    self.selected_asset = None;
                }
                // A copied clip names its source by *index*, and every index
                // past the one that went has just moved down: without this the
                // next paste puts some other file on the timeline.
                self.clipboard = clipboard_after_remove(self.clipboard, idx);
                self.reset_after_reseek();
                // The last row leaves a session naming no file: nothing to
                // play, nothing to save and nothing to show, which is the empty
                // window the editor launches as. The next import scaffolds a
                // fresh timeline from whatever file it is, at that file's own
                // rate -- which is why the session goes rather than lingering
                // on with the gone file's parameters.
                match self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.sources().is_empty())
                {
                    true => {
                        self.close_session();
                        format!(
                            "REMOVED {} — the library is empty; import a file to start again",
                            file_name(path)
                        )
                    }
                    // The undo stack goes with it (`Project::remove_source`):
                    // said here, because a `z` that does nothing afterwards
                    // would otherwise read as a bug.
                    false => format!(
                        "REMOVED {} — there is nothing left to undo",
                        file_name(path)
                    ),
                }
            }
            Some(Err(e)) => format!("NOT REMOVED — {e}"),
            None => "NO TIMELINE — open a file first".to_string(),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// Back to the window the editor launches as: no timeline, no library, no
    /// picture -- and the hint that says to open a file. What removing the last
    /// library row leaves, since a session whose library is empty has nothing
    /// left to be ([`Player::remove_source`]).
    ///
    /// Everything a *loaded project* resets goes here for its reasons (an index
    /// into a timeline that is gone names nothing), plus the three per-file
    /// caches: they are keyed by path, and the next file to arrive fills them
    /// again.
    fn close_session(&mut self) {
        self.session = None;
        // The picture goes with it, or the empty window would keep showing the
        // last frame of a timeline that no longer exists.
        //
        // corner-cut: its atlas tile is not released -- `window.drop_image` wants
        // a `&mut Window` this door has no other reason to take. One tile per
        // emptied library, against one per displayed frame in `pump`; the
        // upgrade path is threading the window through `act_on_row`.
        self.image = None;
        // The drawn cue with it, and its tile for the same reason as above.
        self.sub_image = None;
        self.clipboard = None;
        self.selected = None;
        self.selected_asset = None;
        // The subtitle rows go with the timeline they were on.
        self.sub_track = 0;
        self.context_menu = None;
        self.library_menu = None;
        self.eq_open = None;
        self.color_open = None;
        self.speed_open = None;
        self.close_silence();
        self.waves.clear();
        self.streams.clear();
        self.bitrates.clear();
        self.sizes.clear();
        self.syncs.clear();
        // Scanned off sources that are not in the library any more.
        self.silence_levels.clear();
        // Every gesture in flight, dropped for `reset_after_reseek`'s reason
        // (it drops the trim below): a drag holds a bar, a clip or a band of a
        // timeline that has just stopped existing.
        self.scrubbing = false;
        self.volume_dragging = false;
        self.eq_dragging = false;
        self.speed_dragging = false;
        self.color_dragging = false;
        self.pending_color = None;
        self.pending_speed = None;
        self.displayed = 0;
        self.dropped = 0;
        self.started = None;
        // The empty window's own: no name in the titlebar, nowhere chosen to
        // export or save to yet, and a rate that only keeps the timecode
        // reading in frames until a file brings its own (`main`).
        self.name = NO_FILE.into();
        self.export_path = PathBuf::new();
        self.project_path = PathBuf::new();
        self.fps = 30.;
        // No decoder to wait for a frame from: the hint is what shows. The
        // transport reads `Stopped` from the session being gone, so there is no
        // end-of-stream state left to clear here.
        self.reset_after_reseek();
        self.seek_since = None;
    }

    /// One item of a library row's menu, done. Every one of them closes the
    /// menu first -- the list under it is about to be rebuilt -- except the one
    /// that turns the card over.
    fn act_on_row(&mut self, item: RowItem, cx: &mut Context<Self>) {
        let Some(menu) = self.library_menu.clone() else {
            return;
        };
        match item {
            RowItem::Properties => {
                if let Some(open) = &mut self.library_menu {
                    open.details = true;
                }
            }
            RowItem::Add => {
                self.library_menu = None;
                self.insert_source(&menu.path, menu.stream, None, None, cx);
            }
            RowItem::Remove => {
                self.library_menu = None;
                self.remove_source(&menu.path, menu.stream, cx);
            }
            RowItem::Reveal => {
                self.library_menu = None;
                // Another process starting: off the UI thread, exactly as the
                // export notice's own click starts it.
                cx.background_executor()
                    .spawn(async move { show_in_file_manager(&menu.path) })
                    .detach();
            }
        }
        cx.notify();
    }

    /// The rate and layout the whole timeline's audio is, taken from the stream
    /// of the first source that could have one: what a library row has to match
    /// to be placeable. `None` until that file has been probed, and then nothing
    /// is greyed for it.
    ///
    /// The first source that is *not a still*, which is the rule the engine
    /// holds every import to (`playback::audio_source_of`) -- a picture at the
    /// head of the list (a removal moves indexes) has no stream to describe
    /// anything with.
    fn timeline_audio(&self) -> Option<(u32, u16)> {
        let first = self
            .session
            .as_ref()?
            .sources()
            .iter()
            .find(|s| !engine::is_image(&s.path))?;
        let info = self
            .streams
            .get(&first.path)?
            .iter()
            .find(|s| s.index == first.audio_stream)?;
        Some((info.sample_rate, info.channels))
    }

    /// Queues a file for the library. Nothing is read here: the reading is
    /// [`read_ahead`] on a worker, and [`Player::take_import`] is what finally
    /// touches the timeline, one repaint later and with the pages warm. A drop
    /// is not a key press, so the export guard on the key handler does not
    /// cover it and this checks for itself.
    ///
    /// One file at a time, in arrival order: a drop can carry six and argv can
    /// name more, and six header walks racing over one disk finish no sooner
    /// than six in a row -- while the line above the panel has exactly one file
    /// to name.
    fn import(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.imports.push_back(path.to_path_buf());
        self.start_import(cx);
    }

    /// Starts the worker for the next queued file, if no worker is running.
    /// Called again as each import lands, which is what drains the queue.
    fn start_import(&mut self, cx: &mut Context<Self>) {
        if self.importing.is_some() {
            return;
        }
        let Some(path) = self.imports.pop_front() else {
            return;
        };
        let stage = Arc::new(std::sync::atomic::AtomicU8::new(ImportStage::Header as u8));
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // The fork is made here, once, and carried to the landing: an import is
        // *probed* on the worker and registered from what came back, while the
        // file argv named -- and a file arriving at a window with no timeline to
        // import into -- is *opened* on the worker and handed over whole. None
        // of the three leaves the UI thread anything to read: a cold 24 GB
        // header walk is twenty seconds, and the window keeps painting through
        // all of them.
        let what = arrival(self.opening.as_deref(), &path);
        // The timeline the file will be checked against, taken here because the
        // worker cannot reach the session: two clones and no disk
        // ([`PlaybackSession::import_gate`]). `None` is a window with nothing to
        // import into, which is the fork that opens the file outright.
        let gate = self.session.as_ref().map(PlaybackSession::import_gate);
        let read = cx.background_executor().spawn({
            let (path, stage) = (path.clone(), Arc::clone(&stage));
            async move { open_ahead(what, &path, &stage, gate) }
        });
        let now = Instant::now();
        self.importing = Some(Import {
            path: path.clone(),
            started: now,
            stage,
            seen: ImportStage::Header,
            since: now,
            cancelled: Arc::clone(&cancelled),
        });
        cx.spawn(async move |this, cx| {
            let landed = read.await;
            this.update(cx, |this, cx| {
                this.importing = None;
                // Cancelled while it read: the window was given back at the
                // click and said so then, so what the worker carried is dropped
                // without a second word ([`Player::cancel_import`]).
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                this.take_import(&path, landed, cx);
                // The next one is started by the repaint this notified, which
                // is also what starts the files argv named ([`poll_import`]).
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps the import line's two clocks honest: the elapsed one runs from the
    /// worker, and the stall one from the last time the stage it is naming
    /// actually changed. Sampled here rather than while drawing, for
    /// [`Player::poll_export`]'s reason.
    ///
    /// ...and starts whatever is queued behind it, which is the one place the
    /// files argv named can begin: they are put in the queue before there is a
    /// context to spawn a worker from.
    fn poll_import(&mut self, cx: &mut Context<Self>) {
        match &mut self.importing {
            Some(import) => {
                import.poll();
            }
            None => self.start_import(cx),
        }
    }

    /// The Cancel beside the import line: the window is given back at once and
    /// the file does not land. Everything queued behind it goes too -- a person
    /// who has stopped an import of six dropped files has stopped the six, and
    /// leaving five to start themselves would be the same wait under another
    /// name.
    ///
    /// The read in flight is *not* stopped, for the reason [`Import::cancelled`]
    /// gives, and the notice says as much rather than promising the disk went
    /// quiet.
    fn cancel_import(&mut self, cx: &mut Context<Self>) {
        let Some(import) = self.importing.take() else {
            return;
        };
        import
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let waiting = self.imports.len();
        self.imports.clear();
        let tail = match waiting {
            0 => String::new(),
            n => format!(" — {n} more dropped from the queue"),
        };
        let text = format!(
            "IMPORT CANCELLED: {}{tail} — the read already running finishes unheeded",
            file_name(&import.path)
        );
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Takes a read-ahead file into the library and nowhere else: the timeline
    /// is not touched, and the row is dragged onto a lane when it is wanted
    /// there. Nothing moves, so nothing reseeks; a refusal is shown as the
    /// engine worded it and changes nothing.
    ///
    /// The export guard again, and not for the caller's sake: an export can
    /// have started during the seconds the worker was reading, and a drop
    /// during an export has always been a silent no-op.
    fn take_import(&mut self, path: &std::path::Path, landed: Landed, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // The file argv named is the one file in the queue that is not an
        // import: it *is* the timeline, and the worker has already opened it.
        // All that is left here is to hang everything derived from it off the
        // window -- the clock, the title, where an export and a save go -- and
        // that is arithmetic, not a read.
        let (subs, probe) = match landed {
            Landed::Read(subs, probe) => (subs, probe),
            what => {
                // Only when *this* is the file argv named: a project dropped
                // while that one is still being read must not make it land as
                // an import.
                if self.opening.as_deref() == Some(path) {
                    self.opening = None;
                }
                match what {
                    Landed::Project(opened) => self.install_project(path, opened, cx),
                    Landed::Media(opened, place) => {
                        let text = self.install_media(path, opened, place);
                        eprintln!("{text}");
                        self.notify_user(text.into());
                        cx.notify();
                    }
                    Landed::Read(..) => unreachable!("matched above"),
                }
                // The line a launch has always printed, now printed when the
                // file actually arrives: it is the mark that says the timeline
                // is up, as the window's own appearance is the other one.
                if let Some(meta) = self.session.as_ref().map(PlaybackSession::meta) {
                    println!(
                        "{}: {}x{} @ {:.2} fps, {} samples",
                        path.display(),
                        meta.width,
                        meta.height,
                        meta.frame_rate,
                        meta.frame_count
                    );
                }
                return;
            }
        };
        // An empty window has no library to add to yet: the file opens one, and
        // the timeline under it stays empty, because an import is an import
        // whether or not a session was already up. A file *named at launch* is
        // the other fork -- that one is an open, and it does become the
        // timeline (`main`).
        // A subtitle file is not a source and lands on no lane: it joins the
        // timeline's own list of them, which is what the library's subtitle
        // section shows and what the overlay draws. With no timeline open there
        // is nothing for the cues to be timed against, and it says so.
        if is_subtitle(path) {
            self.take_subtitles(path, subs, cx);
            return;
        }
        // The container was read on the worker and what came back is registered
        // here ([`engine::PlaybackSession::import_probed`]): no header walk, no
        // decoder open, no probe of the timeline's own first source -- the three
        // reads that used to be spent on this thread. A song and a still fork
        // before the demuxer and pay their own small read
        // ([`engine::PlaybackSession::import`]); a window whose timeline went
        // away while the worker read falls to the slow door below, which is the
        // one that can still open one.
        let registered = match (self.session.as_mut(), probe) {
            (Some(session), Some(Ok(probe))) => Some(session.import_probed(path, probe)),
            (Some(_), Some(Err(refused))) => Some(Err(refused)),
            (Some(session), None) => Some(session.import(path)),
            (None, _) => None,
        };
        let text = match registered {
            Some(Ok(_)) => {
                // The file's own subtitle tracks with it, exactly as an open
                // takes them: an import is the other door the same file arrives
                // through. The cues were read on the worker
                // ([`read_ahead`]); what happens here is the push.
                let tail = self
                    .session
                    .as_mut()
                    .and_then(|session| subtitle_tail(session, subs))
                    .unwrap_or_default();
                format!(
                    "IMPORTED {} to the library — drag it onto a lane to place it{tail}",
                    file_name(path)
                )
            }
            // Named, because two files can fail in one launch and the queue now
            // shows both: "No such file or directory" twice over, with nothing
            // saying which file, is two messages that answer nothing.
            Some(Err(e)) => format!("IMPORT FAILED: {} — {e}", file_name(path)),
            None => self.open_media(path, false, subs),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Takes a file as the session an empty window is waiting for. Everything
    /// derived from the media -- the clock, the title, where an export and a
    /// save go -- is set here, exactly as a launch with a file argument sets
    /// it. Paused with its first frame showing, like every other way a timeline
    /// arrives.
    ///
    /// `place` is the difference between the two doors that come here: a file
    /// *opened* is the timeline, one *imported* into an empty window fills the
    /// library and leaves the lanes empty for a drag.
    fn open_media(&mut self, path: &std::path::Path, place: bool, subs: Subs) -> String {
        self.install_media(path, open_session(path, place, subs), place)
    }

    /// The second half of it: everything the window derives from a session that
    /// has already been opened. Split from the open itself because the file
    /// argv named is opened on a worker ([`open_ahead`]) -- the twelve seconds
    /// of a cold header walk are not the UI thread's to spend -- and lands
    /// here, where nothing is read and nothing blocks.
    fn install_media(
        &mut self,
        path: &std::path::Path,
        opened: Result<(PlaybackSession, String), String>,
        place: bool,
    ) -> String {
        match opened {
            Ok((session, subs)) => {
                self.fps = session.meta().frame_rate;
                // Read before the session moves: a file that plays silent says
                // so here or nowhere.
                let silent = audio_notice(&session);
                // A file replaces the one that was open, and track 3 of that one
                // is not track 3 of this.
                self.sub_track = 0;
                self.session = Some(session);
                // A fresh session comes up at full volume; the player's own
                // setting outlives the file, so it is pushed at every new one.
                self.apply_volume();
                // Beside the new file, but still the format the card is set to:
                // opening another clip is not a change of mind about that.
                self.export_path = retarget(&export_path(path), self.format);
                self.project_path = project_path(path);
                self.name = file_name(path).into();
                self.reset_after_reseek();
                let name = file_name(path);
                // The library is filled and the timeline is empty; the only
                // thing that says so is this line, so it says what to do next.
                let what = match place {
                    true => format!("OPENED {name}"),
                    false => {
                        format!("IMPORTED {name} to the library — drag it onto a lane to place it")
                    }
                };
                format!("{what}{}{subs}", silent.unwrap_or_default())
            }
            Err(e) => format!("OPEN FAILED: {e}"),
        }
    }

    /// The Import button: asks the desktop for a path and takes it the same way
    /// a drop would. The chooser is another process and the user may sit in it,
    /// so it runs on a background thread -- blocking here would freeze the
    /// window behind the dialog.
    fn pick_and_import(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let picked = cx
            .background_executor()
            .spawn(async { pick_file("edith — import") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                // One queue, and the fork is made when its worker starts
                // ([`arrival`]): a project replaces the timeline, media joins
                // the library, and neither is read on this thread.
                Ok(Some(path)) => this.import(&path, cx),
                // Cancelled: the user already knows what happened.
                Ok(None) => {}
                Err(text) => {
                    eprintln!("{text}");
                    this.notify_user(text.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The `+ S` button and its key: asks the desktop for a file and takes the
    /// subtitle tracks out of it -- a standalone `.srt`/`.vtt`/`.ass` is one of
    /// them, a Matroska however many are inside. Only the subtitles: the file
    /// itself does not join the library, which is what the Import button beside
    /// this one is for.
    ///
    /// The chooser is another process and the user may sit in it, so it runs on
    /// a background thread, exactly as [`Player::pick_and_import`] does.
    fn pick_and_add_subtitles(&mut self, cx: &mut Context<Self>) {
        // What dims the `+ S` button, asked here as well so the key answers the
        // same question -- and *before* the chooser rather than after it: a door
        // that opens a dialog, waits for a file and only then says the timeline
        // was never there is the second door disagreeing with the first.
        if let Some(why) = self.enable(ActionId::AddSubtitleTrack, None).why() {
            let text = format!("NO SUBTITLES ADDED — {why}");
            eprintln!("{text}");
            self.notify_user(text.into());
            cx.notify();
            return;
        }
        let picked = cx
            .background_executor()
            .spawn(async { pick_file("edith — subtitles to add") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                Ok(Some(path)) => this.add_subtitles(&path, cx),
                // Cancelled: the user already knows what happened.
                Ok(None) => {}
                Err(text) => {
                    eprintln!("{text}");
                    this.notify_user(text.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Takes a file's subtitle tracks onto the timeline, off the render thread.
    /// The walk reads the whole container for its cues
    /// (`engine::PlaybackSession::parse_subtitles`) -- ~200 ms on a two-hour 4K
    /// remux and 1.3 s on a cold 3 GB one -- and a button that costs the window
    /// that many frames is a button that freezes it. So the *parse* is the
    /// worker's, whole, and the UI thread only pushes what came back
    /// ([`PlaybackSession::add_subtitle_tracks`]): no borrow crosses the await,
    /// because the parse is an associated fn that owns nothing.
    fn add_subtitles(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        // Nothing to time the cues against: said now rather than after a walk
        // of a 25 GB file that was never going to be kept.
        if self.session.is_none() {
            self.landed_subtitles(path, None, cx);
            return;
        }
        self.notify_user(format!("READING {} for subtitles…", file_name(path)).into());
        let parsed = cx.background_executor().spawn({
            let path = path.to_path_buf();
            async move { engine::PlaybackSession::parse_subtitles(&path) }
        });
        let path = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let parsed = parsed.await;
            this.update(cx, |this, cx| {
                // The dedupe lives inside the push, so a second `+ S` on the
                // same file still answers 0 and still says so below.
                let added = this
                    .session
                    .as_mut()
                    .map(|session| parsed.and_then(|tracks| pushed(session, &path, tracks)));
                this.landed_subtitles(&path, added, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Every subtitle track a `.srt`/`.vtt`/`.ass` carries, onto the timeline
    /// and nowhere else: they are not clips and land on no lane. The cues came
    /// off the worker that read the file ([`read_ahead`]), like every other
    /// door's do, and what is left here is the push. The engine dedupes by
    /// (file, track), so the same `.srt` twice is one row and says so.
    fn take_subtitles(&mut self, path: &std::path::Path, subs: Subs, cx: &mut Context<Self>) {
        let added = self
            .session
            .as_mut()
            .map(|session| subs.and_then(|tracks| pushed(session, path, tracks)));
        self.landed_subtitles(path, added, cx);
    }

    /// What the timeline says once the tracks are on it, whichever worker did
    /// the reading: the `+ S` button and its key ([`Self::add_subtitles`]), a
    /// dropped or imported subtitle file ([`Self::take_subtitles`]), and a
    /// window with nothing to time cues against all word the outcome here,
    /// once, so no two doors can drift apart.
    fn landed_subtitles(
        &mut self,
        path: &std::path::Path,
        added: Option<engine::Result<usize>>,
        cx: &mut Context<Self>,
    ) {
        let text = match added {
            Some(Ok(0)) => format!(
                "{}'s subtitles are on the timeline already",
                file_name(path)
            ),
            Some(Ok(n)) => format!(
                "SUBTITLES {} — {n} track(s), showing over the picture, {} hides them",
                file_name(path),
                self.keymap.display(ActionId::ToggleSubtitles)
            ),
            Some(Err(e)) => format!("SUBTITLE IMPORT FAILED: {e}"),
            None => "NO SUBTITLES ADDED — open a file for them to run against first".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// The × on a subtitle row, and the stroke that takes the picked one off:
    /// the track leaves the timeline and the pick moves with it. Every index
    /// past the one that went moves down
    /// ([`engine::Project::remove_subtitles`]), so a pick left where it was
    /// would name a *different* track -- and the pick is what an export writes
    /// into the file.
    ///
    /// Not an undo step: subtitles are not on the history's snapshots, so the
    /// way back is putting the file's subtitles on again -- which is a door of
    /// its own ([`Player::pick_and_add_subtitles`]) and reads the subtitles
    /// alone, never the media. The notice says that rather than promising a
    /// ctrl+z that would do nothing.
    fn remove_subtitle_track(&mut self, track: usize, cx: &mut Context<Self>) {
        // The one availability oracle, for the same reason the × on a row and
        // the stroke are one call: an empty list is not a failure, it is an
        // action with nothing to act on, and the engine's "there is no subtitle
        // track 0" is an index nobody typed. A real removal that fails still
        // says what the engine said, below.
        if let Some(why) = self.enable(ActionId::RemoveSubtitleTrack, None).why() {
            let text = format!("NO SUBTITLES REMOVED — {why}");
            eprintln!("{text}");
            self.notify_user(text.into());
            cx.notify();
            return;
        }
        // Read before it goes: a notice naming an index names nothing.
        let name = self
            .session
            .as_ref()
            .and_then(|session| sub_pick_name(session.subtitles(), track))
            .unwrap_or_else(|| format!("subtitle track {track}"));
        let text = match self
            .session
            .as_mut()
            .map(|session| session.remove_subtitles(track))
        {
            Some(Ok(())) => {
                let left = self
                    .session
                    .as_ref()
                    .map_or(0, |session| session.subtitles().len());
                self.sub_track = sub_pick_after_removal(self.sub_track, track, left);
                // The drawn cue is keyed by that index ([`Player::sub_picture`])
                // and the index now stands for another track.
                //
                // corner-cut: its atlas tile is not released -- `close_session`'s
                // note, for its reason and with its upgrade path.
                self.sub_image = None;
                format!(
                    "{name} REMOVED — {} puts a file's subtitles back on, the file itself stays off",
                    self.keymap.display(ActionId::AddSubtitleTrack)
                )
            }
            Some(Err(e)) => format!("NO SUBTITLES REMOVED — {e}"),
            None => "NO SUBTITLES REMOVED — open a file first".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Swaps the whole timeline for one restored from a `.edith`, for
    /// [`Player::install_media`]'s reason: the open is a worker's -- a project
    /// naming a 24 GB film opens that film, which is the same twenty seconds
    /// ([`arrival`] sends every `.edith` through the one queue) -- and this is
    /// what is left once it lands. Nothing is replaced until the new session is
    /// in hand, so a refusal is shown as the engine worded it and leaves what is
    /// playing alone.
    fn install_project(
        &mut self,
        path: &std::path::Path,
        opened: Result<PlaybackSession, String>,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let text = match opened {
            Ok(session) => {
                self.fps = session.meta().frame_rate;
                let silent = audio_notice(&session);
                // A project is named after itself but still exports beside its
                // media: that is the only place an export has ever landed.
                self.export_path = retarget(&export_path(&session.sources()[0].path), self.format);
                self.session = Some(session);
                self.apply_volume();
                self.project_path = path.to_path_buf();
                self.name = file_name(path).into();
                // A copied clip names its source by index, which means a
                // different file -- or none -- in another project.
                self.clipboard = None;
                self.selected = None;
                // A menu can be up while a project is dropped on the window --
                // the scrim swallows clicks, never a drop -- and its index
                // would name some other timeline's clip. The two clip cards
                // hold a (lane, idx) of the old timeline for the same reason.
                self.context_menu = None;
                self.eq_open = None;
                self.color_open = None;
                // Marks are timeline frames of the timeline that was.
                self.close_silence();
                // A different set of sources: the row that was picked is not
                // the file that index names any more -- and neither is the
                // subtitle track that was showing.
                self.selected_asset = None;
                self.sub_track = 0;
                // The counters describe one timeline; the eof line must not
                // report the old one's frames against the new one.
                self.displayed = 0;
                self.dropped = 0;
                self.started = None;
                // Loaded paused at its saved playhead, so the still it owes
                // reaches the screen the way a seek's does. The old picture is
                // released by the swap in `pump`, as after any other seek.
                self.reset_after_reseek();
                format!("LOADED {}{}", file_name(path), silent.unwrap_or_default())
            }
            Err(e) => format!("OPEN FAILED: {e}"),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Writes the timeline back to its project file. Overwrites silently, like
    /// an export: the path was chosen once and the notice is the confirmation.
    fn save_project(&mut self, cx: &mut Context<Self>) {
        let saved = self
            .session
            .as_ref()
            .map(|session| session.save_project(&self.project_path));
        let text = match saved {
            Some(Ok(())) => format!("SAVED {}", file_name(&self.project_path)),
            Some(Err(e)) => format!("SAVE FAILED: {e}"),
            None => "NOTHING TO SAVE — open a file first".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// A new empty track under the ones already there. One undo step in the
    /// engine, so the stroke that takes back an edit takes back a track too, and
    /// no reseek: nothing plays differently until something is dropped on it.
    /// The selection stays -- the lanes it indexes into have not moved.
    fn add_lane(&mut self, kind: LaneKind, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match &mut self.session {
            Some(session) => {
                let lane = session.add_lane(kind);
                self.notify_user(
                    format!(
                        "{} ADDED — drag a clip onto it, {} takes it back",
                        lane.label(),
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            None => self.notify_user("NO TRACK ADDED — open a file first".into()),
        }
        cx.notify();
    }

    /// The × in a track's header: the add taken back, one undo step, and the
    /// engine's own words when it refuses -- those name the clips still on the
    /// track, so the notice says what to delete first. A removal never deletes a
    /// clip.
    ///
    /// Everything holding a `(lane, idx)` is dropped, because the tracks below
    /// the one that went have just moved up an `ord`
    /// ([`engine::Project::remove_lane`]): a selection or an open card kept
    /// across it would be pointing at the *next* track's clip.
    fn remove_lane(&mut self, lane: Lane, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let removed = self
            .session
            .as_mut()
            .map(|session| session.remove_lane(lane));
        let text = match removed {
            Some(Ok(())) => {
                self.selected = None;
                self.context_menu = None;
                self.eq_open = None;
                self.color_open = None;
                self.speed_open = None;
                self.close_silence();
                format!(
                    "{} REMOVED — {} brings it back",
                    lane.label(),
                    self.keymap.display(ActionId::Undo)
                )
            }
            Some(Err(e)) => format!("NO TRACK REMOVED — {e}"),
            None => "NO TRACK REMOVED — open a file first".to_string(),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// A header let go over another header: the track in the hand takes that
    /// one's place in the stack, clips and all
    /// ([`engine::Project::move_lane`]), one undo step. The gesture every
    /// editor reorders tracks with, and the only way the order is ever changed
    /// -- there is no second list of it to keep in step.
    ///
    /// Display order is the stack, so moving a video track past another video
    /// track changes which picture wins, here and in an export alike; audio is
    /// summed and does not care, which is what makes `A1` above `V1` a purely
    /// visual arrangement. A label is a position among the tracks of its kind,
    /// so a track that crossed one of its own kind comes back under a different
    /// name -- and everything holding a `(lane, idx)` is dropped exactly then,
    /// for [`Player::remove_lane`]'s reason: those handles now name another
    /// track's clip. A move that crossed only the other kind renames nothing
    /// and keeps the selection.
    fn reorder_lane(&mut self, lane: Lane, onto: Lane, cx: &mut Context<Self>) {
        self.lane_drop = None;
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(to) = session.lanes().iter().position(|&l| l == onto) else {
            return;
        };
        // Picked up and put back down where it was is a click, and a click says
        // nothing -- `move_lane` refuses it and every other no-op.
        let Some(moved) = session.move_lane(lane, to) else {
            cx.notify();
            return;
        };
        if moved != lane {
            self.selected = None;
            self.context_menu = None;
            self.eq_open = None;
            self.color_open = None;
            self.speed_open = None;
            self.close_silence();
        }
        self.notify_user(
            format!(
                "{} IS TRACK {} NOW — {} puts it back",
                moved.label(),
                to + 1,
                self.keymap.display(ActionId::Undo)
            )
            .into(),
        );
        cx.notify();
    }

    /// What the remove keys act on: the last track of that kind, which is the
    /// one the matching add key appended. Nothing at all before a file is open,
    /// where the timeline drawn is a placeholder pair.
    fn remove_last_lane(&mut self, kind: LaneKind, cx: &mut Context<Self>) {
        let last = self.session.as_ref().and_then(|session| {
            session
                .lanes()
                .into_iter()
                .filter(|l| l.kind == kind)
                .next_back()
        });
        match last {
            Some(lane) => self.remove_lane(lane, cx),
            None => {
                self.notify_user("NO TRACK REMOVED — open a file first".into());
                cx.notify();
            }
        }
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if self.session.as_mut().is_some_and(PlaybackSession::undo) {
            self.reset_after_reseek();
        }
        self.selected = None;
        cx.notify();
    }

    /// The stroke a waiting row was after: it becomes the whole of what reaches
    /// that action, which is what the row was showing. A chord another action
    /// already holds is refused by the keymap and the row keeps waiting, so the
    /// next stroke is another try rather than a lost one. A binding that took
    /// holds either way:
    /// what a failed write costs is only the next run, which is what the notice
    /// is for.
    fn capture(&mut self, action: ActionId, key: &str, ctrl: bool) {
        let chord = keymap::Chord {
            key: key.to_string(),
            ctrl,
        };
        // Only a stroke the file can spell and read back as itself: gpui reports
        // "+" for shift+=, which is the chord grammar's separator, so binding it
        // would write a line the next load would have to drop. Refused here, in
        // front of the user, rather than silently costing that binding later.
        // The row keeps waiting, as it does for a stroke already taken.
        if !chord.bindable() {
            let text = format!("THAT KEY CANNOT BE BOUND — {}", chord.pretty());
            eprintln!("{text}");
            self.notify_user(text.into());
            return;
        }
        let text = match self.keymap.rebind_action(action, chord.clone()) {
            Ok(()) => {
                self.rebinding = None;
                match self.keymap.save() {
                    Ok(()) => return,
                    Err(e) => format!(
                        "KEYBINDINGS NOT SAVED — {}: {e}",
                        Keymap::config_path().display()
                    ),
                }
            }
            Err(holder) => format!("ALREADY BOUND — {} is {}", chord.pretty(), holder.label()),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
    }

    /// Seeks to where the pointer sits along the ruler. `commit` is the press
    /// and the release, which must land exactly even when the throttle below
    /// would have skipped them.
    fn scrub_to(&mut self, x: Pixels, commit: bool, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        // Clamped to the timeline here rather than in the mapping: there is bed
        // past the last frame now, and a seek out there is a seek to the end.
        let t = self
            .scale
            .time_at(px_along(x, self.ruler.get()))
            .clamp(0., session.timeline_duration());
        let target = (t * self.fps) as u32;
        if commit || scrub_due(target, self.last_target, self.last_scrub.elapsed()) {
            self.last_target = target;
            self.last_scrub = Instant::now();
            self.seek(t, cx);
        }
    }

    /// The play binding and the transport button share it: once the timeline is finished
    /// the only sensible "play" is from the top.
    /// Pushes the current volume at the session, which is the only place it is
    /// ever pushed: after a change here, and after a session arrives. A session
    /// starts at full volume, so a file opened while muted has to be told --
    /// that is the whole reason this is not just called from the key handler.
    /// Silent no-op with no timeline, or with a run that has no audio device.
    fn apply_volume(&self) {
        if let Some(session) = &self.session {
            session.set_gain(self.volume.gain());
        }
    }

    /// The mute key and the two volume keys, and the click on the button. The
    /// picture is not touched: silencing the output is not pausing it, so the
    /// clock -- which the device still drives -- runs straight through.
    fn set_volume(&mut self, change: impl FnOnce(&mut Volume), cx: &mut Context<Self>) {
        change(&mut self.volume);
        self.apply_volume();
        cx.notify();
    }

    /// Where the pointer sits along the slider, as a level. The press and every
    /// sample after it come here, so the sound follows the hand rather than the
    /// release -- there is nothing to undo about a monitoring level, which is
    /// why this writes live and keeps no gesture state beyond the flag.
    fn drag_volume(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let along = frac_along(x, self.volume_bar.get());
        self.set_volume(|volume| volume.set_along(along), cx);
    }

    /// One sample of whatever drag is in the hand: the equalizer's handle, a
    /// clip's edge, a colour bar, the speed bar, the volume slider or the
    /// playhead. Each of those starts on a strip a few pixels wide that the
    /// pointer leaves immediately, so none of them can be tracked from the
    /// element it started on -- the gesture is followed here instead, on a
    /// hitbox that covers everything the hand can reach.
    ///
    /// Registered on the root *and* on the scrim of every card that holds a
    /// slider ([`Player::drag_scrim`]). An occluding sheet ends gpui's hit test
    /// where it sits (`Hitbox::is_hovered`, window.rs:788), so while a card is
    /// up the root is not hovered anywhere under it and hears none of this: the
    /// press set a value and the drag then froze on it.
    fn drag_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A handle is 10 px across and the pointer leaves it at once, so
        // the equalizer drag is tracked here for the ruler's reason.
        if self.eq_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_band(event.position, cx);
            } else {
                // Released outside the window: the up below never came,
                // so this is where the gesture ends -- and it still owes
                // the one write the whole drag is worth.
                self.eq_dragging = false;
                self.commit_eq(cx);
            }
            return;
        }
        // A clip edge is 6 px wide and the pointer leaves it on the
        // first drag, so the gesture is tracked here for the same
        // reason -- and it ends here too when the button came up
        // outside the window, still owing its one edit.
        if self.trim.is_some() {
            match event.pressed_button {
                Some(MouseButton::Left) => self.trim_to(event.position.x, cx),
                _ => self.commit_trim(cx),
            }
            return;
        }
        // A colour slider is 4 px tall and the pointer leaves it just as
        // fast; every sample is live, so the release owes no write of
        // its own -- what the last sample set is what the clip carries.
        if self.color_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_color(event.position.x, false, cx);
            } else {
                // The release happened outside the window, so this is
                // where the gesture ends -- and it may not end on a
                // sample the worker was too busy to take.
                self.color_dragging = false;
                self.flush_drag(cx);
            }
            return;
        }
        // The speed bar, the same 4 px and the same live writes: the
        // press took the undo step and every sample since is live.
        if self.speed_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_speed(event.position.x, false, cx);
            } else {
                self.speed_dragging = false;
                self.flush_drag(cx);
            }
            return;
        }
        // The volume slider, the same live writes: what the hand is on
        // is what the speakers are doing, and there is nothing to undo.
        if self.volume_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_volume(event.position.x, cx);
            } else {
                self.volume_dragging = false;
            }
            return;
        }
        if !self.scrubbing {
            return;
        }
        if event.pressed_button == Some(MouseButton::Left) {
            self.scrub_to(event.position.x, false, cx);
        } else {
            // A release outside the window never reaches the handler
            // below, so the first button-up move is when we learn the
            // drag is over. Without this the next hover would scrub.
            self.scrubbing = false;
        }
    }

    /// Where a drag ends: the release lands exactly, and whatever the gesture
    /// owes -- one undo step for the equalizer and the trim, a flush for the
    /// live-writing bars -- is paid here. On the root and on a card's scrim
    /// both, for [`Player::drag_move`]'s reason: a release over an open card
    /// never reaches the root.
    fn drag_release(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.eq_dragging) {
            // The release lands exactly, then the gesture is written
            // once -- the append-only table's whole reason.
            self.drag_band(event.position, cx);
            self.commit_eq(cx);
            return;
        }
        if self.trim.is_some() {
            // The release lands exactly, then the gesture is
            // written once -- one edit, one undo step.
            self.trim_to(event.position.x, cx);
            self.commit_trim(cx);
            return;
        }
        if std::mem::take(&mut self.color_dragging) {
            // The release lands exactly where the hand let go, and
            // it is a live write like every other sample: the undo
            // step the gesture rolls back to was the press's. The
            // flush is what makes "exactly" true while the worker is
            // still busy -- the sample above would only be held.
            self.drag_color(event.position.x, false, cx);
            self.flush_drag(cx);
            return;
        }
        if std::mem::take(&mut self.speed_dragging) {
            self.drag_speed(event.position.x, false, cx);
            self.flush_drag(cx);
            return;
        }
        if std::mem::take(&mut self.volume_dragging) {
            self.drag_volume(event.position.x, cx);
            return;
        }
        if std::mem::take(&mut self.scrubbing) {
            self.scrub_to(event.position.x, true, cx);
        }
    }

    fn toggle_or_restart(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // Nothing to play is a message, not a transport state. An empty
        // timeline is [`Transport::Ended`] from its one black frame onward, so
        // the restart below would start a clock against a zero-length timeline
        // -- and it is Ended again by the next repaint, so no later press could
        // ever stop it: the button would read "Pause" and never pause. A delete
        // can empty the timeline mid-play, and that press must still stop it.
        if nothing_to_play(self.session.as_ref()) {
            match self.session.as_mut().filter(|s| s.is_playing()) {
                Some(session) => session.pause(),
                None => self.notify_user(NOTHING_TO_PLAY.into()),
            }
            cx.notify();
            return;
        }
        // Pressing play is asking to watch, so a view scrolled away while
        // paused comes back to the head with it -- as a seek's does.
        self.panned = false;
        match self.transport() {
            // Nothing open: the button is dimmed and the key says nothing.
            Transport::Stopped => {}
            // Back to the top and away, for the key and the button alike --
            // whichever asked, the transport was showing Play.
            state if state.restarts() => {
                self.seek(0., cx);
                if let Some(session) = &mut self.session {
                    session.play();
                }
            }
            _ => {
                if let Some(session) = &mut self.session {
                    session.toggle();
                    // A paused timeline animates nothing; this is the repaint
                    // that puts the new glyph up.
                    cx.notify();
                }
            }
        }
    }

    /// The export that owns the UI, if any. A cancelled one does not: it has
    /// its own copy of the edit list and owes only its own cleanup.
    fn exporting(&self) -> Option<&ExportHandle> {
        self.export.as_ref().filter(|_| !self.cancelling)
    }

    /// What the export action does now: opens the card, which is where the
    /// quality, the destination and the decision to write at all are. Nothing
    /// is encoded until the button in it is pressed.
    fn open_export(&mut self, cx: &mut Context<Self>) {
        if self.export.is_some() {
            return;
        }
        // Nothing to write out, and a refusal rather than a card about it: the
        // window is empty and the export path is not even chosen yet.
        if self.session.is_none() {
            self.notify_user("NOTHING TO EXPORT — open a file first".into());
            cx.notify();
            return;
        }
        self.export_open = true;
        // One card at a time, and a waiting row must not outlive the card it
        // was waiting in. Nor may a half-typed number: the card opens on the
        // bitrate it will write, never on digits left behind by a closed one.
        self.keys_open = false;
        self.rebinding = None;
        self.mbps_edit = None;
        cx.notify();
    }

    /// A format row was clicked. The destination follows it at once -- a WAV
    /// written to a path ending in `.mp4` is a file every player will lie
    /// about -- keeping whatever stem the save dialog last left there.
    fn set_format(&mut self, format: Format) {
        // The one door both the row and its initial go through, so a format the
        // card greys out cannot be picked by keyboard either.
        if let Some(why) = self
            .session
            .as_ref()
            .and_then(|session| format_refusal(session, format))
        {
            self.notify_user(format!("NOT {} — {why}", format_label(format)).into());
            return;
        }
        self.format = format;
        self.export_path = retarget(&self.export_path, format);
    }

    /// The container row: the same codec in the other box, which retargets the
    /// destination exactly as picking a codec does -- and does nothing at all
    /// for a codec with only one box, so the stroke cannot invent a choice the
    /// card is not offering.
    fn cycle_container(&mut self) {
        self.set_format(next_container(self.format));
    }

    /// The quality rows by keyboard, wrapping. Refused by name where the format
    /// has no bitrate to pick: a key that silently does nothing is the card
    /// looking broken.
    fn cycle_quality(&mut self) {
        if let Some(why) = bitrate_refusal(self.format) {
            self.notify_user(why.into());
            return;
        }
        let at = Quality::ALL
            .iter()
            .position(|&q| q == self.quality)
            .unwrap_or(0);
        self.quality = Quality::ALL[(at + 1) % Quality::ALL.len()];
    }

    /// The sound's rate by keyboard, wrapping through the offered ones -- the
    /// picture's quality row for the other half of the file. Refused by name
    /// where this timeline in this format has no rate to pick, exactly as
    /// [`Player::cycle_quality`] is: a key that silently does nothing is the
    /// card looking broken.
    fn cycle_audio_kbps(&mut self) {
        if let Some(why) = self.audio_rate_refusal() {
            self.notify_user(why.into());
            return;
        }
        self.audio_kbps = next_audio_kbps(self.audio_kbps);
    }

    /// Why the sound row is not a choice right now, the engine answering about
    /// the very project it would export. No session is the same answer as no
    /// sound: there is nothing to write either way.
    fn audio_rate_refusal(&self) -> Option<&'static str> {
        match &self.session {
            Some(session) => session.audio_rate_refusal(self.format),
            None => Some("no sound to write"),
        }
    }

    /// The custom bitrate by pointer: the typed digits were the only control in
    /// this card a mouse could not reach. Clamped to the range the row states
    /// (the engine's own 1..50 Mbps), and picking the row is part of the step --
    /// a stepper that moves a number nobody is using would move nothing.
    fn nudge_mbps(&mut self, step: i32) {
        self.custom_mbps =
            (self.custom_mbps as i32 + step).clamp(MBPS_MIN as i32, MBPS_MAX as i32) as u32;
        self.quality = Quality::Custom;
    }

    /// The same number under the wheel, one step a notch, up for more: fifty
    /// presses of a stepper is not a way to reach the top of this range, and the
    /// wheel is what this editor already moves a value with (the timeline's
    /// zoom and scroll are the same gesture). Hold-to-run stays the keyboard's,
    /// as it is on every other card here -- a button that repeats while held is
    /// not a thing this program has.
    ///
    /// It moves the *field* while one is open, exactly as ↑↓ do, so the two
    /// ways in never disagree about which number is being changed.
    fn wheel_mbps(&mut self, event: &ScrollWheelEvent) {
        let by = wheel_delta(event);
        if by == 0. {
            return;
        }
        let by = by.signum() as i32;
        match &mut self.mbps_edit {
            Some(edit) => edit.step(by),
            None => self.nudge_mbps(by),
        }
    }

    /// Opens the custom bitrate's field on the number the row is carrying, and
    /// picks the row while it is at it: a field typed into is the row being
    /// chosen, and a number nobody is using would be a number typed at nothing.
    /// Nothing is committed here -- until enter, the card still exports at the
    /// bitrate it had.
    fn edit_mbps(&mut self) {
        self.quality = Quality::Custom;
        self.mbps_edit = Some(NumberEdit::new(self.custom_mbps));
    }

    /// The card's Destination row: the desktop's save dialog, on a background
    /// thread like the import chooser -- the user may sit in it and the window
    /// behind must not freeze. No chooser at all leaves the default path, which
    /// is what the refusal says.
    fn pick_destination(&mut self, cx: &mut Context<Self>) {
        let default = self.export_path.clone();
        let picked = cx
            .background_executor()
            .spawn(async move { pick_save(&default) });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| {
                // The dialog outlives the card: an export started meanwhile
                // took the old path and its notice must name what it wrote.
                if this.export.is_some() {
                    return;
                }
                match picked {
                    // The stem is the user's, the extension is the format's: a
                    // FLAC named `.mp4` is a file every player lies about.
                    Ok(Some(path)) => this.export_path = retarget(&path, this.format),
                    // Cancelled: the default stands, as it did before.
                    Ok(None) => {}
                    Err(text) => {
                        eprintln!("{text}");
                        this.notify_user(text.into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The subtitle tracks an export of this timeline carries: every one with a
    /// cue left in the exported range ([`PlaybackSession::timeline_cues`], the
    /// very map the file is written from), in the library's own order.
    ///
    /// Worked out from the cues each time rather than kept as a pick, which is
    /// what makes it impossible to desync: a row added or taken off shifts every
    /// index after it, and a stored list would then name tracks nobody chose.
    /// `Player::sub_track` stays what it always was -- which track the *overlay*
    /// draws -- and has no say here.
    ///
    /// The honest input and not the final answer: the engine filters it again
    /// per track (a track that could not be read, a picture one) and says so in
    /// the card's own words ([`engine::export::planned_subtitles`]).
    fn export_subs(&self) -> Vec<usize> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        (0..session.subtitles().len())
            .filter(|&i| !session.timeline_cues(i).is_empty())
            .collect()
    }

    /// That list in the card's words ([`subtitle_plan`]): what travels, and the
    /// reason beside every track that does not -- including the ones
    /// [`Self::export_subs`] filtered out before the engine ever saw them.
    fn subtitle_line(&self) -> String {
        let Some(session) = self.session.as_ref() else {
            return "none".to_string();
        };
        let picks = self.export_subs();
        let plan = session.planned_subtitles(self.format, picks.iter().copied());
        match self.format.has_video() {
            true => subtitle_plan(plan, session.subtitles(), &picks),
            // A format that is the sound alone has nowhere to put any of them
            // and the engine says that once, about the file. Naming the cues of
            // each track under it answers a question the format already closed.
            false => plan,
        }
    }

    /// Writes the edit list out, at the settings the card was left at. Playback
    /// stops first: the exporter opens its own decoder -- and, on the hardware
    /// path, an encoder -- so a running player would only compete with it for
    /// the GPU. A cancelled export still winding down holds this off for the
    /// frame it takes to notice, which is what keeps its `remove_file` off the
    /// new output.
    fn start_export(&mut self, cx: &mut Context<Self>) {
        if self.export.is_some() {
            return;
        }
        let mut settings =
            export_settings(self.quality, self.custom_mbps, self.format, self.audio_kbps);
        // Whatever is on the timeline travels -- every track with a cue in the
        // exported range, not the one row the overlay happens to be drawing.
        // Set here rather than inside `export_settings`, which the card also
        // calls for the *estimate* and which nothing else needs a subtitle for.
        settings.subtitles = self.export_subs();
        let Some(session) = &mut self.session else {
            self.notify_user("NOTHING TO EXPORT — open a file first".into());
            cx.notify();
            return;
        };
        // An emptied timeline is a timeline; it is simply not a file. Refused by
        // name here rather than written as a project of no frames -- and the
        // engine refuses it again on the worker (`export::start`), so a caller
        // that is not this button cannot get past it either. Two fences on
        // purpose: this one is the one with a keystroke to blame.
        if session.is_empty() {
            self.notify_user("NOTHING TO EXPORT — the timeline is empty".into());
            cx.notify();
            return;
        }
        // The format row can be refused *after* it was picked -- mp4 is the
        // default and an audio-only timeline (or a second audio lane) is one
        // edit away -- so the button asks again rather than starting a worker
        // that will only settle with the same refusal minutes later.
        if let Some(why) = format_refusal(session, self.format) {
            self.notify_user(format!("NOT EXPORTED — {why}").into());
            cx.notify();
            return;
        }
        session.pause();
        self.export = Some(session.export_to_with(&self.export_path, &settings));
        // The clock starts with the worker, not with the first repaint that
        // happens to notice it.
        self.export_started = Some(Instant::now());
        self.export_marks.clear();
        // The card has been answered; the progress line takes the panel from
        // here, and it is the running export's escape that matters now.
        self.export_open = false;
        cx.notify();
    }

    /// Gives the editor back at once and leaves the worker to stop at its next
    /// frame and delete what it has written.
    fn cancel_export(&mut self) {
        if let Some(export) = &self.export {
            export.cancel();
            self.cancelling = true;
        }
    }

    /// Takes the export's verdict once it has one. The only place the app
    /// touches the handle's completion side.
    fn poll_export(&mut self) {
        // Sampled here rather than while drawing: a repaint stays a repaint,
        // and this runs once per repaint either way.
        if let (Some(progress), Some(started)) = (
            self.exporting().map(ExportHandle::progress),
            self.export_started,
        ) {
            note_progress(
                &mut self.export_marks,
                started.elapsed().as_secs_f32(),
                progress,
            );
        }
        let Some(result) = self.export.as_ref().and_then(ExportHandle::result) else {
            return;
        };
        self.export = None;
        self.export_started = None;
        // A cancellation is reported as an error, and the one who asked for it
        // has had the editor back since the keystroke. Nothing to say.
        if std::mem::take(&mut self.cancelling) {
            return;
        }
        let text = match result {
            Ok(()) => {
                // Written and still where it was written: the bar carries it
                // until some other notice takes the bar.
                self.exported = Some(self.export_path.clone());
                format!("{EXPORT_DONE}{}", file_name(&self.export_path))
            }
            Err(e) => format!("EXPORT FAILED: {e}"),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
    }
}

impl Render for Player {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A file that has just been opened sits on its first frame with the
        // clock stopped: opening is not playing, whichever way the file
        // arrived. The play binding and the transport button start it.
        if let Some(session) = &mut self.session {
            session.tick();
        }
        self.pump(window);
        // What every hover label asks before it paints: a card or a menu is
        // drawn over whatever the pointer is resting on.
        OVERLAID.store(self.overlaid(), Ordering::Relaxed);
        // A cleared seek is a frame delivered, which is the one readiness signal
        // there is: whatever a slider drag held back is written here.
        if self.seek_since.is_none() {
            self.flush_drag(cx);
        }
        self.poll_export();
        self.poll_import(cx);
        self.poll_silence();
        // Every way a source can arrive -- argv, an import, a project load --
        // has been through a repaint by the time its clips are drawn, so this
        // is the one place that has to notice a new one.
        self.cache_media(cx);
        self.cache_export_seat(cx);
        self.cache_hw_caps(cx);
        // What the compositor calls this window. Pushed only when it changes:
        // it is a protocol round trip and this runs at vsync.
        let title = window_title(&self.name);
        if title != self.titled {
            window.set_window_title(&title);
            self.titled = title;
        }
        // No shadow flag: the session is the only truth about play state, and
        // [`Player::transport`] is the one place it is read.
        let state = self.transport();
        // A paused timeline has nothing to animate; the toggle handlers notify,
        // which is what starts the loop again. A paused seek keeps the loop
        // running by itself until `pump` has the frame it asked for. An export
        // pauses playback and still needs the loop: its progress only reaches
        // the screen on a repaint. A notice does not: it waits to be dismissed
        // rather than for a clock, so keeping the loop alive for it would spin
        // the GPU until someone answered it.
        // An import does too, and for the same reason: its clock and its sweep
        // only reach the screen on a repaint, and a still line is the very
        // thing it exists to disprove.
        if state.is_playing()
            || self.seek_since.is_some()
            || self.export.is_some()
            || self.importing.is_some()
            // A silence scan too: its progress and its two clocks only reach
            // the screen on a repaint, and a still line is the very thing this
            // card was rewritten to disprove.
            || self.silence_scan.is_some()
        {
            window.request_animation_frame();
        }

        // Read per render, never cached: a delete shortens the timeline and the
        // timecode, the ruler and the clamp below all have to follow it -- and
        // so does the room a tail being dragged needs to grow into.
        let duration = self.drawn_duration();
        let position = self.playhead(duration);
        // Re-settled every frame against the duration this one is drawing: an
        // edit that shortens the timeline moves the far end of the view, and a
        // playhead that has run off the bed pulls the view after it -- which is
        // what makes a zoomed-in timeline scroll while it plays.
        // ...but only a playhead that is *going* somewhere pulls it: following
        // is what a moving one does, during playback and through a seek. A view
        // yanked back to a playhead nobody moved is a hand's own scroll undone
        // by the very next frame, which is what made the wheel look dead.
        // ...and a hand that scrolled the view away from the head keeps it
        // ([`Player::panned`]): a follow that centres the head again would undo
        // the notch before it was seen, which is what made the wheel look dead
        // while playing. It is given straight back below, the moment the head
        // is on the bed a person chose to look at -- so the scroll wins now and
        // the follow resumes by itself, with nothing to press.
        self.scale = match (state.is_playing() || self.seek_since.is_some()) && !self.panned {
            true => self.view().following(position),
            false => self.view().settled(),
        };
        if self.panned && self.view().shows(position) {
            self.panned = false;
        }

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                let ctrl = event.keystroke.modifiers.control;
                // `is_held` is the auto-repeat, and a value is the one thing
                // worth running on it: a held arrow on a card moves the slider
                // it picked, and a held volume key runs the volume. Everything
                // else is filtered exactly as it always was -- a repeat that
                // toggled playback, or cut the timeline, many times a second is
                // what this guard is for, and a row waiting for a stroke takes
                // none of it either (it would bind the key, then fire what it
                // just bound). See [`repeats`].
                if event.is_held
                    && !repeats(this.repeat_scope(), key, this.keymap.lookup(key, ctrl))
                {
                    return;
                }
                // Any key answers the message on the bar and brings the next of
                // them up, whatever it was -- and owes
                // the repaint itself: a notice no longer keeps the render loop
                // alive, and the arms below that do notify are not all of them
                // (an unbound key, or the copy chord, changes nothing else).
                if this.dismiss_notice() {
                    cx.notify();
                }
                // A row is waiting for a stroke, and while it is, that stroke is
                // data: it means the binding and nothing else, which is why this
                // answers before the export guard and before the keymap is
                // consulted at all.
                if let Some(action) = this.rebinding {
                    if key == ESCAPE {
                        this.rebinding = None;
                    } else if !is_bare_modifier(key) {
                        this.capture(action, key, ctrl);
                    }
                    cx.notify();
                    return;
                }
                // On linux gpui reports the copy chord as key "c" with the
                // control modifier set (the control code is mapped back), which
                // is why the keymap is keyed on the pair and never on the key
                // alone.
                let action = this.keymap.lookup(key, ctrl);
                // An export is reading the edit list every other action here
                // would change, so cancelling is the only one that means
                // anything until it is over.
                if this.exporting().is_some() {
                    if cancels_export(key, action) {
                        this.cancel_export();
                    }
                    cx.notify();
                    return;
                }
                // The overlay owns the keyboard while it is up -- but it types
                // now: a printable stroke is the search box's, which is why
                // nothing here reaches the keymap. A waiting row is answered
                // above and still wins, so a rebind onto "v" binds the key
                // rather than typing it.
                if this.keys_open {
                    if key == ESCAPE {
                        // Two steps out, the way a search box anywhere gets
                        // out: the filter first -- the whole list back --
                        // and the card only once there is no search to clear.
                        if this.keys_search.is_empty() {
                            this.keys_open = false;
                        } else {
                            this.keys_search.clear();
                            this.scroll_keys(None);
                        }
                    // The rows past the fold, without a wheel: forty actions
                    // are four times what the viewport shows, and the hand
                    // typing in the search box is already on the keyboard.
                    } else if key == "up" {
                        this.scroll_keys(Some(KEYS_ROW_H));
                    } else if key == "down" {
                        this.scroll_keys(Some(-KEYS_ROW_H));
                    } else if key == "backspace" {
                        this.keys_search.pop();
                        this.scroll_keys(None);
                    } else if let Some(c) = typed(key) {
                        this.keys_search.push(c);
                        this.scroll_keys(None);
                    }
                    cx.notify();
                    return;
                }
                // The export card owns it the same way, and for the same
                // reason. Escape closes it -- nothing has been written yet, so
                // there is nothing here to cancel -- and the card's own letters
                // are its input: it has no widget that takes focus (nothing in
                // it does), so this listener is its keyboard, exactly as it is
                // a waiting row's.
                if this.export_open {
                    // A list open over the card is the innermost thing on
                    // screen, so it is what a stroke closes first -- the rule
                    // every menu here follows, said before the card's own keys
                    // so escape does not take the card out from under it.
                    if this.picker.take().is_some() {
                        cx.notify();
                        return;
                    }
                    // A number being typed is the next thing in: while the
                    // field is open every stroke is text, which is what makes
                    // it a field and not a capture -- the card's letters cannot
                    // fire under it, and escape gives up the edit before it
                    // touches the card.
                    if let Some(edit) = &mut this.mbps_edit {
                        if key == ESCAPE {
                            this.mbps_edit = None;
                        } else if key == "enter" {
                            // Committed or refused in its own words; a refused
                            // one stays open on what was typed, so the number
                            // can be fixed rather than typed again.
                            if let Some(mbps) = edit.commit() {
                                this.custom_mbps = mbps;
                                this.quality = Quality::Custom;
                                this.mbps_edit = None;
                            }
                        } else if key == "backspace" {
                            edit.backspace();
                        } else if key == "up" {
                            edit.step(1);
                        } else if key == "down" {
                            edit.step(-1);
                        } else if let Ok(digit) = key.parse::<u32>() {
                            edit.digit(digit);
                        }
                        cx.notify();
                        return;
                    }
                    if key == ESCAPE {
                        this.export_open = false;
                    } else if key == "enter" {
                        // The card's own button, by keyboard: the one thing in
                        // it that writes a file must not be pointer-only either.
                        this.start_export(cx);
                    } else if let Some(format) = format_key(key, this.format) {
                        // The codec rows by their own letter, so the card can be
                        // driven without a mouse -- the same card-local input
                        // the typed bitrate is, and for the same reason: a
                        // choice reachable only by pointer is not reachable by
                        // everyone. Not a keymap binding: it means nothing
                        // outside this card, exactly like the digits.
                        this.set_format(format);
                    } else if key == "c" {
                        this.cycle_container();
                    } else if key == "q" {
                        this.cycle_quality();
                    } else if key == "b" {
                        // The sound's rate, `q`'s pair for the other half of
                        // the file. Not a digit: those are the picture's.
                        this.cycle_audio_kbps();
                    } else if key == "d" {
                        // The save dialog, which was the one row here a
                        // keyboard could not open.
                        this.pick_destination(cx);
                    } else if key == "g" {
                        this.export_grouped = !this.export_grouped;
                    } else if key == "r" {
                        this.export_refusals_inline = !this.export_refusals_inline;
                    } else if key == "n" {
                        // The custom row's field, by keyboard. The digits used
                        // to do this from anywhere in the card, which meant a
                        // stray keystroke changed the bitrate with nothing on
                        // screen to say it had: now a digit outside the field
                        // means nothing at all, and this is the way in.
                        this.edit_mbps();
                    }
                    cx.notify();
                    return;
                }
                // And the equalizer card, the same way again. Its own strokes
                // are the card's input, exactly as the export card's digits
                // are: a band reachable only by dragging is a band a keyboard
                // cannot move at all, and every one of them is listed in the
                // keys menu (keymap.rs `FIXED`) rather than being a secret.
                if this.eq_open.is_some() {
                    // Shift makes the two horizontal keys Q instead of
                    // frequency: both are the *width* of the same hump, so they
                    // sit on the same axis rather than on two keys nobody would
                    // guess. Wider is a lower Q, which is why left widens.
                    let shift = event.keystroke.modifiers.shift;
                    if key == ESCAPE {
                        // Nothing to undo: every change is already at the clip,
                        // and undo is undo's own key.
                        this.eq_open = None;
                        this.eq_dragging = false;
                    } else if key == "up" {
                        this.nudge_band(|b| b.gain_db += EQ_STEP, cx);
                    } else if key == "down" {
                        this.nudge_band(|b| b.gain_db -= EQ_STEP, cx);
                    } else if key == "left" && shift {
                        this.nudge_band(|b| b.q /= EQ_Q_STEP, cx);
                    } else if key == "right" && shift {
                        this.nudge_band(|b| b.q *= EQ_Q_STEP, cx);
                    } else if key == "left" {
                        this.nudge_band(|b| b.freq_hz /= EQ_FREQ_STEP, cx);
                    } else if key == "right" {
                        this.nudge_band(|b| b.freq_hz *= EQ_FREQ_STEP, cx);
                    } else if key == "r" {
                        for band in &mut this.eq_params.bands {
                            band.gain_db = 0.;
                        }
                        this.commit_eq(cx);
                    } else if key == "f" {
                        // This one band back to flat, which is the undo of one
                        // hand movement -- `r` is the undo of the whole card.
                        this.nudge_band(|b| b.gain_db = 0., cx);
                    } else if key == "a" {
                        this.add_band(cx);
                    } else if key == "x" {
                        this.remove_band(cx);
                    } else if key == "s" {
                        // The analyser off and on. Nothing is committed: it is
                        // what the card *shows*, so it survives no further than
                        // this window.
                        this.eq_spectrum = !this.eq_spectrum;
                    } else if let Ok(digit) = key.parse::<usize>() {
                        // As the keys are laid out: 1-9 then 0 for the tenth,
                        // which is the cap ([`EQ_BANDS_MAX`]). A digit past the
                        // last band picks nothing rather than panics.
                        let band = match digit {
                            0 => EQ_BANDS_MAX - 1,
                            n => n - 1,
                        };
                        if band < this.eq_params.bands.len() {
                            this.eq_band = band;
                        }
                    }
                    cx.notify();
                    return;
                }
                // The colour card owns the keyboard the same way the export
                // card does, and its keys mean nothing outside it: the arrows
                // pick a slider and move it, and `r` takes the grade off. Not
                // keymap bindings for exactly that reason -- see `FIXED`, where
                // the keys menu still lists them.
                if this.color_open.is_some() {
                    match color_key(key) {
                        Some(ColorKey::Close) => {
                            this.color_open = None;
                            this.color_dragging = false;
                        }
                        Some(ColorKey::Band(step)) => {
                            this.color_band = (this.color_band + step) % COLOR_BANDS.len();
                        }
                        Some(ColorKey::Nudge(steps)) => this.nudge_color(steps, cx),
                        Some(ColorKey::Reset) => {
                            this.set_color(ColorParams::default(), cx);
                        }
                        None => {}
                    }
                    cx.notify();
                    return;
                }
                // The speed card, the same way again: its arrows move the rate
                // and `r` puts it back to real time, and neither means anything
                // outside the card -- so neither is a binding (see `FIXED`,
                // where the keys menu still lists them).
                if this.speed_open.is_some() {
                    match color_key(key) {
                        Some(ColorKey::Close) => {
                            this.speed_open = None;
                            this.speed_dragging = false;
                        }
                        // The card has one value, so the pair that picks a
                        // slider on the colour card moves this one by a whole
                        // preset's worth instead of a step.
                        Some(ColorKey::Band(step)) => {
                            this.nudge_speed(if step == 1 { -2 } else { 2 }, cx)
                        }
                        Some(ColorKey::Nudge(steps)) => this.nudge_speed(steps as i32, cx),
                        Some(ColorKey::Reset) => this.set_speed(Speed::NORMAL, cx),
                        None => {}
                    }
                    cx.notify();
                    return;
                }
                // The silence card, the same way again: the arrows pick one of
                // its rows and move it, and its two apply keys are the two
                // things it can do to the timeline. Card-local, every one of
                // them -- and listed in the keys menu (keymap.rs `FIXED`),
                // because a key that cuts forty places at once is not a secret.
                if this.silence_open.is_some() {
                    if key == ESCAPE {
                        // Nothing to undo: a preview is not an edit.
                        this.close_silence();
                    } else if key == "down" {
                        this.silence_field = (this.silence_field + 1) % SILENCE_ROWS;
                    } else if key == "up" {
                        this.silence_field = (this.silence_field + SILENCE_ROWS - 1) % SILENCE_ROWS;
                    } else if key == "right" {
                        this.nudge_silence(1);
                    } else if key == "left" {
                        this.nudge_silence(-1);
                    } else if key == "enter" {
                        this.cut_silences(cx);
                    } else if key == "f" {
                        this.speed_silences(cx);
                    }
                    cx.notify();
                    return;
                }
                // The mix card, the same way again: ↑↓ pick a row -- a track's
                // fader, the limiter's ceiling or its switch -- and ←→ move it,
                // held or pressed. Card-local like the four above it.
                if this.mix_open {
                    let rows = this.mix_lanes().len() + MIX_MASTER_ROWS;
                    if key == ESCAPE {
                        this.mix_open = false;
                    } else if key == "down" {
                        this.mix_field = (this.mix_field + 1) % rows;
                    } else if key == "up" {
                        this.mix_field = (this.mix_field + rows - 1) % rows;
                    } else if key == "right" {
                        this.nudge_mix(1, cx);
                    } else if key == "left" {
                        this.nudge_mix(-1, cx);
                    }
                    cx.notify();
                    return;
                }
                // A clip menu names an index, and every edit below moves
                // indices -- so a stroke closes it before it acts. Escape means
                // that and nothing else, which is the `esc` the keys menu
                // already lists (keymap.rs `FIXED`).
                // Both menus, taken rather than short-circuited: the library's
                // one names a row the edits below can remove, so it closes on a
                // stroke exactly as the clip menu does.
                // A choice list goes the same way and for the same reason: it
                // names a clip index too, and escape is the way out of it.
                // An open list is the innermost thing on screen, so it takes
                // the keys before anything under it does: ↑↓ walk it, enter
                // takes the row, and escape falls through to the close below --
                // the same three strokes every list in this editor answers.
                if let Some(mut picker) = this.picker {
                    let rows = this.choices(picker.of);
                    if !rows.is_empty() && matches!(key, "up" | "down" | "enter") {
                        match key {
                            "down" => picker.sel = (picker.sel + 1) % rows.len(),
                            "up" => picker.sel = (picker.sel + rows.len() - 1) % rows.len(),
                            _ => {
                                let (choice, ..) = rows[picker.sel.min(rows.len() - 1)];
                                this.choose(choice, cx);
                                cx.notify();
                                return;
                            }
                        }
                        this.picker = Some(picker);
                        cx.notify();
                        return;
                    }
                }
                let clip_menu = this.context_menu.take().is_some();
                let row_menu = this.library_menu.take().is_some();
                let list = this.picker.take().is_some();
                if clip_menu || row_menu || list {
                    cx.notify();
                    if key == ESCAPE {
                        return;
                    }
                }
                if let Some(action) = action {
                    this.act(action, cx);
                }
            }))
            // The whole window is the drop target: gpui turns an external file
            // drop into an `ExternalPaths` drag (window.rs:3626) delivered as a
            // mouse-up to every hovered hitbox, and the root's is the only one
            // that covers the picture as well as the panel.
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                // The overlay owns the pointer as well as the keyboard, and a
                // drop is a click the scrim cannot swallow: gpui delivers it to
                // the root's hitbox, which is under the scrim but is not a
                // sibling it can stop. The export card is over the timeline for
                // the same reason: importing under it would change the very
                // edit list the card is about to write out.
                if this.modal() {
                    return;
                }
                for path in paths.paths() {
                    // One queue for all of them, in arrival order: the fork --
                    // a project replaces the timeline, media joins the library
                    // -- is made when each one's worker starts ([`arrival`]),
                    // and neither is read on this thread.
                    this.import(path, cx);
                }
            }))
            // A drop event carries no path of its own -- gpui only tells the
            // target that something landed -- so the line that promises where
            // it will land is fed by the drag's own moves, which do carry the
            // pointer (gpui div.rs:282). On the root, because a drag crosses
            // the window: it starts on a clip or on a library row and ends over
            // a lane, and only an ancestor of both hears all of it.
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ClipDrag>, _, cx| {
                // The clip the payload named, wherever an edit mid-drag has
                // since put it ([`Player::dragged`]): the line has to promise a
                // landing for the take actually in the hand.
                let drag = *event.drag(cx);
                if let Some(idx) = this.dragged(&drag) {
                    this.preview_drop(drag.lane, idx, event.event.position.x, cx);
                }
                // The shadow belongs to a *lane*, and which lane the pointer is
                // over is the one thing this element cannot see. Cleared here
                // and drawn again by the lane the pointer is actually inside
                // (`lane_row`), which gpui runs straight after this one: the
                // capture phase goes parent first, so a pointer over no lane at
                // all -- up in the library, say -- promises nothing.
                this.set_ghost(None, cx);
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<AssetDrag>, _, cx| {
                    this.preview_place(event.event.position.x, cx);
                    this.set_ghost(None, cx);
                }),
            )
            // Scrubbing is tracked on the root because the pointer leaves the
            // 6 px ruler on the first drag and its own listeners then stop
            // firing; the root's hitbox is the whole window.
            .on_mouse_move(cx.listener(Self::drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::drag_release))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG_CANVAS()))
            .text_color(rgb(FG_PRIMARY()))
            .text_size(px(12.))
            // Four regions, the arrangement every consumer editor shares:
            // library left, picture centre, inspector right, and the timeline
            // full width along the bottom with its edit toolbar directly above
            // it. Nothing here moves when the state changes -- the regions are
            // fixed and the panels keep their room whether or not anything is
            // open in them.
            .child(self.topbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(self.library(
                        library_w(f32::from(window.viewport_size().width)),
                        f32::from(window.viewport_size().height),
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(0.))
                                    .overflow_hidden()
                                    // The bed the cue plate is placed against:
                                    // it hangs off the bottom of the picture
                                    // region, which is the one box that is the
                                    // picture and nothing else.
                                    .relative()
                                    .flex()
                                    .justify_center()
                                    .items_center()
                                    .bg(rgb(BG_CANVAS()))
                                    .children(
                                        self.image
                                            .clone()
                                            .map(|i| {
                                                img(i)
                                                    .size_full()
                                                    .object_fit(gpui::ObjectFit::Contain)
                                                    .into_any_element()
                                            })
                                            // With no file open the letterbox
                                            // is the whole region, and a black
                                            // rectangle says only that
                                            // something is broken -- so it says
                                            // what it wants instead. The window
                                            // is already the drop target.
                                            .or_else(|| {
                                                self.session
                                                    .is_none()
                                                    .then(|| empty_hint().into_any_element())
                                            }),
                                    )
                                    // After the picture, so the plate is drawn
                                    // over it rather than under.
                                    .children(self.subtitle_overlay(position, window))
                                    // The three transient lines hang off the
                                    // bottom of the picture rather than taking
                                    // a row of the column: a notice that
                                    // arrives must not push the transport, the
                                    // toolbar and the timeline down by its own
                                    // height -- which is a control moving with
                                    // state, on every control below it at once.
                                    .child(
                                        div()
                                            .absolute()
                                            .bottom_0()
                                            .left_0()
                                            .right_0()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.))
                                            .children(self.import_bar(cx))
                                            .children(self.seek_bar())
                                            .children(self.notice_bar(cx)),
                                    ),
                            )
                            .child(self.transport_bar(
                                position,
                                state,
                                f32::from(window.viewport_size().width),
                                cx,
                            )),
                    )
                    // The settings cards live in here rather than over the
                    // timeline: adjusting a clip must not hide the clip.
                    .child(self.inspector(window.viewport_size(), cx)),
            )
            .child(self.toolbar(cx))
            .child(self.timeline(
                position,
                duration,
                state,
                f32::from(window.viewport_size().height),
                cx,
            ))
            // Over the region they were opened on, and under the modal cards:
            // they are only ever up while none of those is (`modal`).
            .children(self.context_card(window.viewport_size(), cx))
            .children(self.library_card(window.viewport_size(), cx))
            // The two that are genuinely modal -- the whole registry, and the
            // card that writes a file -- are the only sheets left over the
            // window.
            .children(self.keys_overlay(cx))
            .children(self.export_card(window.viewport_size(), cx))
            // Last, so it floats over whatever opened it -- an inspector row or
            // a clip menu -- rather than under it.
            .children(self.picker_card(window.viewport_size(), cx))
    }
}


/// Rate limit for scrub seeks: a video worker reopen costs 72-87 ms on the
/// hardware path for the small files it was measured on (215 ms in software), so
/// one seek per mouse move would only queue workers that are cancelled before
/// they decode anything.
///
/// It is a *floor*, not a bound: the reopen is a demux open
/// ([`engine::decode::open_worker`]), and on a 25 GB film that is 550-750 ms --
/// five to seven times this gap, which therefore gates nothing there. Where the
/// cost had to be bounded rather than thinned -- the colour and speed drags --
/// the gate is the frame the worker delivers ([`Player::flush_drag`]) and no
/// timer at all. The ruler keeps this one: a scrub has no value to hold back,
/// only a position that the next mouse move replaces anyway.
const SCRUB_GAP: Duration = Duration::from_millis(100);

fn scrub_due(target: u32, last_target: u32, since: Duration) -> bool {
    target != last_target && since >= SCRUB_GAP
}

/// How far the picture may fall behind the sound before it is restarted at the
/// clock instead of left to crawl after it. Past what an eye reads as lip sync
/// (a tenth of a second or so) and above what a single reopen costs to fix, so a
/// picture that is merely a reopen behind is not answered with another one.
const LATE_RESYNC: f64 = 0.4;
/// The least time between two such restarts: the decoder that cannot keep up is
/// the one this fires for, and it will still be behind straight afterwards.
const RESYNC_GAP: Duration = Duration::from_secs(2);

/// Whether a picture `late` seconds behind the master clock is restarted at it,
/// given when the last restart was ([`Player::pump`]).
fn should_resync(late: f64, last: Option<Instant>) -> bool {
    late > LATE_RESYNC && last.is_none_or(|t| t.elapsed() >= RESYNC_GAP)
}

/// The gate a live drag sample goes through. With the worker still owing a
/// frame (`busy`), writing now would only cancel the open the picture is already
/// waiting for -- the sample is held in `stash` instead, and the frame that
/// lands writes it ([`Player::flush_drag`]). Returns what to write, if anything.
///
/// The press (`first`) never waits: it is the undo step the whole gesture rolls
/// back to, so it has to be taken against the state the hand picked up.
fn stash_or_write<T: Copy>(stash: &mut Option<T>, value: T, first: bool, busy: bool) -> Option<T> {
    match busy && !first {
        true => {
            *stash = Some(value);
            None
        }
        false => Some(value),
    }
}

/// How long an open may stand before the window says so in words. Well past an
/// ordinary seek (a warm reopen is under a tenth of this) and well under what a
/// cold read of a big film takes, which is the only case worth a line.
const SEEK_STALL: Duration = Duration::from_secs(2);

/// What a seek that has stood past [`SEEK_STALL`] says, and nothing at all
/// before that: a line on every click of the ruler would be a flicker, and the
/// picture holding still for a tenth of a second is not something to explain.
/// The import bar's words, for the import bar's reason -- a window that cannot
/// move and a window that has hung look identical, so this one says which.
fn seek_line(standing: Option<Duration>) -> Option<String> {
    let since = standing.filter(|d| *d >= SEEK_STALL)?;
    Some(format!(
        "still opening the picture — a cold read of a big file is seconds of it, and the window \
         is not frozen · {} elapsed",
        clock(since.as_secs_f32())
    ))
}

/// How a finished export announces itself. Written by `poll_export` and read
/// by the notice bar, which is what makes that one line clickable.
const EXPORT_DONE: &str = "EXPORT DONE → ";

/// The path as a URI the bus will take: percent-encoded, because an export
/// lands wherever its source lives and those directories have spaces in them.
/// Bytes, not chars -- a path is not required to be UTF-8.
fn file_uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

/// Shows a file in the desktop's file manager, selected: the freedesktop
/// interface every major one answers, asked for over the session bus. With no
/// file manager on the bus the folder itself is the next best thing, and with
/// neither there is nothing to say -- the notice the click retired was the
/// answer, and a machine without a desktop opener is not one a second notice
/// would help. Blocks on two child processes, so it is never called on the UI
/// thread.
fn show_in_file_manager(path: &std::path::Path) {
    // The URI must be absolute; an export path is only as absolute as the
    // source it was built from.
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let shown = std::process::Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            "ass",
            "1",
            &file_uri(&path),
            "",
        ])
        .status()
        .is_ok_and(|s| s.success());
    if shown {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).status();
    }
}

/// Runs the first chooser that is installed. `Some(None)` is a cancelled
/// dialog; `None` is a machine with no chooser at all, and what still works
/// without one differs per dialog, so the caller words that refusal.
///
/// The desktop's own choosers, asked for by name because gpui 0.2 has no file
/// dialog of its own and none of these is worth a dependency.
fn run_picker(pickers: [(&str, Vec<String>); 2]) -> Option<Option<PathBuf>> {
    for (bin, args) in pickers {
        // Not installed: try the next one. Anything else (a cancel, a refusal)
        // is that chooser's answer and is taken as final.
        let Ok(out) = std::process::Command::new(bin).args(args).output() else {
            continue;
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Some((!path.is_empty()).then(|| PathBuf::from(path)));
    }
    None
}

/// `title` is what the dialog calls itself: two buttons open this same chooser
/// for two different questions -- a file to import, a file to take subtitles out
/// of -- and a dialog titled "import" over the second one is the wrong question
/// answered. No extension filter on either: what can be read is the engine's
/// answer (`PlaybackSession::parse_subtitles` takes a container as readily as a
/// `.srt`), and a list of suffixes written here would hide a file edith would
/// have taken.
fn pick_file(title: &str) -> Result<Option<PathBuf>, &'static str> {
    run_picker([
        (
            "zenity",
            vec!["--file-selection".into(), format!("--title={title}")],
        ),
        ("kdialog", vec!["--getopenfilename".into()]),
    ])
    .ok_or(
        "NO FILE CHOOSER — install zenity or kdialog, or drag the file onto this window to import it",
    )
}

/// The save-side dialog, opened on where the export would land anyway: with no
/// chooser installed that default is still what gets written, so this refusal
/// costs the export nothing and says so.
fn pick_save(default: &std::path::Path) -> Result<Option<PathBuf>, &'static str> {
    let default = default.to_string_lossy().into_owned();
    run_picker([
        (
            "zenity",
            vec![
                "--file-selection".into(),
                // No `--confirm-overwrite`: zenity 4.2 lists it as deprecated
                // and does the confirming itself.
                "--save".into(),
                "--title=edith — export to".into(),
                format!("--filename={default}"),
            ],
        ),
        ("kdialog", vec!["--getsavefilename".into(), default]),
    ])
    .ok_or(
        "NO FILE CHOOSER — install zenity or kdialog to choose where; exporting beside the source",
    )
}

/// Where an export goes: the source path with `.export.mp4` for an extension,
/// so it lands beside the original and can never be the original.
fn export_path(source: impl Into<PathBuf>) -> PathBuf {
    let mut path = source.into();
    path.set_extension("export.mp4");
    path
}

/// Where a save goes when the timeline did not come from a project file: the
/// media path with `.edith` for an extension, beside it like an export. A
/// project loaded from disk keeps its own path instead, so saving it twice
/// writes the same file.
fn project_path(source: impl Into<PathBuf>) -> PathBuf {
    let mut path = source.into();
    path.set_extension("edith");
    path
}

/// Whether a dropped or named path is a project rather than media. Exactly the
/// lowercase extension `save_project` writes -- anything else goes to the
/// demuxer, which is the one that can say what it really is.
fn is_project(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| e == "edith")
}

/// The tail of a path, for showing. A path that is all root has none, and reads
/// as itself.
fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
}

/// How many messages wait behind the one on the bar before the oldest is
/// dropped. A queue with no ceiling is a way for a stuck loop to eat the heap;
/// eight is more than a user will ever answer in a row.
const NOTICES_MAX: usize = 8;

/// The narrowest a notice's *message* is allowed to be squeezed before its hint
/// gives up the line and drops below it.
///
/// The bar is a message beside a hint, and at the 640x360 floor the picture
/// region it hangs under is narrower than the hint alone -- so the message was
/// squeezed to nothing and wrapped one character per line, which is a failure
/// rendered as a column of letters. Wide enough that a line of it is a phrase
/// rather than a word ladder, and narrow enough to still fit beside the hint in
/// any window worth putting two things on a line in.
const NOTICE_MIN_W: f32 = 180.;

/// The whole of the queue's policy, where it can be read at once and tested
/// without a window: dedupe against the back, a ceiling, oldest out first.
/// [`Player::notify_user`] is the door every message comes through; this is what
/// the door does.
fn push_notice(notices: &mut std::collections::VecDeque<SharedString>, message: SharedString) {
    // A repeat of what is already at the back is dropped -- holding a key that
    // refuses would otherwise fill the queue with one sentence, and the count on
    // the bar would be a count of how long the key was held.
    if notices.back() == Some(&message) {
        return;
    }
    if notices.len() >= NOTICES_MAX {
        notices.pop_front();
    }
    notices.push_back(message);
}

/// The tail an open/load notice grows when the file has sound the engine cannot
/// decode: it plays perfectly, in silence, and that is the one thing the window
/// would otherwise never say (the engine's own word for it, verbatim).
fn audio_notice(session: &PlaybackSession) -> Option<String> {
    session
        .audio_disabled_reason()
        .map(|reason| format!(" — NO AUDIO: {reason}"))
}

/// Whether a dropped or named path is a subtitle file rather than media: the
/// formats `engine::subtitle` parses, lowercased, for [`engine::is_audio`]'s
/// reason -- the import door has to know which of the engine's two doors a file
/// goes through before anything is opened.
///
/// `.mks` is one of them, and it is the only Matroska extension that is: it is
/// the *subtitles alone*, so there is no source in it to import and a drop of
/// one used to be refused for having no video track -- while `+ S` on the same
/// bytes took it ([`PlaybackSession::parse_subtitles`] reads it as Matroska).
/// The other two are media and stay media: `.mka` is the sound alone, which
/// [`engine::is_audio`] already imports as a song, and `.mk3d` is a film. Both
/// may *carry* subtitles, which is [`carries_subtitles`] and not this.
fn is_subtitle(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "srt" | "vtt" | "webvtt" | "ass" | "ssa" | "mks"
        )
    })
}

/// Whether a media path is a container that can carry subtitle tracks *inside*
/// it -- every Matroska extension and the three ISO-BMFF ones, which is the list
/// [`PlaybackSession::parse_subtitles`] walks (Matroska blocks and mp4 `tx3g`
/// alike). Named for what it gates and not for one of the two families, because
/// an mp4 answers `true` here now.
///
/// Matroska's set is the standard's own and closed by it
/// (`engine::demux::is_matroska`), so it is copied whole rather than trimmed to
/// the two that carry a film: a `.mk3d` opened as media has its tracks walked
/// like the `.mkv` it is, and a `.mka` song can hold a lyric track. A suffix
/// wider than the engine's would be a file taken here and refused deeper down.
fn carries_subtitles(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "mkv" | "mka" | "mks" | "mk3d" | "webm" | "mp4" | "m4v" | "mov"
        )
    })
}

/// What a worker hands back, which is the fork [`arrival`] made when it was
/// started. An import is *read* and thrown away; the file argv named is opened
/// outright, because nothing else is going to open it afterwards.
enum Landed {
    /// An import into a timeline that is up: the container probed and the
    /// subtitle tracks walked, both *kept* -- they are the expensive halves and
    /// the worker is where they belong, so [`Player::take_import`] is left with
    /// two pushes ([`engine::PlaybackSession::import_probed`],
    /// [`subtitle_tail`]).
    Read(Subs, Probe),
    /// A whole timeline the worker opened, with the tail its subtitle tracks
    /// earn, ready to be hung off the window -- or the engine's refusal. `true`
    /// is the media argv named, which *becomes* the timeline; `false` is a file
    /// arriving at a window that had none, which fills the library and leaves
    /// the lanes empty for a drag ([`Player::install_media`]).
    Media(Result<(PlaybackSession, String), String>, bool),
    /// A `.edith`, restored: argv's, and the one a drop or the Import button
    /// brought ([`arrival`]).
    Project(Result<PlaybackSession, String>),
}

/// What the container walk found, for [`Player::take_import`] to register, or
/// the engine's refusal in the words it would have used on the UI thread.
///
/// `None` for the doors that never reach a demuxer: a song, a still and a
/// subtitle file, whose own small reads stay where they were
/// ([`engine::PlaybackSession::import`]).
type Probe = Option<engine::Result<engine::ImportProbe>>;

/// A media file opened as a session, with the subtitle tracks it carries inside
/// it taken in the same breath -- the two reads a file costs, in the one place
/// both doors ([`Player::open_media`] and the worker below) go through.
///
/// `place` is which door: a file *opened* is the timeline, one *imported* into
/// an empty window fills the library and leaves the lanes empty for a drag.
fn open_session(
    path: &std::path::Path,
    place: bool,
    subs: Subs,
) -> Result<(PlaybackSession, String), String> {
    let opened = match place {
        true => PlaybackSession::open(path),
        false => PlaybackSession::open_library(path),
    };
    let mut session = opened.map_err(|e| e.to_string())?;
    let tail = subtitle_tail(&mut session, subs).unwrap_or_default();
    Ok((session, tail))
}

/// The whole of what a queued file costs, off the UI thread. An import only
/// needs its pages warmed ([`read_ahead`]); the file argv named is *opened*
/// here instead, and that is the difference between a warm launch paying for
/// one header walk and paying for two -- the window is up either way.
fn open_ahead(
    what: Landing,
    path: &std::path::Path,
    stage: &std::sync::atomic::AtomicU8,
    gate: Option<engine::ImportGate>,
) -> Landed {
    match what {
        Landing::Import => read_ahead(path, stage, gate),
        Landing::Project => {
            let opened = PlaybackSession::open_project(path).map_err(|e| e.to_string());
            Landed::Project(opened)
        }
        Landing::Open => open_whole(path, true, stage),
    }
}

/// A whole timeline, opened here and handed over: the file argv named (`place`,
/// which *is* the timeline) and a file arriving at a window with no timeline to
/// import into, which fills the library instead. One function because they are
/// one read -- the engine's two doors differ by which lanes come up empty
/// ([`engine::PlaybackSession::open_library`]), not by what they walk.
fn open_whole(path: &std::path::Path, place: bool, stage: &std::sync::atomic::AtomicU8) -> Landed {
    use std::sync::atomic::Ordering::Relaxed;
    let opened = match place {
        true => PlaybackSession::open(path),
        false => PlaybackSession::open_library(path),
    };
    // The same two stages a read reports, because they are the same two
    // reads: the container, and then the tracks inside it.
    stage.store(ImportStage::Subtitles as u8, Relaxed);
    Landed::Media(
        opened.map_err(|e| e.to_string()).map(|mut session| {
            let subs = subtitle_notice(&mut session, path).unwrap_or_default();
            (session, subs)
        }),
        place,
    )
}

/// Reads, off the UI thread, everything the import that follows would have read
/// -- and hands all of it over. Nothing here is a warm-up any more: the
/// container is *probed* ([`engine::PlaybackSession::probe_import`]) and the
/// subtitle tracks are *walked*, and [`Player::take_import`] registers what came
/// back. Measured on the 24 GB 4K HEVC remux: 21.4 s of header cold, 429 ms
/// warm, plus 1-4 s of probing the timeline's own first source, plus the cue
/// walk -- all of it here, and the window keeps painting through it.
///
/// `gate` is the timeline the file is checked against
/// ([`engine::PlaybackSession::import_gate`]), taken before the worker started
/// because a worker cannot reach the session. `None` is a window with no
/// timeline to import into: then there is nothing to check against and nothing
/// to register, so the file is *opened* here instead, whole
/// ([`open_whole`]) -- which is the same twenty seconds, on the same thread,
/// rather than on the one that draws.
///
/// The header error is carried back now rather than dropped: it is the engine's
/// own refusal, from the only walk anybody makes, so it is worded once and shown
/// at the landing. The subtitle refusal travels beside it for the same reason.
///
/// `stage` is what the line above the panel is naming while this runs.
fn read_ahead(
    path: &std::path::Path,
    stage: &std::sync::atomic::AtomicU8,
    gate: Option<engine::ImportGate>,
) -> Landed {
    use std::sync::atomic::Ordering::Relaxed;
    stage.store(ImportStage::Header as u8, Relaxed);
    // Nothing to import into, and nothing a subtitle file needs opened: the
    // first opens the library itself, here; the second has no container at all.
    let Some(gate) = gate else {
        return match is_subtitle(path) {
            true => Landed::Read(walk_subtitles(path), None),
            false => open_whole(path, false, stage),
        };
    };
    // The three doors an import goes through: a song is measured by its
    // duration and a still by its header -- both the engine's own reads, warmed
    // here and paid again at the landing, which is a header apiece -- and
    // everything else is the container walk, which is handed over whole.
    let probe = if engine::is_audio(path) {
        engine::AudioSession::duration_secs(path).ok();
        None
    } else if engine::is_image(path) || is_subtitle(path) {
        None
    } else {
        Some(PlaybackSession::probe_import(gate, path))
    };
    stage.store(ImportStage::Subtitles as u8, Relaxed);
    // ...and the tracks inside it, kept.
    Landed::Read(walk_subtitles(path), probe)
}

/// Every subtitle track a file carries, cues and all -- the walk that costs, in
/// the one place every door that pays it goes through. `Ok` and empty for a file
/// with none to read, which is what a file that is neither a container we can
/// walk ([`carries_subtitles`]) nor a subtitle file is: the same answer
/// `add_subtitle_tracks` gives it, and nothing is opened to find that out.
///
/// Nothing in here is a session, on purpose ([`PlaybackSession::parse_subtitles`]
/// is an associated fn): no borrow crosses the await, so this runs whole on a
/// worker while the window keeps painting.
fn walk_subtitles(path: &std::path::Path) -> Subs {
    match carries_subtitles(path) || is_subtitle(path) {
        true => PlaybackSession::parse_subtitles(path),
        false => Ok(Vec::new()),
    }
}

/// The subtitle tracks a media file carries, taken into the session as it is
/// opened, and the tail the notice grows for them: an mkv or an mp4 with
/// subtitles in it arrives with its subtitles, because a track nobody imported
/// is a track nobody knows is there. Every other container answers `None`
/// without being read.
///
/// A refusal is a tail too, never a failure of the open: the picture and the
/// sound of a film whose subtitle tracks cannot be walked are still the film.
///
/// Both halves at once, which only a *worker* may do: the walk reads the whole
/// file for its cues (`engine::subtitle::of_matroska`) -- ~200 ms on a two-hour
/// 4K remux, 9.7 s on a cold 25 GB one. The one caller is the open beside which
/// this runs on the worker ([`open_ahead`]), never the render thread; an
/// import splits the two halves across the hop instead ([`read_ahead`] walks,
/// [`subtitle_tail`] pushes).
fn subtitle_notice(session: &mut PlaybackSession, path: &std::path::Path) -> Option<String> {
    subtitle_tail(session, walk_subtitles(path))
}

/// What a subtitle walk ([`walk_subtitles`]) gave, on its way from whichever
/// thread paid for it to the timeline. `Send` all the way down (`sendable()`,
/// `engine/tests/subtitles.rs`), which is what lets the walk be a worker's.
type Subs = engine::Result<Vec<engine::subtitle::SubtitleTrack>>;

/// The tail a file's own subtitle tracks earn on the notice that names it, and
/// the push that puts them on the timeline -- the second half of the walk, the
/// cheap one ([`PlaybackSession::add_subtitle_tracks`]: no open, no seek, no
/// decode). Every door that arrives with a *file* words it here, once: the file
/// argv named, an import, and an import into an empty window cannot say the same
/// thing differently, whichever thread read the cues.
///
/// `None` for a file that gave none and for one whose tracks are on the timeline
/// already -- an import that adds nothing says nothing about subtitles. A
/// refusal is a tail too, never a failure of the import: the picture and the
/// sound of a film whose subtitle tracks cannot be walked are still the film.
fn subtitle_tail(session: &mut PlaybackSession, subs: Subs) -> Option<String> {
    match subs {
        Ok(tracks) => match session.add_subtitle_tracks(tracks) {
            0 => None,
            n => Some(format!(" — {n} subtitle track(s) in the file")),
        },
        Err(e) => Some(format!(" — SUBTITLES UNREAD: {e}")),
    }
}

/// The push both deliberate add-subtitles doors end on -- `+ S` and its key
/// ([`Player::add_subtitles`]) and a dropped or argv'd subtitle file
/// ([`Player::take_subtitles`]) -- so the two cannot come to word the same file
/// differently.
///
/// The walk having found *nothing* is a refusal and not a count: a container
/// with no subtitle track in it and a file whose tracks are all on the timeline
/// already both push zero rows, and they are opposite answers -- one says look
/// somewhere else, the other says you already have them. The engine's own door
/// draws the same line in the same words
/// ([`PlaybackSession::no_subtitles_in`], asked here because this route splits
/// the walk from the push to keep the walk off the render thread).
///
/// The file itself joins nothing either way: no library row, no lane, no clip.
/// Subtitles are a list the timeline carries, and this is the only thing that
/// touches it.
fn pushed(
    session: &mut PlaybackSession,
    path: &std::path::Path,
    tracks: Vec<engine::subtitle::SubtitleTrack>,
) -> engine::Result<usize> {
    match tracks.is_empty() {
        true => Err(PlaybackSession::no_subtitles_in(path)),
        false => Ok(session.add_subtitle_tracks(tracks)),
    }
}

/// What a subtitle row says under its name: how many cues it holds and whether
/// they are pictures ([`engine::subtitle::SubtitleTrack::is_bitmap`]) -- which
/// is the difference between a track an export writes into the file and one it
/// can only draw -- or, for a track that could not be read, the engine's own
/// reason verbatim. A refusal is still what a row can be *for*: a VobSub track
/// dropped from the list would say the film has no subtitles at all.
fn subtitle_detail(track: &engine::subtitle::SubtitleTrack) -> String {
    match (&track.refused, track.is_bitmap()) {
        (Some(why), _) => why.clone(),
        (None, true) => format!("{} cues — pictures", track.cues.len()),
        (None, false) => format!("{} cues", track.cues.len()),
    }
}

/// What the export card's Subtitles row says: the engine's own words for the
/// tracks an export carries (`plan`, [`engine::export::planned_subtitles`] asked
/// about `picks`) and, beside them, the rows it is carrying nothing of.
///
/// `picks` is [`Player::export_subs`] -- every track with a cue left in the
/// exported range -- so a track sitting in the list with eighty-three cues
/// nowhere near a trimmed timeline never reaches the engine at all, and the card
/// said nothing about it while the list went on showing it. Which tracks have
/// cues *here* is this side's answer and nobody else's, so this side words it.
///
/// Past [`SUB_PLAN_CHARS`] the line counts rather than names: every name is more
/// words in a value box `MENU_W` wide, and at 35 tracks the row wrapped to ten
/// lines and pushed the Destination row under the fold of the card. The names
/// are still one row each in the Subtitles list, with the cue count and the
/// reason under them ([`subtitle_detail`]) -- more than this line ever said.
///
/// The counted split follows the engine's own order of reasons (`refused`, then
/// pictures, then no cues), with the last asked of the timeline instead of the
/// track.
///
/// corner-cut: that split reads the same public fields the list rows read
/// (`refused`, [`engine::subtitle::SubtitleTrack::is_bitmap`]) rather than the
/// engine's decision, so a *new* reason to drop a track would be counted here as
/// embedded until this follows it -- the named line, which is the engine's
/// string verbatim, would say it correctly meanwhile. Upgrade path: reasons out
/// of `planned_subtitles` as data rather than as one sentence, which is an
/// engine change.
fn subtitle_plan(
    plan: String,
    tracks: &[engine::subtitle::SubtitleTrack],
    picks: &[usize],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    // "none" is the engine's word for an empty pick list and would read as a
    // verdict on the tracks named after it.
    if !picks.is_empty() {
        parts.push(plan);
    }
    parts.extend(
        tracks
            .iter()
            .enumerate()
            .filter(|(i, _)| !picks.contains(i))
            .map(|(_, track)| format!("{} — no cues here", track.label)),
    );
    let named = parts.join("; ");
    if named.chars().count() <= SUB_PLAN_CHARS {
        return match named.is_empty() {
            true => "none".to_string(),
            false => named,
        };
    }
    let (mut embedded, mut unread, mut pictures, mut off) = (0, 0, 0, 0);
    for (i, track) in tracks.iter().enumerate() {
        match (
            track.refused.is_some(),
            track.is_bitmap(),
            picks.contains(&i),
        ) {
            (true, _, _) => unread += 1,
            (_, true, _) => pictures += 1,
            (_, _, false) => off += 1,
            _ => embedded += 1,
        }
    }
    let mut counted = vec![format!("{embedded} of {} → embedded", tracks.len())];
    counted.extend(
        [
            (pictures, "pictures"),
            (unread, "unread"),
            (off, "no cues here"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, why)| format!("{n} {why}")),
    );
    counted.join("; ")
}

/// Every subtitle track one file gave, kept together. Plain data, planned
/// before anything is drawn -- the grouping is the branchy part and it is the
/// same answer whatever a styling puts around it.
///
/// [`Player::subtitle_section`] draws one block per group of these, and
/// [`sub_pick_name`] names the pick out of the same answer -- so what a heading
/// says and what a row a person clicked says cannot disagree.
#[derive(Debug, PartialEq)]
struct SubGroup {
    /// What the group is called: the file without its extension, which is what
    /// a person named the film even when the subtitles came out of a remux.
    name: String,
    /// What the swatch is asked by ([`file_tint`]) -- the file, so a subtitle
    /// row and the media rows of the same file wear one colour. `None` from
    /// that lookup for a standalone `.srt`, which is nobody's stream.
    path: PathBuf,
    rows: Vec<SubRow>,
}

/// One subtitle track, as a row shows it.
#[derive(Debug, PartialEq)]
struct SubRow {
    /// Which track of the session's list this row is: the *flat* index into the
    /// add-order Vec `PlaybackSession::subtitles` hands back, which is what a
    /// click sets `sub_track` to and what a save writes into the `.edith`.
    /// Grouping moves rows around on screen and never touches this number.
    track: usize,
    /// Which of *this file's* tracks it is, counted from 1 -- the numbering
    /// [`row_name`] gives audio streams, for the same reason: two tracks off one
    /// remux that both say "eng" are told apart by nothing else.
    number: usize,
    label: String,
    /// The row's second line ([`subtitle_detail`]).
    detail: String,
    /// Why it cannot be shown, for a track that cannot: the only thing that
    /// greys a row out. Listed all the same -- a picker that hides them is a
    /// picker that lies.
    refused: Option<String>,
    /// Pictures rather than lines ([`engine::subtitle::SubtitleTrack::bitmap`]).
    bitmap: bool,
}

/// What a subtitle row is called, off the two fields the container really
/// stated rather than out of the flattened
/// [`label`](engine::subtitle::SubtitleTrack::label). The pair is what an export
/// writes (`TRACK_LANGUAGE` and `TRACK_NAME` are two fields), and reading the
/// display string back apart is the very heuristic that sent every French track
/// out as English: `lang_human` on a whole "und — Signs" matches nothing and
/// names a language nobody speaks.
///
/// The three shapes a track arrives in, all three of them real: a standalone
/// file states no language and is its own name, an embedded one states a
/// language and sometimes a title beside it, and a refused one states neither
/// and keeps the label it was refused under (`SubtitleTrack::refused`).
fn sub_title(sub: &engine::subtitle::SubtitleTrack) -> String {
    match (sub.language.as_str(), sub.name.as_str()) {
        ("", "") => sub.label.clone(),
        ("", name) => name.to_string(),
        // A track whose only name is the tag for "nobody said" says that in
        // words rather than showing the tag itself.
        (lang, "") => lang_human(lang).to_string(),
        // ...and one that says "nobody said" *and* gives a title is the title:
        // "unknown language — Signs" pads it with a word the file never said.
        ("und", name) => name.to_string(),
        (lang, name) => format!("{lang} — {name}"),
    }
}

/// The subtitle list as rows under the file each came out of: one group per
/// distinct path, in the order the files first appear, and each file's tracks
/// in the order they were added. Two remuxes' tracks arriving interleaved --
/// which is what importing a second film does -- still read as two films.
fn subtitle_rows(tracks: &[engine::subtitle::SubtitleTrack]) -> Vec<SubGroup> {
    let mut groups: Vec<SubGroup> = Vec::new();
    for (track, sub) in tracks.iter().enumerate() {
        let group = match groups.iter().position(|g| g.path == sub.path) {
            Some(i) => &mut groups[i],
            None => {
                groups.push(SubGroup {
                    name: sub
                        .path
                        .file_stem()
                        .map_or_else(|| file_name(&sub.path), |s| s.to_string_lossy().into()),
                    path: sub.path.clone(),
                    rows: Vec::new(),
                });
                groups.last_mut().expect("just pushed")
            }
        };
        group.rows.push(SubRow {
            track,
            number: group.rows.len() + 1,
            label: sub_title(sub),
            detail: subtitle_detail(sub),
            refused: sub.refused.clone(),
            bitmap: sub.is_bitmap(),
        });
    }
    groups
}

/// How the picked track is named wherever the pick is echoed -- the strip
/// header, the section heading, the toggle's own notice. What the track is
/// *and* which file it came out of: two remuxes each carrying an "eng" track
/// give the same word twice, and the file is the only thing that tells them
/// apart. A file that gave several of them numbers them within itself, the way
/// [`row_name`] numbers audio streams, since "eng" twice off one remux is the
/// same problem one file down.
///
/// Goes through [`subtitle_rows`], so the name a header says and the row a
/// person clicked cannot disagree -- and the label is humanised there, so an
/// "und" track is named in words here without being passed through
/// [`lang_human`] twice.
///
/// `None` for an index no track answers to, which is the silence
/// [`Player::subtitle_track`] gives at the same moment.
/// Where the picked subtitle row lands once `removed` has been taken off a list
/// that is `left` long afterwards. The pick follows the list: the same *track*
/// while it is still there -- every index past the one that went moves down
/// ([`engine::Project::remove_subtitles`]) -- the row that slid into the empty
/// place when the picked one is what went, and the last row when that was the
/// last. Zero on an emptied list, which is the index the section is not drawn at
/// all.
///
/// Its own function because the pick is what the overlay draws: left where it
/// was it would name a different track, and the plate over the picture would
/// change language on its own the moment a row above it went. What an export
/// writes is *not* this pick and cannot be desynced by a removal -- it is worked
/// out from the cues on the timeline each time ([`Player::export_subs`]).
fn sub_pick_after_removal(picked: usize, removed: usize, left: usize) -> usize {
    let picked = match removed < picked {
        true => picked - 1,
        false => picked,
    };
    picked.min(left.saturating_sub(1))
}

fn sub_pick_name(tracks: &[engine::subtitle::SubtitleTrack], track: usize) -> Option<String> {
    subtitle_rows(tracks).into_iter().find_map(|group| {
        let row = group.rows.iter().find(|row| row.track == track)?;
        let track = match group.rows.len() > 1 {
            false => row.label.clone(),
            true => format!("{} {}", row.label, row.number),
        };
        // A standalone `.srt` is already named after its own file: "sub.srt —
        // sub" says the one thing twice, and the film is only worth naming
        // where it is not in the label yet.
        Some(match row.label.starts_with(&group.name) {
            true => track,
            false => format!("{track} — {}", group.name),
        })
    })
}

/// Where a cue is drawn on the bed: left edge and width in pixels, through the
/// same [`Scale`] every clip box goes through -- so a cue and the take it is
/// spoken over line up at every zoom, which is the only reason the strip is
/// worth drawing. Microseconds are the cue's unit and seconds are the scale's,
/// and this is the one place they meet.
///
/// Never narrower than [`SUB_CUE_MIN_W`]: zoomed out, a one-second cue is a
/// fraction of a pixel, and a mark that rounds away reads as a track with
/// nothing in it.
fn cue_box(scale: Scale, cue: &engine::subtitle::Cue) -> (f32, f32) {
    let (start, end) = (cue.start_us as f64 / 1e6, cue.end_us as f64 / 1e6);
    (
        scale.px_at(start),
        scale.width_px(end - start).max(SUB_CUE_MIN_W),
    )
}

/// Which cues of a track are on screen at `at` seconds. Half-open, as the cue
/// itself is: one that ends exactly where the next begins hands over rather than
/// overlapping it for a frame, and several that genuinely overlap all come back
/// -- a sign and a line of dialogue are two cues at one moment.
fn cues_at(cues: &[engine::subtitle::Cue], at: f64) -> Vec<&engine::subtitle::Cue> {
    let us = (at * 1e6) as i64;
    cues.iter()
        .filter(|cue| cue.start_us <= us && us < cue.end_us)
        .collect()
}

/// The sources a repaint has not asked about yet. A key that is already there
/// means "asked", whatever state it is in, which is what stops a decode already
/// running from being started again by the next of sixty repaints a second.
fn unseen_sources(
    sources: &[Source],
    waves: &HashMap<(PathBuf, usize), Wave>,
) -> Vec<(PathBuf, usize)> {
    sources
        .iter()
        .map(|s| (s.path.clone(), s.audio_stream))
        .filter(|key| !waves.contains_key(key))
        .collect()
}

/// The same, for the per-file caches: one entry per *file*, however many of its
/// streams the timeline plays. Generic in the value, because the stream probe
/// and the still's own size are both asked once per file and answered by
/// presence in a map.
fn unseen_paths<V>(sources: &[Source], seen: &HashMap<PathBuf, V>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for s in sources {
        if !seen.contains_key(&s.path) && !out.contains(&s.path) {
            out.push(s.path.clone());
        }
    }
    out
}

/// Which timeline frame the playhead is on, by the rule the engine's own edits
/// use (playback.rs `secs_to_frame`): the frame that has started, with the
/// epsilon that keeps a clock sitting exactly on a boundary from reading as the
/// frame before it. Only ever a hint here -- what an edit does is still the
/// engine's answer, taken from the same seconds.
fn frame_at(secs: f64, fps: f64) -> u32 {
    (secs * fps + 1e-6).floor().max(0.) as u32
}

/// Where a frequency sits across the graph, 0..1. Log, because an octave is an
/// octave whether it is 40 Hz wide or 10 kHz wide -- a linear axis would spend
/// three quarters of the card on the top two octaves and squeeze the bass, the
/// half of the range people actually reach for, into nothing.
fn eq_x(freq_hz: f32) -> f32 {
    let span = (EQ_FREQ_HIGH / EQ_FREQ_LOW).log10();
    ((freq_hz.max(1.) / EQ_FREQ_LOW).log10() / span).clamp(0., 1.)
}

/// The frequency at a fraction across the graph -- [`eq_x`] backwards, which is
/// how a drag and the curve's own sample points read one. Clamped to the axis
/// either way, so a pointer that leaves the box stops at 20 Hz or 20 kHz.
fn eq_freq(along: f32) -> f32 {
    EQ_FREQ_LOW * (EQ_FREQ_HIGH / EQ_FREQ_LOW).powf(along.clamp(0., 1.))
}

/// The band an "add" makes: a flat peak half way -- in octaves, which is what
/// the log axis draws -- between the picked band and whatever sits above it, so
/// a new band lands in the gap on screen rather than on top of its neighbour.
/// Above the topmost band the gap is the rest of the axis.
fn inserted_band(bands: &[Band], after: usize) -> Band {
    let below = bands.get(after).map_or(1000., |b| b.freq_hz);
    let above = bands
        .iter()
        .map(|b| b.freq_hz)
        .filter(|f| *f > below)
        .min_by(f32::total_cmp)
        .unwrap_or(EQ_FREQ_HIGH);
    Band {
        freq_hz: (below * above).sqrt().clamp(EQ_FREQ_LOW, EQ_FREQ_HIGH),
        gain_db: 0.,
        // A shade narrower than the flat-shelf 0.707 the defaults use: a band
        // someone asked for is a band they mean to aim, and a wide one aimed at
        // 300 Hz is really a band at everything.
        q: 1.,
        kind: BandKind::Peak,
    }
}

/// Where a gain sits *down* the graph, 0..1 from the top: flat is the middle,
/// so a cut reads as a dip below the line it is a cut from. The inverse of
/// [`Player::drag_band`]'s reading of the pointer, and clamped like it, so a
/// curve loaded from a file with a gain past the card's limit paints on the
/// edge of the box rather than outside it.
fn eq_y(gain_db: f32) -> f32 {
    0.5 - (gain_db / EQ_GAIN_LIMIT).clamp(-1., 1.) / 2.
}

/// A frequency as the card writes it. Two decimals of a kHz at most, with the
/// zeroes trimmed, so "1 kHz" stays "1 kHz" and a band nudged off it reads as
/// 1.12 kHz rather than as the same "1 kHz" it was before the keystroke -- a
/// number that does not move under an edit is worse than no number.
fn eq_freq_label(freq_hz: f32) -> String {
    if freq_hz < 1000. {
        return format!("{freq_hz:.0} Hz");
    }
    let khz = format!("{:.2}", freq_hz / 1000.);
    let khz = khz.trim_end_matches('0').trim_end_matches('.');
    format!("{khz} kHz")
}

/// What a band row calls itself: the corner or centre frequency, and for a
/// shelf the fact that it tilts everything past it -- which is the difference
/// between "12 kHz" moving the last octave and moving one band inside it.
fn band_label(band: &Band) -> String {
    let freq = eq_freq_label(band.freq_hz);
    match band.kind {
        BandKind::LowShelf => format!("{freq} low shelf"),
        BandKind::HighShelf => format!("{freq} high shelf"),
        BandKind::Peak => freq,
    }
}

/// Whether an action can be asked for, and what to say when it cannot. Two
/// kinds of no: `Hidden` is about the *kind* of thing the action was aimed at
/// -- an audio clip has no picture, so a grade is not a thing that exists for
/// it, whatever the editor does next -- and `No` is about the state of this
/// moment, which the next click of the playhead can change. The clip menu
/// leaves the class refusals *out* and dims the state ones ([`Enable::listed`]);
/// the actions card, which is the whole registry laid out, dims both with their
/// reason -- an action missing from the one surface that lists everything would
/// read as an action that does not exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Enable {
    Yes,
    No(&'static str),
    Hidden(&'static str),
}

impl Enable {
    /// Whether the row takes a click.
    fn yes(self) -> bool {
        self == Enable::Yes
    }

    /// Whether a menu *about this clip* draws the row at all: a class refusal
    /// is a thing that does not exist for what was clicked, and a row nothing
    /// the user does could ever light is noise between the ones they came for.
    fn listed(self) -> bool {
        !matches!(self, Enable::Hidden(_))
    }

    /// What the row says instead of its stroke, if it says anything.
    fn why(self) -> Option<&'static str> {
        match self {
            Enable::Yes => None,
            Enable::No(why) | Enable::Hidden(why) => Some(why),
        }
    }
}

/// What an enablement question is asked *about*: the clip in question, if there
/// is one, and the little of the editor's state the answers need. Handed in
/// rather than read off the player, so [`enable`] is a pure function a test can
/// ask about a clip without building a window.
#[derive(Clone, Copy, Default)]
struct Ctx {
    /// The clip the question is about -- the one a menu was opened on, or the
    /// marked one. `None` means the question is about the editor as a whole,
    /// and the clip-relative answers stand aside: those actions find their own
    /// clip under the playhead and word their own refusal.
    clip: Option<(Clip, Lane)>,
    /// The clip plays a still ([`engine::is_image`]), which has no sound to
    /// reach at all -- not the lane's business, because a still sits on a video
    /// lane exactly like a take whose sound is one lane down.
    image: bool,
    playhead: u32,
    /// A timeline is open.
    timeline: bool,
    /// Something has been copied.
    clipboard: bool,
    /// This timeline has at least one subtitle track: a toggle with nothing to
    /// show is a switch that does nothing, and it says so rather than flipping.
    subtitles: bool,
    /// There is something on the lanes to play. A transport with nothing under
    /// it is the one refusal that is about the timeline's contents rather than
    /// about a clip.
    playable: bool,
    exporting: bool,
}

/// Whether `action` can be asked for, on `ctx`. One arm per action and nothing
/// else in the editor asks the question: the clip menu dims a row with this,
/// the actions card dims a row with this, and the two can never come to
/// disagree about what an action needs -- exactly the reason [`Player::act`] is
/// one table too.
fn enable(action: ActionId, ctx: Ctx) -> Enable {
    // The one action that is about the editor rather than about the timeline:
    // the list of what everything does, and where a key is changed. An empty
    // window has no clips and still has keys -- so this answers ahead of the
    // timeline question, and only an export shuts it (a waiting row would
    // swallow the escape the progress line promises cancels the export).
    // The window's own colours: there is always a window, and repainting it
    // touches no timeline -- so this one is live even while an export is
    // reading one, which is the only state that dims the list above.
    if action == ActionId::Theme {
        return Enable::Yes;
    }
    if action == ActionId::ShowActions {
        return match ctx.exporting {
            true => Enable::No("an export is running"),
            false => Enable::Yes,
        };
    }
    // The four that are about the *editor* and its monitoring rather than
    // about the edit list: they work with nothing open, the keyboard has
    // always fired them there, and so their buttons are live there too.
    if matches!(
        action,
        ActionId::ToggleSnap | ActionId::ToggleMute | ActionId::VolumeUp | ActionId::VolumeDown
    ) {
        return match ctx.exporting {
            true => Enable::No("an export is running"),
            false => Enable::Yes,
        };
    }
    if !ctx.timeline {
        return Enable::No("no timeline open");
    }
    // An export is reading the edit list every other action would change, which
    // is the rule the key handler already follows.
    if ctx.exporting {
        return match action {
            ActionId::CancelExport => Enable::Yes,
            _ => Enable::No("an export is running"),
        };
    }
    match action {
        // -- class: what kind of thing the action is about. The equalizer
        // filters samples, and a video clip has none of its own here: the sound
        // is the audio lane's, clip for clip.
        ActionId::Equalizer => match ctx.clip {
            Some((_, lane)) if lane.kind != LaneKind::Audio => Enable::Hidden("this clip is picture"),
            _ => Enable::Yes,
        },
        // A grade is a picture setting and an audio clip has no picture. A fit
        // policy is a picture setting for the same reason.
        ActionId::Color | ActionId::Fit => match ctx.clip {
            Some((_, lane)) if lane.kind != LaneKind::Video => Enable::Hidden("this clip is sound"),
            _ => Enable::Yes,
        },
        // The scan reads samples, and a still has none -- ever, unlike a video
        // clip whose sound may be one lane down or simply silent. Exactly what
        // `unscannable` says after the fact, said before the row is drawn so
        // there is no row left to click.
        ActionId::Silence if ctx.image => Enable::Hidden("this clip is a still"),
        // -- state: true of this clip now, and the next playhead click or the
        // next selection changes the answer. Splits this clip only from inside
        // it: at either edge there is nothing to split off -- and, on a speeded
        // clip, only at a frame its own rate can address, which is the same
        // question `splittable` asks.
        ActionId::Cut => match ctx.clip {
            Some((clip, _)) if !(clip.start < ctx.playhead && ctx.playhead < clip.end()) => {
                Enable::No("only from inside a clip")
            }
            // Inside it, and still not a cut: a slowed clip shows one frame of
            // the file for several frames of the timeline, and only the first of
            // those is a frame the file has ([`Speed::split_at`]). Cutting
            // between two showings of one frame would leave halves whose lengths
            // no longer add up, so it is refused -- and it says *that* rather
            // than repeating "inside a clip" at a playhead that plainly is.
            Some((clip, _))
                if clip
                    .speed
                    .split_at(clip.len(), ctx.playhead - clip.start)
                    .is_none() =>
            {
                Enable::No("this speed holds one frame here — step to the next")
            }
            _ => Enable::Yes,
        },
        // Rejoins whatever meets at the playhead, so it can mean something only
        // at an edge of this clip. Whether those two halves were ever one take
        // is the engine's question, and it words that refusal itself.
        ActionId::Regroup => match ctx.clip {
            Some((clip, _)) if ctx.playhead != clip.start && ctx.playhead != clip.end() => {
                Enable::No("only where two clips meet")
            }
            _ => Enable::Yes,
        },
        // Nothing to take apart in a clip that names no group at all. Whether
        // the group it names still has another half is the engine's question,
        // like the regroup above.
        ActionId::Detach => match ctx.clip {
            Some((clip, _)) if clip.link.is_none() => Enable::No("this clip is not grouped"),
            _ => Enable::Yes,
        },
        // The three that act on the marked clip and on nothing else: with none
        // marked they would silently do nothing, which is what the Delete
        // button's own dimming has always said.
        ActionId::Copy | ActionId::Delete | ActionId::Lift if ctx.clip.is_none() => {
            Enable::No("click a clip first")
        }
        ActionId::Paste if !ctx.clipboard => Enable::No("nothing copied yet"),
        // Nothing to draw over the picture, so nothing to switch off: the
        // library says how subtitles arrive, and this row would flip a state
        // with no visible half either way.
        // ...and nothing to take off the timeline either, for the same reason:
        // the row would name a track that is not there.
        ActionId::ToggleSubtitles | ActionId::RemoveSubtitleTrack if !ctx.subtitles => {
            Enable::No("no subtitles yet")
        }
        // Not `No`: with nothing running there is no export for this to be
        // about at all, which is what keeps `esc` -- the same key -- a quiet
        // way out of a card rather than a line about exports.
        ActionId::CancelExport => Enable::Hidden("nothing is exporting"),
        // A clock started against an empty timeline is a clock counting
        // nothing: the transport says so by being dim, which is what its own
        // ad-hoc boolean used to say before the oracle knew the question.
        ActionId::Play if !ctx.playable => Enable::No("put a clip on a lane first"),
        // A rate applies to a clip of either kind and to its whole group, so
        // there is no lane it means nothing on, and the engine words the one
        // refusal there is (no room). Everything else is the editor's own and
        // needs nothing but a timeline.
        _ => Enable::Yes,
    }
}

/// The rows a clip menu draws, for the clip it was opened on: the registry
/// filtered by the one availability oracle, and the *only* way that menu is
/// built. An action that means nothing for what was right-clicked -- a grade on
/// a waveform, an equalizer on a picture -- is not a row at all, so a future
/// action cannot appear where it does not apply by being added to
/// [`MENU_ITEMS`] alone.
fn menu_items(ctx: Ctx) -> Vec<ActionId> {
    MENU_ITEMS
        .into_iter()
        .filter(|&action| enable(action, ctx).listed())
        .collect()
}

/// What a library row's items are asked *about*: the file that was
/// right-clicked and the little of the editor's state the answers need. The
/// library's [`Ctx`], handed in for the same reason.
#[derive(Clone, Copy, Default)]
struct RowCtx {
    /// A timeline to put it on.
    timeline: bool,
    exporting: bool,
    /// This row can join *this* timeline: what greys it in the list, and what
    /// the engine would otherwise refuse the Add with after the click.
    usable: bool,
    /// How many clips play this exact row -- a source with any is one the
    /// engine will not take out of the list.
    placed: usize,
}

/// Whether a library row's item can be asked for. The library's half of
/// [`enable`], and the same rule: one table, no second policy in the render.
fn row_enable(item: RowItem, ctx: RowCtx) -> Enable {
    match item {
        // The two that change the timeline, so an export reading it stops them
        // both -- the key handler's rule, applied to a menu.
        RowItem::Add | RowItem::Remove if ctx.exporting => Enable::No("an export is running"),
        RowItem::Add | RowItem::Remove if !ctx.timeline => Enable::No("no timeline open"),
        // Dimmed and saying why rather than clicked and refused afterwards: the
        // row's own grey already says the file cannot join this timeline.
        RowItem::Add if !ctx.usable => Enable::No("it cannot join this one"),
        RowItem::Remove if ctx.placed > 0 => Enable::No("clips play it"),
        // Neither of these touches the timeline: a file can be found on disk and
        // described whatever the editor is doing, with no timeline at all.
        _ => Enable::Yes,
    }
}

/// The rows a library menu draws, for the row it was opened on -- the clip
/// menu's [`menu_items`] on the other panel, and the only way that menu is
/// built.
fn row_items(ctx: RowCtx) -> Vec<RowItem> {
    ROW_ITEMS
        .into_iter()
        .filter(|&item| row_enable(item, ctx).listed())
        .collect()
}

/// How tall a menu's list may draw: the whole of it where the window has room,
/// and what the window has where it has not -- only then does the list scroll.
/// A cap fixed at twelve rows put the last items behind a scroll on a window
/// with room to spare, which reads as a menu cut off by the bottom edge.
fn menu_rows_h(rows: usize, viewport: Size<Pixels>) -> f32 {
    let room = f32::from(viewport.height) - MENU_PAD * 2.;
    (rows as f32 * MENU_ROW_H).min(room.max(MENU_ROW_H))
}

/// Where the menu actually hangs: at the pointer, pulled back inside the window
/// when it would not fit -- an item off the bottom edge is an item nobody can
/// click. Never negative, so a window smaller than the menu loses the bottom of
/// it rather than the top, where the items are.
fn menu_at(at: Point<Pixels>, viewport: Size<Pixels>, height: f32) -> (f32, f32) {
    let fit = |v: f32, size: f32, room: f32| v.min(room - size).max(0.);
    (
        fit(f32::from(at.x), MENU_W, f32::from(viewport.width)),
        fit(f32::from(at.y), height, f32::from(viewport.height)),
    )
}

/// Whether the Add button does anything: a row picked, a timeline to put it on,
/// and no export reading that timeline. A button that would do nothing is dimmed
/// and takes no click, like every other one here.
fn can_add(picked: Option<&(PathBuf, usize)>, timeline: bool, exporting: bool) -> bool {
    picked.is_some() && timeline && !exporting
}

/// What is left on the clipboard after the library row at `removed` was taken
/// out. A copied clip names its file by *index* into the source list
/// (`engine::Clip::source`) and a removal renumbers that list
/// (`engine::Project::remove_source`), so a clipboard kept as it was would paste
/// **a different file** -- the next one along -- over the range it was copied
/// from.
///
/// The clip's own file gone means there is nothing to paste: `None`, and the
/// next paste says the clipboard is empty rather than putting some other take
/// down. Every index past it moves down by one, exactly as the lanes' clips do.
fn clipboard_after_remove(clip: Option<Clip>, removed: usize) -> Option<Clip> {
    let mut clip = clip?;
    match clip.source.cmp(&removed) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => {
            clip.source -= 1;
            Some(clip)
        }
        std::cmp::Ordering::Less => Some(clip),
    }
}

/// One library row: a file and one of its audio streams. Plain data, planned
/// before anything is drawn -- which rows exist at all is the branchy part.
#[derive(Debug, PartialEq)]
struct Row {
    path: PathBuf,
    stream: usize,
    /// The file, plus which stream this is when the file has several.
    name: String,
    /// What the stream is, or blank for a file with a single one.
    detail: String,
    /// Why it cannot be put on this timeline, for a row that cannot: shown in
    /// place of the length and the only thing that greys a row out.
    unusable: Option<String>,
    /// Frames of the *file*, for the length line.
    frames: u32,
    /// Index into `SOURCE_TINTS`, shared by every stream of one file: the
    /// swatch says which file, and the lanes tint their clips the same way.
    tint: usize,
}

/// Every row the library shows: one per source entry, plus one for each further
/// audio stream those files have that no clip plays yet -- a remux with a track
/// per language lists them all, the ones this engine cannot use greyed out
/// rather than hidden. Streams a file has not been probed for yet simply are
/// not there; the row for what *is* on the timeline is always there.
///
/// `timeline_audio` is the rate and layout of source 0's stream, which every
/// other source must match: one output device and one copied AAC track for the
/// whole timeline (`PlaybackSession::place_stream_at`). `None` while unknown,
/// and then nothing is greyed for it -- the engine still refuses.
fn library_rows(
    sources: &[Source],
    streams: &HashMap<PathBuf, Vec<StreamInfo>>,
    decoders: &HashMap<PathBuf, Option<(Option<Codec>, Backend)>>,
    timeline_audio: Option<(u32, u16)>,
    frames: impl Fn(&Path) -> u32,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for (i, source) in sources.iter().enumerate() {
        let of_file = streams.get(&source.path).map_or(&[][..], Vec::as_slice);
        let info = of_file.iter().find(|s| s.index == source.audio_stream);
        let tint = sources
            .iter()
            .position(|s| s.path == source.path)
            .expect("a source finds itself");
        rows.push(Row {
            path: source.path.clone(),
            stream: source.audio_stream,
            name: row_name(&source.path, source.audio_stream, of_file.len() > 1),
            // The decoder first: it is the same for every row of a file and
            // it is what a person opening the panel is asking about. The
            // stream half only where there is a choice to describe -- a file
            // with one audio track is the row it has always been, name and
            // length, and the length is what would be squeezed out at the
            // panel's least width.
            detail: join_detail(
                &decoders
                    .get(&source.path)
                    .copied()
                    .flatten()
                    .map_or_else(String::new, |(codec, backend)| decode_label(codec, backend)),
                &info
                    .filter(|_| of_file.len() > 1)
                    .map_or_else(String::new, stream_detail),
            ),
            // A stream already on the timeline is playing: whatever a probe
            // would say about it now, it is usable by demonstration.
            unusable: None,
            frames: frames(&source.path),
            tint,
        });
        // The file's other streams, listed once, right after the last entry
        // that names the file -- so a file's rows sit together.
        if sources[i + 1..].iter().any(|s| s.path == source.path) {
            continue;
        }
        for info in of_file {
            if sources
                .iter()
                .any(|s| s.path == source.path && s.audio_stream == info.index)
            {
                continue; // it has a row of its own above
            }
            rows.push(Row {
                path: source.path.clone(),
                stream: info.index,
                name: row_name(&source.path, info.index, true),
                detail: stream_detail(info),
                unusable: unusable(info, timeline_audio),
                frames: frames(&source.path),
                tint,
            });
        }
    }
    rows
}

/// The row's second line: what the stream is and then either how long it is or
/// why it cannot be used, with the separator only where both halves exist (a
/// single-stream file says nothing about its stream).
fn join_detail(detail: &str, tail: &str) -> String {
    match (detail.is_empty(), tail.is_empty()) {
        (true, _) => tail.to_string(),
        (false, true) => detail.to_string(),
        (false, false) => format!("{detail} · {tail}"),
    }
}

/// How a source's decoder reads: the codec and which seat has it, or the seat
/// alone for a still, which has no coded stream to name. The one place either
/// answer is spelled, so a row, a transport line and a card cannot disagree.
fn decode_label(codec: Option<Codec>, backend: Backend) -> String {
    match codec {
        Some(codec) => format!("{} · {}", codec.name(), backend.label()),
        None => backend.label().to_string(),
    }
}

/// How a row names its file: the file alone when it has one audio stream or
/// none, and the file plus which stream this row is when it has several --
/// counted from 1, the way a player numbers tracks.
fn row_name(path: &Path, stream: usize, several: bool) -> String {
    match several {
        false => file_name(path),
        true => format!("{} [audio {}]", file_name(path), stream + 1),
    }
}

/// What a stream is, for the row's second line: the language if the file says
/// one, then rate and layout. A field the header does not give is left out
/// rather than shown as a zero -- a stream we cannot parse says nothing about
/// itself, and saying "0 Hz" would be saying something.
fn stream_detail(info: &StreamInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(lang) = &info.lang {
        parts.push(lang_human(lang).to_string());
    }
    if info.sample_rate > 0 {
        parts.push(format!("{} kHz", f64::from(info.sample_rate) / 1000.));
    }
    parts.extend(layout(info.channels));
    parts.join(" ")
}

/// What a file is coded at, for the properties cards: the rate of the whole
/// file, then each track's own beside it. A component the container does not
/// state is left out for `stream_detail`'s reason -- a fabricated `0` would be
/// saying something -- and a file that states none of the three says so rather
/// than showing an empty row. `None` is the probe still running, which is a
/// real wait: it walks a Matroska's clusters.
///
/// The whole file's rate is the one that carries the unit and no word: named
/// "total" as well, the line loses its last component to `MENU_W`'s truncation
/// on an ordinary 1080p file -- and a rate cut to "0.13 soun" is a number the
/// card did not give.
///
/// `tracks` is how many sound tracks the file has, from `probe_streams`. The
/// rate is the one track this engine plays -- the first, neither their sum nor
/// the biggest -- so a file with more says which of how many: a bare "0.16
/// sound" on a file whose name says AC3.5.1 names a track without saying it is
/// one of two, and the card's Audio row above it may be describing the other.
///
/// The marker costs the word "sound", the way the whole file's rate costs the
/// word "total" above, and for the same reason. Measured in Noto Sans 11 px
/// against the 186 px this value has beside a "Bitrate" label: "0.16 sound 1/2"
/// wants 192 px on a 4.7 Mb/s film and 205 on the 39.8 Mb/s remux --
/// the marker is what gets cut, which is the one part of the line that is new.
/// Nothing that keeps the word fits the wide files -- the shortest, "snd 1/2",
/// still wants 188 px on that remux -- so the word goes and the number keeps
/// the answer: "0.16 1 of 2" wants 172 px, and 185 on the widest line his
/// library can produce (the 10.9 Mb/s three-track film).
fn bitrate_detail(rate: Option<MediaBitrate>, tracks: usize) -> String {
    let Some(rate) = rate else {
        return "…".to_string();
    };
    let (per, unit) = rate_scale(rate);
    let mut parts: Vec<String> = Vec::new();
    if let Some(total) = rate.total {
        // One unit for the three numbers, carried once, on the whole file's
        // rate: the components are read against it.
        parts.push(format!("{} {unit}", scaled(total, per)));
    }
    if let Some(video) = rate.video {
        parts.push(format!("{} video", scaled(video, per)));
    }
    if let Some(audio) = rate.audio {
        parts.push(match tracks > 1 {
            true => format!("{} 1 of {tracks}", scaled(audio, per)),
            false => format!("{} sound", scaled(audio, per)),
        });
    }
    match parts.is_empty() {
        true => "not stated".to_string(),
        false => parts.join(" · "),
    }
}

/// Below this a megabit's two decimals cannot state a rate, so the line is read
/// in kilobits instead.
const MB_FLOOR: u64 = 10_000;

/// What the line counts in, and the name of it: the largest unit that can state
/// its *smallest* component, because that is the one a bigger unit rounds away.
/// Megabits for everything a real file produces -- the smallest component over
/// an 18-file sweep of his library was 0.13 Mb/s -- kilobits for the sub-32x32
/// encodes below that.
///
/// corner-cut: one unit for the whole line, so a file mixing a multi-megabit
/// picture with a sub-10 kb/s sound track prints the picture as four or five
/// digits of kilobits and loses the line's tail to `MENU_W`. No such file
/// exists in his library, and both units at once ("0.01 Mb/s · 1.2 kb/s video ·
/// 9.5 kb/s sound") wants 215 px of the 186 the row has. Upgrade path is a
/// suffix per component ("1.2k video"), which measures 177 px.
fn rate_scale(rate: MediaBitrate) -> (f64, &'static str) {
    match [rate.total, rate.video, rate.audio]
        .into_iter()
        .flatten()
        .min()
    {
        // A rate this small is a broken header rather than a track, but it is
        // still a number the file stated, and bits state it.
        Some(bits) if bits < 10 => (1., "b/s"),
        Some(bits) if bits < MB_FLOOR => (1_000., "kb/s"),
        _ => (1_000_000., "Mb/s"),
    }
}

/// A rate in the line's unit as a person reads it: one decimal above 1 of it,
/// two below -- a 128 kbps song rounded to one decimal is "0.1", which reads as
/// a guess where "0.13" reads as a measurement.
///
/// Never `0.00`: [`rate_scale`] picks the unit off the smallest component, so
/// two decimals always reach it. Every rate that gets here is one the container
/// really stated (the probe leaves out what it does not state, it never zeroes
/// it), and a "0.00 sound" would be this card saying a track that plays is
/// silent.
fn scaled(bits: u64, per: f64) -> String {
    let n = bits as f64 / per;
    match n >= 1. {
        true => format!("{n:.1}"),
        false => format!("{n:.2}"),
    }
}

/// A language tag as a person reads it. Everything a file writes is passed
/// through untouched bar the one tag that is not a language: "und" is what a
/// muxer writes when nobody said, and a row showing it verbatim names a
/// language nobody speaks.
fn lang_human(lang: &str) -> &str {
    match lang {
        "und" => "unknown language",
        lang => lang,
    }
}

fn layout(channels: u16) -> Option<String> {
    match channels {
        0 => None,
        1 => Some("mono".to_string()),
        2 => Some("stereo".to_string()),
        n => Some(format!("{n} ch")),
    }
}

/// Why a stream cannot go on this timeline, or `None` if it can. Both answers
/// are shown: a stream nothing can be done with is listed greyed with the
/// reason, never dropped from the list -- a file has the tracks it has, and a
/// picker that hides them is a picker that lies.
fn unusable(info: &StreamInfo, timeline_audio: Option<(u32, u16)>) -> Option<String> {
    if !info.decodable {
        // AAC and AC-3 name themselves (`StreamInfo::codec`, which reads the
        // stsd fourcc by hand); anything else mp4 0.14 does not parse has no
        // name to give, and a row with no name still says why it is greyed.
        return Some(match info.codec.as_str() {
            "unknown" => "unsupported codec".to_string(),
            codec => format!("{codec} is not supported"),
        });
    }
    // The **layout** and not the rate, which is what the engine's own gate now
    // asks (`PlaybackSession::import`): a stream written at another sample rate
    // is resampled onto the timeline's at the decoder's door, so greying its row
    // would be this picker refusing what the timeline accepts.
    let (_, channels) = timeline_audio?;
    (info.channels != channels).then(|| {
        format!(
            "the timeline is {}",
            layout(channels).unwrap_or_else(|| "silent".to_string())
        )
    })
}

/// How wide the equalizer card is drawn in a window this wide: all of it bar a
/// margin, up to [`EQ_W_MAX`]. The card is a graph, and a graph of twenty
/// thousand hertz on 320 px spends four pixels on the octave below middle C --
/// so it takes the room a big window has and stays inside a small one. Floored
/// at the other cards' width, which is the last size the rows still read at.
fn eq_card_w(window_w: f32) -> f32 {
    (window_w - EQ_W_MARGIN).clamp(KEYS_W, EQ_W_MAX)
}

/// What the media list is given of the window. A share of it, so a narrow
/// window gives the panel less rather than giving the picture nothing, floored
/// where a name stops being readable and capped at a third of the window --
/// the picture is what the program is for and keeps the majority at every size.
fn library_w(window_w: f32) -> f32 {
    (window_w * LIBRARY_FRAC)
        .clamp(LIBRARY_MIN_W, LIBRARY_MAX_W)
        .min(window_w / 3.)
}

/// What is left of a library column this wide for a row's *words*: the panel's
/// padding on both sides, the tint bar, the gap after it and the row's own right
/// inset. Every row in the column -- media and subtitle -- is built to this
/// shape, so one number answers for both.
fn row_text_w(width: f32) -> f32 {
    // 8 px of panel padding each side, then the bar, the gap after it and the
    // row's own right inset -- the numbers the rows are built with.
    width - 16. - SWATCH_W - 6. - 6.
}

/// A name cut to what a column this wide can hold, out of the *middle*. Two
/// files off one release differ in their last characters and nowhere else --
/// "…Episode 01" against "…02" -- so a name cut from the right is the same
/// name twice and the list stops naming anything. The width decides how much
/// survives, not a number of characters somebody guessed: a wider window spells
/// more of the file out, and the floor still keeps both ends.
///
/// The element truncates for real; this only decides where what is lost comes
/// out of.
fn clip_middle(name: &str, width: f32) -> String {
    let budget = ((width / LIST_CHAR_W) as usize).max(LIST_CLIP_MIN);
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= budget {
        return name.to_string();
    }
    // The gap costs a character, and what is left of the odd one goes to the
    // tail: the tail is the half that tells two of them apart.
    let head = (budget - 1) / 2;
    let tail = budget - 1 - head;
    format!(
        "{}…{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    )
}

/// Whether the subtitle list has the height to name each file over its tracks.
/// A header is worth its own height only where there are still tracks under it
/// to read: at the 640x360 floor the list is one row tall, and a header there
/// would name a film and show none of it -- so at the floor there are no
/// headers and the rows say which file they came from themselves.
///
/// The window's height answers it on its own: the row above the picture and the
/// panel under it are fixed, so every pixel the window gains past the floor is a
/// pixel this column gains.
fn sub_headers_fit(viewport_h: f32) -> bool {
    SUB_ROWS_AT_FLOOR + (viewport_h - 360.).max(0.) >= SUB_HEAD_H + 2. * ROW_H
}

/// What the window is called: the program, and what is open in it. The name is
/// what the header shows, so an empty window says the program alone rather than
/// "no file open — edith".
fn window_title(name: &str) -> String {
    if name == NO_FILE {
        "edith".to_string()
    } else {
        format!("{name} — edith")
    }
}

/// The tint a clip from source `n` wears. Cycled rather than extended: past the
/// palette two sources share a colour, which is a smaller lie than a fifth tint
/// bright enough to leave the family.
fn source_tint(source: usize) -> u32 {
    SOURCE_TINTS()[source % SOURCE_TINTS().len()]
}

/// The tint of a *file*, which is what a library row is named by: the first
/// source entry naming it, since two audio streams of one file are two sources
/// and one colour.
///
/// `None` for a path no source names -- a standalone `.srt` is on nobody's
/// timeline, and painting it with the first file's colour would say it came out
/// of that file. No swatch says what is true: it belongs to itself.
fn file_tint(sources: &[Source], path: &Path) -> Option<u32> {
    sources
        .iter()
        .position(|s| s.path == path)
        .or_else(|| {
            // A source entry is stored symlink-resolved (`Source::new`) and a
            // path from anywhere else -- a subtitle track, a file being
            // dragged -- is stored as it was spelled. `edith assets/film.mkv`
            // is one file under two spellings, and matching by spelling alone
            // said the film had no colour. Only asked when the spellings
            // differ, so the common paint costs no syscall.
            let path = std::fs::canonicalize(path).ok()?;
            sources.iter().position(|s| s.path == path)
        })
        .map(source_tint)
}

/// What the export card offers, top to bottom. Bitrate is the only thing the
/// encoder actually takes: the codec and the container are what this program
/// can write and nothing else, so the card states them rather than offering
/// them.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Quality {
    Auto,
    Low,
    Medium,
    High,
    Custom,
}

impl Quality {
    const ALL: [Quality; 5] = [
        Quality::Auto,
        Quality::Low,
        Quality::Medium,
        Quality::High,
        Quality::Custom,
    ];

    fn label(self) -> &'static str {
        match self {
            Quality::Auto => "Auto",
            Quality::Low => "Low",
            Quality::Medium => "Medium",
            Quality::High => "High",
            Quality::Custom => "Custom",
        }
    }

    /// The figure the row stands for, said in the units the row is chosen by.
    fn detail(self, custom_mbps: u32) -> String {
        match self {
            Quality::Auto => "from the picture size and frame rate".to_string(),
            Quality::Custom => {
                format!("{custom_mbps} Mbps — wheel or ± steps, n types one, {MBPS_MIN}–{MBPS_MAX}")
            }
            other => format!(
                "{} Mbps",
                export_settings(other, 0, Format::Mp4, DEFAULT_AUDIO_KBPS)
                    .bitrate
                    .unwrap_or_default()
                    / 1_000_000
            ),
        }
    }
}

/// The export card's format rows: the key that picks one, its name, and what it
/// writes -- or, where this program cannot write it, the reason it cannot. A
/// format with no entry at all would read as an oversight, and a menu of three
/// as a claim that nothing else exists -- so the refusals are rows too, dimmed
/// and unclickable.
///
/// `None` is exactly that kind of row, and there are two left. MP3 stopped
/// being one when `rusty_mp3` gave this project an Apache-2.0 encoder (the LGPL
/// `shine-rs` was the licence question, and it is not the only encoder any
/// more), and HEVC stopped being one when OxideAV's pure-Rust H.265 gave it an
/// encoder — an *intra-only* one, which the rows say rather than let a user
/// find out from the size of the file. VP9 is the one this program still only
/// *reads*: the plugin decodes it and there is no encoder for it here, so it is
/// a row for the reason the refusals are rows at all — a codec that opens but
/// never comes back out is exactly the gap a user would otherwise go looking
/// for. AAC is not a row at all: it is what both containers' sound *is*, never a
/// file of its own.
///
/// A codec is one row, and the boxes it can be written into are the row's
/// containers: the same AV1 picture and the same AAC track go into a Matroska
/// file or into an mp4, and which one a user needs is about what has to play
/// the file, not about the encode. So the container is asked *once*, in a row
/// of its own, and only where there is more than one to ask about -- five
/// picture rows to read past were four of them saying the same codec twice.
const FORMATS: [(&[Format], &str, &str, &str); 8] = [
    (
        &[Format::Mp4],
        "m",
        "H.264",
        "plays everywhere · AAC sound · MP4 only",
    ),
    (
        &[Format::Av1, Format::Av1Mp4],
        "a",
        "AV1",
        "smallest file for the picture · AAC sound",
    ),
    (
        &[Format::Hevc, Format::HevcMp4],
        "h",
        "HEVC",
        "intra-only, every frame a cut point — large files",
    ),
    (&[Format::Wav], "w", "WAV", "16-bit PCM — audio only"),
    (&[Format::Flac], "f", "FLAC", "lossless — audio only"),
    (&[Format::Mp3], "p", "MP3", "MPEG-1 Layer III — audio only"),
    (
        &[Format::Ogg],
        "o",
        "OGG",
        "Vorbis (rusty_vorbis) — quality-coded, stereo",
    ),
    (&[], "", "VP9", "AV1 above replaces it"),
];

/// The codec row a key picks, landed in the container already chosen: pressing
/// `a` after an mp4 gives AV1 *in mp4*, because a box decided once is not a
/// question again. A letter of the row's own name, spelled out in the table
/// rather than taken from the initial (`MP4` and `MP3` share one). Never a
/// digit -- those are the bitrate field's. `None` for every other stroke, the rows
/// that cannot be picked included.
fn format_key(key: &str, current: Format) -> Option<Format> {
    FORMATS
        .into_iter()
        .filter(|(_, stroke, ..)| !stroke.is_empty() && stroke.eq_ignore_ascii_case(key))
        .find_map(|(row, ..)| same_box(row, current))
}

/// The boxes one codec may be written into, in the order its container row
/// cycles them. Empty for a codec this program cannot write at all.
fn containers(format: Format) -> &'static [Format] {
    FORMATS
        .into_iter()
        .map(|(row, ..)| row)
        .find(|row| row.contains(&format))
        .unwrap_or(&[])
}

/// This row's format under the container `current` is already in, or the row's
/// first when it has no such box: an AV1 picked from a WAV lands in Matroska,
/// and picked from an mp4 stays in the mp4.
fn same_box(row: &[Format], current: Format) -> Option<Format> {
    row.iter()
        .copied()
        .find(|f| f.ext() == current.ext())
        .or_else(|| row.first().copied())
}

/// The next box for the same codec, wrapping -- what the container row's key
/// does. The format itself for a codec with only one, so the stroke cannot
/// change what it is not offering.
fn next_container(format: Format) -> Format {
    let row = containers(format);
    let at = row.iter().position(|&f| f == format).unwrap_or(0);
    row.get((at + 1) % row.len().max(1))
        .copied()
        .unwrap_or(format)
}

/// The next rate the Sound row offers, wrapping -- what its key does, and what
/// the row itself names so the stroke is never a guess. [`next_container`]'s
/// shape, for the same reason: one place decides what "next" means.
fn next_audio_kbps(kbps: u32) -> u32 {
    let at = AUDIO_KBPS.iter().position(|&k| k == kbps).unwrap_or(0);
    AUDIO_KBPS[(at + 1) % AUDIO_KBPS.len()]
}

/// Why the quality rows say nothing about this format, or `None` where they
/// decide the picture. Only a picture encoder is given a bitrate here: the two
/// lossless audio formats have none to give and MP3 is written at one fixed
/// figure, so a live row over either would be a control that changes nothing.
fn bitrate_refusal(format: Format) -> Option<&'static str> {
    match format {
        Format::Wav | Format::Flac => Some("lossless audio — no bitrate to pick"),
        Format::Mp3 => Some("sound only — its rate is the Sound row"),
        // The guard is the question and not a list of names: an audio format
        // added without a line of its own above would otherwise show live
        // quality rows over a file that has no picture to spend a bitrate on.
        // OGG is exactly that format -- and it wants this sentence rather than
        // one of its own, because its *Sound* row already says the Vorbis half
        // ("quality-coded — Vorbis holds no rate to pick") and two rows saying
        // the same thing is one of them wasted.
        _ if !format.has_video() => Some("sound only — no picture to spend a bitrate on"),
        _ => None,
    }
}

/// What one of the colour card's own strokes does. Its keys are card-local --
/// they mean nothing outside it -- so they are a table here rather than keymap
/// bindings, exactly as the export card's format initials are. Listed in
/// `keymap::FIXED` all the same, which is how the keys menu still says so.
enum ColorKey {
    Close,
    /// Steps down the four sliders, wrapping.
    Band(usize),
    /// Moves the picked slider, in [`COLOR_STEP`]s.
    Nudge(f32),
    Reset,
}

/// Why the silence card has nothing to scan on that clip, in its own voice: the
/// lane and index the user picked, the file it is of, and which of the two
/// soundless things it is. One place, because a still and a silent video are
/// the same answer to the same question -- "a box with a larger size than it"
/// is what the *demuxer* would say about a png, and it is not an answer.
///
/// Costs nothing: the scan reads a file and writes marks, so a refusal here
/// leaves the project (and its undo history) exactly where it was.
fn unscannable(lane: Lane, idx: usize, path: &Path) -> String {
    let what = match engine::is_image(path) {
        true => "is a picture",
        false => "is silent",
    };
    format!(
        "{} clip {} has no audio to scan — {} {what}",
        lane.label(),
        idx + 1,
        file_name(path)
    )
}

/// The half of a take whose *sound* the silence card scans: a link is one span
/// on however many lanes, so a card opened on the picture opens on the sound it
/// is grouped with. That is the lane the waveform is drawn on, and so the lane
/// the marks have to land on to be read against it -- and the ranges agree,
/// because a group is one span.
///
/// The clip itself for one already on an audio lane, for a detached picture,
/// and for a take whose sound is not on any lane: there is nothing better to
/// open on, and the refusal for a source with no audio at all is `scan`'s.
fn audio_half(session: &PlaybackSession, (lane, idx): (Lane, usize)) -> (Lane, usize) {
    if lane.kind == LaneKind::Audio {
        return (lane, idx);
    }
    let Some(link) = session.lane_clips(lane).get(idx).and_then(|c| c.link) else {
        return (lane, idx);
    };
    session
        .lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Audio)
        .find_map(|l| {
            session
                .lane_clips(l)
                .iter()
                .position(|c| c.link == Some(link))
                .map(|i| (l, i))
        })
        .unwrap_or((lane, idx))
}

fn color_key(key: &str) -> Option<ColorKey> {
    Some(match key {
        ESCAPE => ColorKey::Close,
        "down" => ColorKey::Band(1),
        "up" => ColorKey::Band(COLOR_BANDS.len() - 1),
        "right" => ColorKey::Nudge(1.),
        "left" => ColorKey::Nudge(-1.),
        "r" => ColorKey::Reset,
        _ => return None,
    })
}

/// The band'th control of a grade, to read or to write. The order is
/// [`COLOR_BANDS`]', which is the order the card lists them in.
fn band_mut(params: &mut ColorParams, band: usize) -> &mut f32 {
    match band {
        0 => &mut params.brightness,
        1 => &mut params.contrast,
        2 => &mut params.saturation,
        _ => &mut params.tint,
    }
}

/// The line under the rows: what the picked format really writes, in the terms
/// a file is judged by afterwards.
/// The next policy round the cycle, in the order the action's label reads.
/// Every fit policy, in the order the list offers them -- which is the order
/// [`next_fit`] steps through them, pinned by the test below: a list and a
/// stroke that disagreed about what comes next would be two settings.
const FITS: [FitPolicy; 4] = [
    FitPolicy::Fit,
    FitPolicy::Fill,
    FitPolicy::Stretch,
    FitPolicy::Center,
];

fn next_fit(fit: FitPolicy) -> FitPolicy {
    match fit {
        FitPolicy::Fit => FitPolicy::Fill,
        FitPolicy::Fill => FitPolicy::Stretch,
        FitPolicy::Stretch => FitPolicy::Center,
        FitPolicy::Center => FitPolicy::Fit,
    }
}

/// What a person calls one, said as what it does to the picture.
fn fit_label(fit: FitPolicy) -> &'static str {
    match fit {
        FitPolicy::Fit => "fit (whole picture, bars)",
        FitPolicy::Fill => "fill (cropped, no bars)",
        FitPolicy::Stretch => "stretch (aspect broken)",
        FitPolicy::Center => "centre (1:1, no resample)",
    }
}

/// Every project resolution on offer, largest first: [`RESOLUTIONS`] with the
/// media's own size cycled in at its place by size -- so a project already at a
/// listed size does not see it twice, and the media's own shape, whatever it is,
/// is always on the list. The one order both the stroke and the list use.
fn resolution_ladder(native: (u32, u32)) -> Vec<(u32, u32)> {
    let mut sizes: Vec<(u32, u32)> = RESOLUTIONS.to_vec();
    if !sizes.contains(&native) {
        // By area, descending, like the list itself: the cycle then reads as one
        // ladder rather than a list with a stray rung at the end.
        let at = sizes
            .iter()
            .position(|&(w, h)| {
                u64::from(w) * u64::from(h) < u64::from(native.0) * u64::from(native.1)
            })
            .unwrap_or(sizes.len());
        sizes.insert(at, native);
    }
    sizes
}

/// The resolution list's rows: every rung of the ladder, the media's own said
/// so, and the one in force marked. A size is named by its height the way the
/// button that opens the list names it, with the full figure beside it.
fn resolution_choices(current: (u32, u32), native: (u32, u32)) -> Vec<ChoiceRow> {
    resolution_ladder(native)
        .into_iter()
        .map(|(w, h)| {
            (
                Choice::Size(w, h),
                format!("{h}p").into(),
                match (w, h) == native {
                    // Short enough to sit beside the label inside `MENU_W`:
                    // the longer phrase lost its last word to the truncation.
                    true => format!("{w}x{h} · the media's own"),
                    false => format!("{w}x{h}"),
                }
                .into(),
                (w, h) == current,
            )
        })
        .collect()
}

/// Every project frame rate on offer, slowest first: [`FRAME_RATES`] with the
/// media's own cycled in at its place by speed, so a project already cut at a
/// listed rate does not see it twice and the media's own rate -- the one a
/// project moved off it has no other way back to -- is always there.
/// [`resolution_ladder`]'s rule, for the other setting the project has of its
/// own.
fn frame_rate_ladder(native: f64) -> Vec<f64> {
    let mut rates = FRAME_RATES.to_vec();
    // Bit for bit: 23.976023976... is not 23.976, and a rate that read as
    // "already listed" when it is not would take the media's own off the list.
    if !rates.contains(&native) {
        let at = rates
            .iter()
            .position(|&fps| fps > native)
            .unwrap_or(rates.len());
        rates.insert(at, native);
    }
    rates
}

/// The rate list's rows: every rung of the ladder, the media's own said so, and
/// the one in force marked. Named as a person writes a rate ([`fps_label`]),
/// with what it is for beside it -- short, or the row loses its tail to the
/// truncation the resolution list already met.
fn fps_choices(current: f64, native: f64) -> Vec<ChoiceRow> {
    frame_rate_ladder(native)
        .into_iter()
        .map(|fps| {
            (
                Choice::Fps(fps),
                format!("{} fps", fps_label(fps)).into(),
                match fps == native {
                    true => "the media's own".to_string(),
                    // The rates that are a ratio are the ones nobody can tell
                    // from their neighbour by the label alone.
                    false => match (fps - fps.round()).abs() < 0.001 {
                        true => String::new(),
                        false => "NTSC".to_string(),
                    },
                }
                .into(),
                fps == current,
            )
        })
        .collect()
}

/// The fit list's rows: all four policies against the canvas they place a
/// picture on, since the word alone ("fill") says nothing about the size it is
/// filling -- which is the very thing the notice says after a stroke.
fn fit_choices(lane: Lane, idx: usize, current: FitPolicy, (w, h): (u32, u32)) -> Vec<ChoiceRow> {
    FITS.into_iter()
        .map(|fit| {
            (
                Choice::Fit(lane, idx, fit),
                fit_label(fit).into(),
                // The canvas alone, worded as the resolution list words a size:
                // the policy names are long and anything wordier here loses its
                // tail to the truncation.
                format!("{w}x{h}").into(),
                fit == current,
            )
        })
        .collect()
}

/// The sound-rate list's rows: every offered rate, the one in force marked, and
/// what each buys said in the fewest words that fit beside the label (a longer
/// phrase loses its tail to `MENU_W`'s truncation, as the two lists above say).
fn audio_rate_choices(current: u32) -> Vec<ChoiceRow> {
    AUDIO_KBPS
        .into_iter()
        .enumerate()
        .map(|(n, kbps)| {
            (
                Choice::AudioRate(kbps),
                format!("{kbps} kbps").into(),
                match (kbps, n) {
                    (DEFAULT_AUDIO_KBPS, _) => "the default",
                    (_, 0) => "smallest file",
                    (k, _) if k < DEFAULT_AUDIO_KBPS => "smaller file",
                    _ => "better sound",
                }
                .into(),
                kbps == current,
            )
        })
        .collect()
}

/// How a rendition is named where a person reads it: the panel button, the
/// notice and the list row all say the same word ([`engine::tonemap::Preset`]).
fn tone_label(preset: Preset) -> &'static str {
    match preset {
        Preset::Reference => "Reference",
        Preset::Standard => "Standard",
        Preset::Vivid => "Vivid",
    }
}

/// The HDR list's rows: all three renditions, the one in force marked, and what
/// each one *is* beside it -- in the fewest words that fit inside `MENU_W`, the
/// truncation the three lists above already met. Always offered, whatever is on
/// the timeline: a setting that appeared and vanished with the media would be a
/// setting nobody could find, and the row says who it acts on instead.
fn tone_choices(current: Preset) -> Vec<ChoiceRow> {
    Preset::ALL
        .into_iter()
        .map(|preset| {
            (
                Choice::Tone(preset),
                tone_label(preset).into(),
                match preset {
                    Preset::Reference => "BT.2446-A, as published",
                    Preset::Standard => "brighter, player-like",
                    Preset::Vivid => "brightest, richer colour",
                }
                .into(),
                preset == current,
            )
        })
        .collect()
}

/// The next project resolution after `current`, over [`RESOLUTIONS`] with the
/// media's own size cycled in at its place by size -- so the trip round always
/// comes back to the media, whatever odd shape it is, and a project already at a
/// listed size does not see it twice.
fn next_resolution(current: (u32, u32), native: (u32, u32)) -> (u32, u32) {
    let sizes = resolution_ladder(native);
    let at = sizes.iter().position(|&s| s == current);
    // A project at a size nobody listed (a hand-edited file) joins the cycle at
    // the top rather than being stuck.
    sizes[at.map_or(0, |at| (at + 1) % sizes.len())]
}

/// The first of the two lines the card keeps above its button: what will be
/// *inside* the file. [`format_line`]'s codec and box, then the project's own
/// picture size and rate -- which is what a video export is written at however
/// many sizes and rates the media on the timeline are -- and last what the
/// sound will be, or that there is none. Every field here is one `ffprobe`
/// reads back off the finished file, so the line is checkable rather than a
/// promise.
fn summary_head(format: Format, picture: Option<((u32, u32), f64)>, audio: &str) -> String {
    let line = match picture.filter(|_| format.has_video()) {
        Some(((w, h), fps)) => {
            format!("{} · {w}x{h} · {} fps", format_line(format), fps_label(fps))
        }
        None => format_line(format).to_string(),
    };
    join_detail(&line, audio)
}

/// The second: where it lands, roughly how big, and what will encode the
/// picture -- the seat as the probe found it (`…` until it lands, never a
/// guess), which is what the running export then names on its progress line.
fn summary_tail(path: &Path, bytes: Option<u64>, seat: Option<&'static str>, video: bool) -> String {
    let size = bytes.map_or_else(String::new, |bytes| format!("≈ {}", size_label(bytes)));
    let seat = match (video, seat) {
        (true, Some(seat)) => seat,
        (true, None) => "encoder …",
        (false, _) => "",
    };
    join_detail(&join_detail(&file_name(path), &size), seat)
}

/// A frame rate as a person writes it: `30`, not `30.000`, and `23.976` for the
/// rate that is a ratio.
fn fps_label(fps: f64) -> String {
    match (fps - fps.round()).abs() < 0.001 {
        true => format!("{fps:.0}"),
        false => format!("{fps:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string(),
    }
}

/// About how big a *chosen* bitrate makes the file: the picture's bits over the
/// timeline's length, in bytes -- the unit the line is written in is
/// [`size_label`]'s to pick. `None` for `Auto`, whose figure is the encoder's to
/// decide, and for a format with no bitrate at all -- a number nobody picked is
/// not an estimate. The sound and the container's own overhead are not in it,
/// which is why the card says "≈".
fn estimated_bytes(bitrate: Option<u64>, duration: f64) -> Option<u64> {
    let bitrate = bitrate.filter(|&b| b > 0 && duration > 0.)?;
    Some((bitrate as f64 * duration / 8.).round() as u64)
}

/// A size in the largest unit that can state it, [`rate_scale`]'s rule:
/// megabytes for an export of any length, kilobytes below the one a whole
/// megabyte rounds away. A three second clip at the floor bitrate really is
/// 375 kB, and "≈ 0 MB" would be this line saying the file it is about to write
/// is empty -- the one thing the size field is there to deny. Never "0 kB"
/// either: an estimate that exists is at least a kilobyte of file.
fn size_label(bytes: u64) -> String {
    match (bytes as f64 / 1e6).round() as u64 {
        0 => format!("{} kB", (bytes as f64 / 1e3).round().max(1.) as u64),
        mb => format!("{mb} MB"),
    }
}

/// What is in the file and what box it is in, which is the head of the summary.
/// Terse on purpose: the fields after it (size, rate, sound) are what the line
/// is *for*, and a head that spent its width on prose used to push the whole
/// summary onto a second line -- what each codec means is on its row.
fn format_line(format: Format) -> &'static str {
    match format {
        Format::Mp4 => "H.264 · MP4",
        Format::Av1 => "AV1 · MKV",
        Format::Av1Mp4 => "AV1 · MP4",
        Format::Hevc => "HEVC intra · MKV",
        Format::HevcMp4 => "HEVC intra · MP4",
        // The three whose codec *is* their box: naming it twice would be the
        // only field on this line that says nothing.
        Format::Wav => "16-bit PCM · WAV",
        Format::Flac => "FLAC · lossless",
        Format::Mp3 => "MP3 · lossy",
        Format::Ogg => "Vorbis · OGG",
    }
}

/// The row a format is picked by, which is what a refusal calls it: the codec,
/// since the container is a row of its own now -- `AV1`, not the `mkv` such a
/// file is named with.
fn format_label(format: Format) -> &'static str {
    FORMATS
        .iter()
        .find(|(row, ..)| row.contains(&format))
        .map_or("EXPORT", |(_, _, label, _)| *label)
}

/// The destination under a format: `take.export.mp4` becomes `take.export.wav`.
/// The stem is untouched, so a name typed into the save dialog survives a
/// change of mind about the format -- only the extension is the format's to say.
fn retarget(path: &std::path::Path, format: Format) -> PathBuf {
    let mut path = path.to_path_buf();
    path.set_extension(format.ext());
    path
}

/// The card's rows as the engine takes them. `Auto` leaves the bitrate to the
/// exporter, which derives it from the picture; the fixed rows are figures that
/// hold from 720p to 1080p, and a typed one is passed exactly as typed -- the
/// engine clamps every explicit bitrate to 1..50 Mbps (`MAX_EXPLICIT_BITRATE`), so this
/// must not clamp it a second time and disagree about where the edge is.
///
/// The bitrate travels even for an audio format, where the engine ignores it:
/// one settings value, and a row the card has dimmed cannot have been changed.
fn export_settings(
    quality: Quality,
    custom_mbps: u32,
    format: Format,
    audio_kbps: u32,
) -> ExportSettings {
    ExportSettings {
        format,
        // Always travels, exactly as the picture's bitrate does above: the
        // engine ignores it where nothing encodes the sound, and a row the card
        // has dimmed cannot have been changed.
        audio_kbps: Some(audio_kbps),
        bitrate: match quality {
            Quality::Auto => None,
            Quality::Low => Some(2_000_000),
            Quality::Medium => Some(6_000_000),
            Quality::High => Some(12_000_000),
            Quality::Custom => Some(u64::from(custom_mbps) * 1_000_000),
        },
        // No row of its own: the software pin is for a driver that encodes
        // badly, which is a thing about the machine and not about the output --
        // `VE_SW_ENC` already says it, and to the whole run rather than once.
        force_sw: false,
        // The picked track is put on by `start_export`, which is the only
        // caller that writes a file; the rest of them are asking about the
        // bitrate and the format.
        subtitles: Vec::new(),
    }
}

/// What the engine will code an explicit bitrate at, in whole Mbps: outside
/// this it clamps (`export.rs` `MIN_BITRATE`/`MAX_EXPLICIT_BITRATE`), so a
/// number typed past either end would be written as a different one. The field
/// refuses it instead of clamping quietly -- a card that changes the user's
/// number without saying so is the one thing a field like this must never do.
///
/// The ceiling was 20, which was never a limit of any encoder here: it was the
/// top of the range the exporter *derives* an automatic bitrate in, borrowed as
/// the cap on a typed one. A 1080p master or a 4K edit wants more than that, so
/// the asked-for rate has its own ceiling now and this is it.
const MBPS_MIN: u32 = 1;
const MBPS_MAX: u32 = 50;

/// How many digits the field takes. Two reach the ceiling; the third is there so
/// a number *past* it can be typed whole and refused in its own words, rather
/// than being dropped keystroke by keystroke.
const MBPS_DIGITS: usize = 3;

/// A number being typed into a card row: the digits so far and, once a commit
/// has been refused, why. Text-field semantics on a card that has no text field
/// -- typing, backspace, arrows that step, enter that commits and escape that
/// gives up -- held as state and driven by the root's key handler, since
/// nothing in these cards takes gpui focus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NumberEdit {
    text: String,
    refusal: Option<String>,
}

impl NumberEdit {
    /// Starts on the number the row already carries, so backspace edits it
    /// rather than the field opening empty over a value that is still in force.
    /// Zero is no number at all -- it is what the card opens at, before anyone
    /// has typed one.
    fn new(value: u32) -> Self {
        NumberEdit {
            text: match value {
                0 => String::new(),
                v => v.to_string(),
            },
            refusal: None,
        }
    }

    /// A digit against what is there. The one past [`MBPS_DIGITS`] is refused
    /// *out loud*: a keystroke dropped in silence is how the old digit capture
    /// left the card showing a number the user had already typed past.
    fn digit(&mut self, digit: u32) {
        if self.text.chars().count() >= MBPS_DIGITS {
            self.refusal = Some(format!("{MBPS_DIGITS} digits is already past the ceiling"));
            return;
        }
        match char::from_digit(digit, 10) {
            Some(c) => {
                self.text.push(c);
                self.refusal = None;
            }
            None => self.refusal = Some("digits only".into()),
        }
    }

    /// Erases the last digit, and the refusal with it: the number on screen has
    /// changed, so the reason the old one was refused no longer describes it.
    fn backspace(&mut self) {
        self.text.pop();
        self.refusal = None;
    }

    /// The arrows, which is how a number gets picked rather than typed. Steps
    /// from what is in the field -- an empty one starts at the floor, so the
    /// first press up is `MBPS_MIN` and not a jump to some remembered value --
    /// and stays inside the range, because a step is a walk through the legal
    /// numbers rather than a way out of them.
    fn step(&mut self, by: i32) {
        let at = self.text.parse::<i32>().unwrap_or(MBPS_MIN as i32 - by.signum());
        self.text = (at + by)
            .clamp(MBPS_MIN as i32, MBPS_MAX as i32)
            .to_string();
        self.refusal = None;
    }

    /// The number, or `None` with the reason recorded where the row will read
    /// it. Never clamped: 45 committed as 20 is a number the user did not type.
    fn commit(&mut self) -> Option<u32> {
        match commit_mbps(&self.text) {
            Ok(mbps) => Some(mbps),
            Err(why) => {
                self.refusal = Some(why);
                None
            }
        }
    }

    /// What the row shows while it is being typed into: the digits, the caret
    /// that says they are landing *here*, and either the refusal or the two
    /// keys that end the edit.
    fn detail(&self) -> String {
        format!(
            "{}▏ Mbps — {}",
            self.text,
            match &self.refusal {
                Some(why) => why.as_str(),
                None => "enter commits · esc cancels",
            }
        )
    }
}

/// A typed bitrate as the card takes it, or the reason it is not one. The words
/// are the row's: they are what the field shows in place of its hint.
fn commit_mbps(text: &str) -> Result<u32, String> {
    match text.parse::<u32>() {
        Ok(mbps) if (MBPS_MIN..=MBPS_MAX).contains(&mbps) => Ok(mbps),
        Ok(0) => Err(format!("0 is not a rate — {MBPS_MIN}–{MBPS_MAX} Mbps")),
        Ok(mbps) => Err(format!("{mbps} is past the {MBPS_MAX} Mbps ceiling")),
        Err(_) => Err(format!("type a number — {MBPS_MIN}–{MBPS_MAX} Mbps")),
    }
}

/// Whether a stroke gets out of a running export. Escape does, whatever
/// modifiers are held with it, for the same reason it gets out of a capture and
/// out of the overlay: it is this window's way out, and a way out that only
/// works with the right modifiers is not one. Whatever the keymap has on cancel
/// works too, so rebinding it adds a way rather than replacing the one that
/// always worked -- and that binding is still what the progress line shows.
fn cancels_export(key: &str, action: Option<ActionId>) -> bool {
    key == ESCAPE || action == Some(ActionId::CancelExport)
}

/// Where a held key would land, which is the whole of what auto-repeat has to
/// know. [`Player::repeat_scope`] answers it from the same state the handler
/// walks below itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Repeat {
    /// A card with values in it owns the keyboard: equalizer, colour, speed or
    /// silence. Its arrows are sliders.
    Card,
    /// Nobody does, so the keymap answers -- and only one pair of its actions
    /// is a value being moved rather than a thing being done once.
    Keymap,
    /// A stroke is being captured, an export is running, or an overlay with no
    /// value in it is up. Nothing there is worth a repeat.
    Nothing,
}

/// Whether a *held* stroke means it again. One press is always one action; a
/// hold is only ever a value running, so this is what tells the two apart.
///
/// The cards' arrows, because that is what every one of them moves a slider
/// with -- and only the arrows, so the equalizer's `r` cannot flatten five
/// bands forty times a second and the silence card's `enter` cannot cut forty
/// places again on the next tick. Outside a card the volume pair and nothing
/// else: play, cut, delete, save, export and every other binding is a one-shot,
/// exactly as it was when the handler filtered every held key alike.
fn repeats(scope: Repeat, key: &str, action: Option<ActionId>) -> bool {
    match scope {
        Repeat::Card => matches!(key, "up" | "down" | "left" | "right"),
        Repeat::Keymap => matches!(
            action,
            Some(
                ActionId::VolumeUp
                    | ActionId::VolumeDown
                    // A zoom is a value being moved as much as a level is:
                    // held, it runs from the whole timeline down to a handful
                    // of frames and stops there. The fit is one press.
                    | ActionId::ZoomIn
                    | ActionId::ZoomOut
            )
        ),
        Repeat::Nothing => false,
    }
}

/// The keys that are only ever half a chord. gpui delivers a lone modifier
/// press as a keystroke of its own, and taking one as a binding would leave an
/// action that fires the moment the user reaches for any chord that uses it --
/// so a capture waits through them instead.
fn is_bare_modifier(key: &str) -> bool {
    matches!(
        key,
        "control" | "shift" | "alt" | "super" | "platform" | "function" | "fn" | "meta" | "command"
    )
}

/// A clip's share of the lane. A timeline with no length reads as one full-width
/// box rather than as NaN, which gpui would carry into layout.
/// Why this timeline cannot be written in `format`, if it cannot.
///
/// An audio-only timeline is the one both picture formats refuse: every frame of
/// it is a gap, so the file would be a black picture over the sound. The engine
/// refuses it too (`export::start`); this is what greys the row before a
/// destination has been picked.
///
/// It is the *only* reason left. A second audio lane, a speeded clip, a source
/// no mp4 sample table holds: each of those used to grey the MP4 row, because
/// the mp4 path could only *copy* an AAC track. It re-encodes where a copy
/// cannot say what the timeline says (`export::copy_audio`), so none of them is
/// a refusal any more -- and every video format carries the sound, so there is
/// nothing here that is one format's alone.
fn format_refusal(session: &PlaybackSession, format: Format) -> Option<String> {
    if !format.has_video() {
        return None;
    }
    let picture = session
        .lanes()
        .into_iter()
        .any(|lane| lane.kind == LaneKind::Video && !session.lane_clips(lane).is_empty());
    match picture {
        true => None,
        false => Some(format!(
            "no picture — {} would be black; export WAV, FLAC, MP3 or OGG",
            format.name()
        )),
    }
}

/// How tall a column of `lanes` rows is, gaps included -- the panel's own gap
/// between them, since the rows sit in it.
fn lanes_h(lanes: usize) -> f32 {
    match lanes {
        0 => 0.,
        n => n as f32 * LANE_H + (n - 1) as f32 * 8.,
    }
}

/// How many whole lane rows a box this tall shows. At least one: a box too
/// short for a single lane still shows part of one, and "0 shown" would count
/// every lane there is as hidden.
fn lanes_shown(box_h: f32) -> usize {
    (((box_h + 8.) / (LANE_H + 8.)).floor() as usize).max(1)
}

/// How many rows are still below the fold *now*: the count the affordance says
/// out loud, and the reason it is a function of the scroll rather than of the
/// lane count alone. A line that keeps saying "2 more tracks below" after the
/// user has scrolled to the last one is a line that has stopped being true, and
/// an affordance nobody can make go away is read as a bug rather than as an
/// instruction.
///
/// `scrolled` is how far down the column has been taken, in pixels. Rounded to
/// the nearest row: a half-scrolled row is showing, so it is not below.
fn rows_below(total: usize, box_h: f32, scrolled: f32) -> usize {
    let past = ((scrolled / (LANE_H + 8.)).round().max(0.)) as usize;
    total.saturating_sub(past + lanes_shown(box_h))
}

/// The same question for a column whose rows are not one height -- the
/// inspector's sections -- answered in pixels off what the scroll itself
/// reports: how far it may still be taken (`max_offset`) less how far it has
/// been (`offset`, which gpui keeps negative going down).
fn px_below(max_offset_h: f32, offset_y: f32) -> f32 {
    (max_offset_h + offset_y).max(0.)
}

/// What the subtitle strip adds to the panel: its own row and the panel's gap
/// above it, and nothing at all for a timeline with no subtitles on it -- the
/// picture does not pay for a strip that is not drawn.
fn subtitle_strip_h(shown: bool) -> f32 {
    match shown {
        true => SUB_LANE_H + 8.,
        false => 0.,
    }
}

/// How tall the panel is with `lanes` tracks in it: [`PANEL_H`] is sized for the
/// two a project starts with, and every further one adds its own row -- up to
/// [`LANES_MAX`], past which the lane column scrolls instead and the panel stops
/// growing.
fn panel_h(lanes: usize) -> f32 {
    PANEL_H + lanes_h(lanes.clamp(2, LANES_MAX)) - lanes_h(2)
}

/// How tall the timeline region is: its own padding, the timecode line, the
/// ruler and the gaps between them, plus a row per lane. Measured from its
/// parts rather than taken off [`PANEL_H`] -- the button row moved out of it
/// ([`Player::toolbar`]), and a height still carrying that row's pixels is a
/// height that cuts the last lane off the bottom of the window.
fn timeline_h(lanes: usize) -> f32 {
    TIMELINE_FIXED_H + lanes_h(lanes.clamp(2, LANES_MAX))
}

/// 8+8 padding, the timecode line, the 24 px ruler strip and the two 8 px gaps
/// between the three rows, with a couple of px of slack so a taller text line
/// cannot push a lane off the bottom.
const TIMELINE_FIXED_H: f32 = 16. + 18. + 8. + RULER_HIT_H + 8. + 4.;

/// The most of a short window the timeline may take. At the 640x360 floor that
/// is 151 px of the 360, which leaves the picture a region rather than a
/// letterbox stripe -- and the lanes scroll inside it.
const TIMELINE_SHARE: f32 = 0.42;

/// The edit toolbar directly above the timeline: one control's height in its
/// own padding, fixed so nothing in it can push the timeline down.
const TOOLBAR_H: f32 = CONTROL_H + 16.;

/// The top bar: the project's name on the left, the two file actions on the
/// right. Fixed for the reason [`HEADER_H`] is.
const TOPBAR_H: f32 = 36.;

/// The transport strip under the picture -- play, timecode, volume -- where a
/// player's own controls live in every consumer editor.
const TRANSPORT_H: f32 = CONTROL_H + 12.;

/// One press of a zoom key, or one notch of ctrl+wheel.
const ZOOM_STEP: f32 = 1.25;

/// How far one notch of a bare wheel slides the view along, as a share of what
/// is on the bed. A *share* rather than a number of pixels or of seconds: one
/// gesture then moves the same fraction of what is being looked at whether the
/// bed is showing five seconds or five hours, which is the only way a wheel is
/// usable at both ends of the zoom.
const SCROLL_NOTCH_SHARE: f32 = 0.1;

/// How few frames the bed may be narrowed down to. Past this there is nothing
/// left to aim at -- a single frame across a whole window is a wall of colour,
/// not an edit surface.
const ZOOM_MIN_FRAMES: f64 = 8.;

/// How thin a second of timeline may be drawn on a bed that is already showing
/// all of it: the far stop for a *short* project, which has no length of its own
/// worth widening to, so a five second import can still be zoomed out of.
const PPS_MIN: f64 = 1.;

/// How much bed the far stop leaves past the last frame, as a multiple of the
/// timeline's own length -- so a timeline zoomed all the way out ends a sliver
/// short of the window's edge rather than glued to it.
const ZOOM_OUT_MARGIN: f64 = 1.05;

/// How wide a second of timeline is drawn before anyone zooms: a five second
/// import is 200 px of a bed several times that, so a short clip reads as
/// short -- the thing a bed scaled to the content's own length cannot say.
/// [`View::fit`] is the one way back to "the whole timeline across the bed".
const PPS_DEFAULT: f64 = 40.;

/// What the zoom button says it is showing: how much timeline fits on the bed,
/// in the coarsest unit that still tells two zooms apart.
fn span_label(span: f64) -> String {
    match span {
        s if !s.is_finite() || s <= 0. => "—".to_string(),
        // Hours once a timeline is long enough to be measured in them: the far
        // stop follows the content now, so "315m" is a span a user can be sat
        // at, and no one reads a pill in minutes past sixty.
        s if s >= 3600. => format!("{:.1}h", s / 3600.),
        s if s >= 600. => format!("{:.0}m", s / 60.),
        s if s >= 60. => format!("{:.1}m", s / 60.),
        s if s >= 10. => format!("{s:.0}s"),
        s => secs_label(s),
    }
}

/// A span of seconds as a person reads it: one decimal above a second, two
/// below -- [`scaled`]'s rule, for its reason. The tightest zoom is
/// [`ZOOM_MIN_FRAMES`] across the bed, which on the 240 fps slow-motion a phone
/// writes is 0.03s, and a single frame of quiet at 60 fps is 0.02s: one decimal
/// prints both as "0.0s", a pill and a notice saying the thing they are about
/// has no length at all.
fn secs_label(secs: f64) -> String {
    match secs >= 1. {
        true => format!("{secs:.1}s"),
        false => format!("{secs:.2}s"),
    }
}

/// The mapping the whole panel is drawn and clicked through: `pps` pixels to a
/// second of timeline, `start` the moment at the bed's left edge. Absolute --
/// how wide a clip is drawn depends on how long the clip is and on nothing
/// else, so adding a second clip does not redraw the first one narrower and
/// zooming out always makes every box smaller.
///
/// The one place seconds become pixels: every box, the playhead, a seek and a
/// trim all go through it, so none of them can drift away from the others when
/// the view moves. What clamps it -- the stops, the scroll, the fit -- needs the
/// bed it is drawn on and lives on [`View`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct Scale {
    pps: f64,
    start: f64,
}

impl Default for Scale {
    fn default() -> Self {
        Scale {
            pps: PPS_DEFAULT,
            start: 0.,
        }
    }
}

impl Scale {
    /// Where a moment sits on the bed, in pixels from its left edge. Negative
    /// for a moment scrolled off to the left, which is exactly the offset a
    /// half-visible clip is drawn at.
    fn px_at(self, at: f64) -> f32 {
        ((at - self.start) * self.pps) as f32
    }

    /// How wide a stretch of `len` seconds is drawn. Wider than the bed once
    /// zoomed in, which is the point of zooming in; never negative, which gpui
    /// has no meaning for.
    fn width_px(self, len: f64) -> f32 {
        (len * self.pps).max(0.) as f32
    }

    /// The moment `x` pixels along the bed is pointing at: the inverse of
    /// [`Scale::px_at`], and what every seek and every trim reads. Clamped at
    /// the head of the timeline only -- there is bed past the last frame now,
    /// and a tail dragged into it is a longer clip, not an error.
    fn time_at(self, x: f32) -> f64 {
        if self.pps > 0. {
            (self.start + f64::from(x) / self.pps).max(0.)
        } else {
            self.start
        }
    }

    /// [`SNAP_PX`] in timeline frames at the scale the bed is drawn at: a snap
    /// is a distance on screen, so zoomed right in it is worth less than a frame
    /// (no snap at all, which is what a hand placing single frames wants) and
    /// zoomed out it is worth many.
    fn snap_frames(self, fps: f64) -> u32 {
        if self.pps > 0. {
            (SNAP_PX / self.pps * fps) as u32
        } else {
            0
        }
    }
}

/// A [`Scale`] against the bed it is drawn on and the timeline it is drawn
/// from. The bed's width is what turns a scale into "how much is on screen",
/// and that is all the stops, the scroll clamp and the fit are made of.
///
/// Built per use out of [`Player::view`] and thrown away again -- the state is
/// the `Scale`, and this is what a bed of `bed` px showing `duration` seconds
/// at `fps` may do to it. So no call site can measure a moment against a bed or
/// a duration that another one did not.
#[derive(Clone, Copy, Debug, PartialEq)]
struct View {
    scale: Scale,
    bed: f32,
    duration: f64,
    fps: f64,
}

impl View {
    /// How much of the timeline is on the bed, in seconds.
    fn span(self) -> f64 {
        if self.scale.pps > 0. {
            f64::from(self.bed) / self.scale.pps
        } else {
            0.
        }
    }

    /// The two stops, in pixels per second: [`ZOOM_MIN_FRAMES`] across the bed
    /// is as tight as it goes, and as wide as it goes is the whole timeline on
    /// the bed with [`ZOOM_OUT_MARGIN`] to spare -- the far stop is *relative*,
    /// so the end of a five hour timeline is reachable for the same reason the
    /// end of a five minute one is. A fixed far stop could not say that: a
    /// timeline longer than it had an end no zoom could reach, which is the bug
    /// this is. Short of [`PPS_MIN`] the timeline's own length is not worth
    /// widening to and the pixel takes over, so a short import can still be
    /// zoomed out of and the resting scale is nobody's content.
    ///
    /// `None` on a bed that was never painted -- there is nothing to measure
    /// them against yet, and a stop guessed off a zero width would throw away a
    /// zoom the user asked for.
    fn stops(self) -> Option<(f64, f64)> {
        (self.bed > 0.).then(|| {
            let bed = f64::from(self.bed);
            let whole = match self.duration > 0. {
                true => bed / (self.duration * ZOOM_OUT_MARGIN),
                false => PPS_MIN,
            };
            let min = whole.min(PPS_MIN);
            (min, (bed * self.fps / ZOOM_MIN_FRAMES).max(min))
        })
    }

    /// Clamped to the bed it draws on: between the two stops, and never
    /// scrolled past either end of the timeline. Unlike the fractional view
    /// this replaced, the *resting* scale is not the content -- a five second
    /// timeline zooms out as far as an hour long one does, and both are drawn
    /// at [`PPS_DEFAULT`] until someone zooms. Only the far stop knows the
    /// length, and only once the length is worth more than [`PPS_MIN`].
    fn settled(self) -> Scale {
        let pps = match self.scale.pps.is_finite() && self.scale.pps > 0. {
            true => self.scale.pps,
            false => PPS_DEFAULT,
        };
        let start = match self.scale.start.is_finite() {
            true => self.scale.start,
            false => 0.,
        };
        let Some((min, max)) = self.stops() else {
            return Scale {
                pps,
                start: start.max(0.),
            };
        };
        let pps = pps.clamp(min, max);
        let span = f64::from(self.bed) / pps;
        Scale {
            pps,
            start: start.clamp(0., (self.duration - span).max(0.)),
        }
    }

    /// Zoomed by `factor` about `anchor` (pixels along the bed): whatever moment
    /// was under that point stays under it, so a zoom magnifies what was being
    /// looked at rather than throwing it off the edge.
    fn zoomed(self, factor: f32, anchor: f32) -> Scale {
        let at = self.scale.time_at(anchor);
        // Clamped *before* the offset is worked out, not after: a press that
        // runs into either stop must still leave the anchor where it is, and a
        // start measured against a scale the stop then took away would slide it.
        let raw = self.scale.pps * f64::from(factor);
        let pps = match self.stops() {
            Some((min, max)) => raw.clamp(min, max),
            None => raw,
        };
        View {
            scale: Scale {
                pps,
                start: at - f64::from(anchor) / pps,
            },
            ..self
        }
        .settled()
    }

    /// Slid along by `by` pixels, later in the timeline for a positive one: the
    /// one move that changes what is on the bed without changing how wide a
    /// second is drawn, which is what a bare wheel does. Clamped by
    /// [`View::settled`] like every other move, so a run at either end stops at
    /// the end rather than scrolling the timeline off the bed -- and the extent
    /// it stops against is the content's own length, never a number.
    fn scrolled(self, by: f32) -> Scale {
        let pps = self.settled().pps;
        View {
            scale: Scale {
                pps,
                start: self.scale.start + f64::from(by) / pps,
            },
            ..self
        }
        .settled()
    }

    /// The whole timeline across the bed. The one place the content's own
    /// length sets the scale, and the only one -- everywhere else a second is a
    /// second -- because this is a user pressing a key that asks for exactly
    /// that.
    fn fit(self) -> Scale {
        let pps = match self.duration > 0. && self.bed > 0. {
            true => f64::from(self.bed) / self.duration,
            false => PPS_DEFAULT,
        };
        View {
            scale: Scale { pps, start: 0. },
            ..self
        }
        .settled()
    }

    /// The scale a playhead at `at` needs: the same one while it is on the bed,
    /// and one centred on it once it has run off -- which is how a zoomed-in
    /// timeline scrolls, during playback and after a seek alike. With the whole
    /// timeline on the bed this can never fire, so a panel showing all of it is
    /// untouched by it.
    fn following(self, at: f64) -> Scale {
        // Nothing is drawn yet, so nothing has run off anything.
        if self.bed <= 0. {
            return self.scale;
        }
        let scale = self.settled();
        if self.shows(at) {
            return scale;
        }
        let span = f64::from(self.bed) / scale.pps;
        View {
            scale: Scale {
                start: at - span / 2.,
                ..scale
            },
            ..self
        }
        .settled()
    }

    /// Whether the moment `at` is on the bed as it is drawn now. The one
    /// question both halves of the follow ask -- [`View::following`] to decide
    /// whether to chase a head that has run off, and the render to decide when
    /// a hand's own scroll ([`Player::panned`]) has been caught up with and the
    /// follow may have the view back -- so the two can never disagree about
    /// where the edge of the bed is.
    fn shows(self, at: f64) -> bool {
        if self.bed <= 0. {
            return false;
        }
        let scale = self.settled();
        at >= scale.start && at <= scale.start + f64::from(self.bed) / scale.pps
    }
}

/// Whether this clip is a whole take, i.e. whether deleting it may close the
/// hole under it: a take is what the first pair of lanes carries between them,
/// `V1`'s picture and the sound grouped with it, and dropping one moves the
/// frames after it on every lane.
///
/// Everything else is a half or a layer, and is *lifted* instead: a half whose
/// picture was lifted (what a lift leaves behind) has no take to ripple, and a
/// clip on a further lane is laid over the timeline rather than part of it --
/// closing a hole under it would drag the take beneath out of step with it.
fn whole_take(session: &PlaybackSession, lane: Lane, idx: usize) -> bool {
    let Some(clip) = session.lane_clips(lane).get(idx) else {
        return false;
    };
    let paired = || {
        session
            .lanes()
            .into_iter()
            .filter(|&other| other != lane)
            .flat_map(|other| session.lane_clips(other))
            .any(|o| o.link.is_some() && o.link == clip.link)
    };
    match (lane.kind, lane.ord) {
        (_, 1..) => false,
        // The picture of a take -- unless the take has been taken apart: a
        // detached picture (a group id no other lane carries, which is also what
        // a lift of the sound leaves) is a half like the sound is, and a ripple
        // under it would drag away the very half it was detached from. A clip in
        // no group at all is not a half but a placement, and on `V1` a placement
        // is the take there is.
        (LaneKind::Video, _) => clip.link.is_none() || paired(),
        // The sound of a take, only while the take is still there: its group is
        // carried by a clip on some other lane.
        (LaneKind::Audio, _) => paired(),
    }
}

/// The clip a Group would pair this one with: the first clip on another track,
/// in the order the lanes are drawn, covering exactly the same frames and not in
/// this clip's group already. Exactly the same frames because that is all a
/// group id can mean (engine `links_are_consistent`), which is what leaves
/// nothing for a second click to choose. `None` when no track has one, and the
/// notice says so.
fn span_partner(session: &PlaybackSession, lane: Lane, idx: usize) -> Option<(Lane, usize)> {
    let clip = *session.lane_clips(lane).get(idx)?;
    let matches = |other: Lane| {
        let i = session.lane_clips(other).iter().position(|c| {
            (c.start, c.end()) == (clip.start, clip.end())
                && !(c.link.is_some() && c.link == clip.link)
        })?;
        Some((other, i))
    };
    // Sound before picture (and picture before sound): "group this" means the
    // other half of the take, and a project whose audio lane was added after a
    // second video one has that half *after* the layer in storage order -- which
    // is the order the lanes come in. A same-kind lane is still groupable (V1
    // and V2 may be one take), but only where no opposite one covers the span.
    let (opposite, same): (Vec<Lane>, Vec<Lane>) = session
        .lanes()
        .into_iter()
        .filter(|&other| other != lane)
        .partition(|other| other.kind != lane.kind);
    opposite.into_iter().chain(same).find_map(matches)
}

/// Whether a click marks this clip: the clip that was clicked always, and the
/// other lane's clip of the same group with it. A clip whose group has no other
/// half -- what a lift leaves behind -- marks only itself, which is what makes a
/// detached half separately deletable.
fn marked(
    here: (Lane, usize),
    link: Option<u32>,
    sel: Option<(Lane, usize)>,
    sel_link: Option<u32>,
) -> bool {
    sel == Some(here) || (link.is_some() && link == sel_link)
}

/// Whether a clip is wide enough to be worth naming.
fn show_label(w: f32) -> bool {
    w >= LABEL_MIN_W
}

/// The clip a trim is *showing*, worked out the way `Project::trim` will write
/// it: the timeline room the edge leaves is turned into source frames by the one
/// conversion that exists for it ([`Speed::fit`]), and the box is drawn from
/// that. The preview and the commit are then the same arithmetic -- a box let go
/// of stays where the hand left it at every rate. Assigning the timeline count
/// straight to the source field, as this used to, drew a speeded clip's tail
/// moving at the wrong rate (it snapped on release) and drew a *head* trim
/// moving the clip's other edge, since the length it implied was not the length
/// the release would commit.
///
/// A still grows forward from source frame 0 instead: every frame of it is the
/// same picture, so there is no earlier one for an in-point to walk back to.
///
/// Room too narrow to hold one source frame is the edit the engine refuses, and
/// the box is drawn unchanged rather than as something that will not be
/// committed.
fn trimmed_clip(clip: Clip, edge: Edge, to: u32, still: bool) -> Clip {
    // An edge that has not moved is not an edit, and `Project::trim` refuses it
    // as one: the press that starts a drag must draw the clip it pressed, not a
    // clip a rounding narrower.
    if to
        == match edge {
            Edge::Start => clip.start,
            Edge::End => clip.end(),
        }
    {
        return clip;
    }
    match edge {
        Edge::Start => {
            // What survives is measured from the *end* -- the frames that stay
            // play what they always played, which is what makes this a trim.
            let Some(keep) = clip.speed.fit(clip.end().saturating_sub(to)) else {
                return clip;
            };
            match still {
                true => Clip {
                    in_frame: 0,
                    out_frame: keep,
                    start: to,
                    ..clip
                },
                false => Clip {
                    in_frame: clip.out_frame - keep.min(clip.out_frame),
                    start: to,
                    ..clip
                },
            }
        }
        Edge::End => match clip.speed.fit(to.saturating_sub(clip.start)) {
            Some(keep) => Clip {
                out_frame: clip.in_frame + keep,
                ..clip
            },
            None => clip,
        },
    }
}

/// Which index on its own lane a dragged clip is at *now*: the one it was picked
/// up at while nothing has moved, and wherever the clip itself has slid to when
/// an edit during the drag rippled the lane's indices -- a delete, an undo or a
/// paste from a stroke, none of which gpui's frozen drag payload hears about.
/// `None` when the clip is gone altogether, and then the drop is not an edit at
/// all: moving whatever slid into its place is the one thing the hand did not
/// ask for. A lane's clips are disjoint and sorted, so at most one of them can
/// be the clip that was picked up.
fn live_idx(clips: &[Clip], idx: usize, clip: Clip) -> Option<usize> {
    match clips.get(idx) {
        Some(&at) if at == clip => Some(idx),
        _ => clips.iter().position(|&c| c == clip),
    }
}

/// The part of a clip's box that is on the bed, in the box's own pixels:
/// `(left, width)` of its intersection with the visible strip. Everything drawn
/// *inside* a box -- its waveform, its name, its speed badge -- is placed in
/// here rather than at the box's own edges, which at a deep zoom sit thousands
/// of pixels off either side of the screen: a label out there is a label nobody
/// can read, and a waveform out there is a path with a point per two pixels of a
/// width nobody can see. A bed that has not been measured yet answers with the
/// whole box, which is what was drawn before there was a bed to clip to.
fn visible_slice(left: f32, width: f32, bed: f32) -> (f32, f32) {
    if bed <= 0. {
        return (0., width);
    }
    let from = (-left).clamp(0., width);
    let to = (bed - left).clamp(from, width);
    (from, to - from)
}

/// Scales an envelope to its own loudest sample, so a quietly mastered source
/// still draws as a shape. The fixtures peak around an eighth of full scale, and
/// an eighth of a 30 px lane is a flat line -- which says "silent" about a file
/// that is not.
fn normalise(mut peaks: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    let loudest = peaks.iter().fold(0f32, |m, &(lo, hi)| m.max(-lo).max(hi));
    if loudest > 0. {
        for (lo, hi) in &mut peaks {
            *lo /= loudest;
            *hi /= loudest;
        }
    }
    peaks
}

/// The min/max envelope of `peaks` over the source seconds `from..to`, as
/// `(x, top, bottom)` columns of a `w` x `h` box. Every point is inside that box
/// -- a clip's waveform cannot paint over its neighbour -- and every column is
/// at least a pixel tall, so silence reads as a line through the middle rather
/// than as a polygon with no area.
fn envelope(peaks: &[(f32, f32)], from: f64, to: f64, w: f32, h: f32) -> Vec<(f32, f32, f32)> {
    if peaks.is_empty() || w <= 0. || h <= 0. {
        return Vec::new();
    }
    let cols = ((w / WAVE_COL).ceil().max(1.) as usize).min(WAVE_COLS_MAX);
    let mid = h / 2.;
    (0..=cols)
        .map(|col| {
            let along = col as f64 / cols as f64;
            let at = from + (to - from) * along;
            // Casting a float to an integer saturates in Rust, so a source
            // second past the end of the peaks clamps rather than wrapping.
            let bucket = ((at * f64::from(WAVE_BPS)) as usize).min(peaks.len() - 1);
            let (lo, hi) = peaks[bucket];
            let top = (mid - hi.clamp(0., 1.) * mid).min(mid - 0.5);
            let bottom = (mid - lo.clamp(-1., 0.) * mid).max(mid + 0.5);
            (w * along as f32, top.max(0.), bottom.min(h))
        })
        .collect()
}








/// Records its parent's laid-out box: gpui hands a mouse listener the window
/// position only, and the ruler sits behind the panel's padding. Paints
/// nothing and takes no hitbox of its own, so the click still reaches the bar.
fn bounds_probe(into: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(move |bounds, _, _| into.set(bounds), |_, _, _, _| ())
        .absolute()
        .size_full()
}

/// Where along an element a click landed, 0..1. An element that was never
/// painted has no width, and reads as its start rather than as NaN.
fn frac_along(x: Pixels, bounds: Bounds<Pixels>) -> f32 {
    if bounds.size.width <= px(0.) {
        return 0.;
    }
    ((x - bounds.left()) / bounds.size.width).clamp(0., 1.)
}

/// Where along an element a click landed, in pixels from its own left edge:
/// [`frac_along`] in the units the timeline is drawn in, since a [`Scale`]
/// measures in pixels and not in shares of a bed. Clamped to the element -- a
/// drag that slid off the end names its end -- and an element that was never
/// painted reads as its start.
fn px_along(x: Pixels, bounds: Bounds<Pixels>) -> f32 {
    f32::from(x - bounds.left()).clamp(0., f32::from(bounds.size.width).max(0.))
}

/// One turn of the wheel as a single number, positive for a turn *up*, in
/// whatever units the device sends (a mouse counts lines, a touchpad pixels).
/// A tilt wheel's own axis counts as the same gesture: it is the sideways
/// scroll a mouse that has one sends, and the controls this drives have one
/// direction each, so the two axes are one answer here.
fn wheel_delta(event: &ScrollWheelEvent) -> f32 {
    let (dx, dy) = match event.delta {
        ScrollDelta::Lines(d) => (d.x, d.y),
        ScrollDelta::Pixels(d) => (f32::from(d.x), f32::from(d.y)),
    };
    match dy == 0. {
        true => dx,
        false => dy,
    }
}

/// The frame a dropped clip's head lands on: `raw`, unless one of `marks` is
/// within `tol` frames of where its head -- or its tail, `len` frames along --
/// would come to rest, in which case that edge wins. The snap every timeline
/// has: clips meet exactly instead of a frame apart, and the hand does not have
/// to aim. The nearest mark wins, and a head landing on one beats a tail at the
/// same distance -- what was dragged is the head.
fn snapped(raw: u32, len: u32, tol: u32, marks: &[u32]) -> u32 {
    let mut best: Option<(u32, u32)> = None;
    for &mark in marks {
        // Head on the mark, then tail on it -- a clip dragged up against the
        // take in front of it snaps by whichever end reaches first.
        for start in [Some(mark), mark.checked_sub(len)] {
            let Some(start) = start else { continue };
            let d = start.abs_diff(raw);
            if d <= tol && best.is_none_or(|(near, _)| d < near) {
                best = Some((d, start));
            }
        }
    }
    best.map_or(raw, |(_, start)| start)
}

/// The edges worth landing on, off *every* lane: both ends of every clip on the
/// timeline, less `skip` -- the clip being dragged, which does not snap to where
/// it already is -- and less the other halves of its group, which travel with
/// it. `skip` is a lane's place in `lanes` and an index into it. The playhead
/// and the head of the timeline go on the end: a clip meets the cursor and the
/// start of the show as readily as it meets another take.
///
/// All lanes rather than the one being dropped on, because a cut is made across
/// the timeline: a title on V2 lines up with the shot under it, and a sound
/// effect lines up with the frame it belongs to.
fn snap_marks(lanes: &[&[Clip]], skip: Option<(usize, usize)>, playhead: u32) -> Vec<u32> {
    let link = skip
        .and_then(|(lane, idx)| lanes.get(lane)?.get(idx))
        .and_then(|clip| clip.link);
    let mut marks: Vec<u32> = lanes
        .iter()
        .enumerate()
        .flat_map(|(lane, clips)| {
            clips
                .iter()
                .enumerate()
                .filter(move |&(idx, clip)| {
                    Some((lane, idx)) != skip && !(link.is_some() && clip.link == link)
                })
                .flat_map(|(_, clip)| [clip.start, clip.end()])
        })
        .collect();
    marks.push(playhead);
    marks.push(0);
    marks
}

/// [`snapped`], and the mark that pulled it there -- the line the bed draws
/// while the hand is still moving. `None` when nothing was near enough: a line
/// standing over open bed would promise a landing that is not going to happen.
/// The head is read before the tail, exactly as [`snapped`] prefers it.
///
/// `on` is the switch ([`ActionId::ToggleSnap`]): off, the gesture lands raw and
/// draws no line at all, which is the whole point of being able to turn it off.
fn snap_cue(on: bool, raw: u32, len: u32, tol: u32, marks: &[u32]) -> (u32, Option<u32>) {
    if !on {
        return (raw, None);
    }
    let start = snapped(raw, len, tol, marks);
    let mark = [start, start.saturating_add(len)]
        .into_iter()
        .find(|mark| marks.contains(mark));
    (start, mark)
}

/// Where a drag lands and the mark that pulled it there: the frame under the
/// pointer, less however far into the box the hand grabbed it (so a clip travels
/// with the pointer rather than jumping its head under it), snapped by
/// [`snap_cue`]. One answer, asked by the shadow drawn in flight
/// ([`Player::preview_ghost`]), by the line ([`Player::preview_drop`]) and by
/// the drop that commits ([`Player::move_clip`]) -- which is what makes the
/// promise and the landing the same frame.
fn landing(
    under: u32,
    grab: u32,
    len: u32,
    on: bool,
    tol: u32,
    marks: &[u32],
) -> (u32, Option<u32>) {
    snap_cue(on, under.saturating_sub(grab), len, tol, marks)
}

/// Why this file may not go on that lane, in the words the refusal is told in --
/// `None` when it may. A file with no picture belongs on an audio lane and
/// nowhere else, and a still is silent, so an audio lane is the one place it
/// cannot go. Asked twice: by the ghost tinting itself as refused on the way
/// down, and by the insert that commits ([`Player::insert_source`]), so what is
/// shown as impossible is exactly what is refused.
fn lane_refuses(path: &Path, lane: Lane) -> Option<String> {
    let name = file_name(path);
    let label = lane.label();
    match lane.kind {
        LaneKind::Video if engine::is_audio(path) => {
            Some(format!("NOT ON {label} — {name} has no picture; drop it on an audio lane"))
        }
        LaneKind::Audio if engine::is_image(path) => {
            Some(format!("NOT ON {label} — {name} is a still image; drop it on a video lane"))
        }
        _ => None,
    }
}

/// Where down an element a pointer sits, 0..1 from the top: the vertical twin
/// of [`frac_along`], for the equalizer, whose gain axis is the y one. An
/// element that was never painted reads as its middle -- flat, the one answer
/// that changes nothing -- rather than as a full boost.
fn frac_down(y: Pixels, bounds: Bounds<Pixels>) -> f32 {
    if bounds.size.height <= px(0.) {
        return 0.5;
    }
    ((y - bounds.top()) / bounds.size.height).clamp(0., 1.)
}

/// A slider value on the [`COLOR_STEP`] grid: what a drag rounds to, so the
/// pointer stops where the arrow keys do and "0.35" on screen is the number the
/// file writes rather than a rounding of one.
fn color_snap(value: f32) -> f32 {
    (value / COLOR_STEP).round() * COLOR_STEP
}

/// How the frame on screen is spread across the tone range: `HIST_BINS` counts
/// per channel, read off the BGRA the decoder handed over -- which is the
/// *graded* picture, because the grade is folded into the conversion
/// (`engine::convert::i420_to_bgra_with`). So what this counts is what the eye
/// is looking at, and moving a slider moves it.
///
/// Every [`HIST_SAMPLES`]th-of-a-frame pixel, not every pixel: a shape drawn
/// from eight thousand samples is the same shape, at a thousandth of the reads.
fn histogram(bgra: &[u8]) -> [[u32; HIST_BINS]; 3] {
    let pixels = bgra.len() / 4;
    let stride = (pixels / HIST_SAMPLES).max(1);
    let mut bins = [[0u32; HIST_BINS]; 3];
    for p in (0..pixels).step_by(stride) {
        let px = &bgra[p * 4..];
        // BGRA on the wire, `[r, g, b]` in the bins: the graph names channels
        // the way a person does.
        for (channel, value) in [px[2], px[1], px[0]].into_iter().enumerate() {
            bins[channel][usize::from(value) * HIST_BINS / 256] += 1;
        }
    }
    bins
}

/// The three counts drawn as three lines across the box, tallest bin to the top.
///
/// Square root, not linear: a shot with a big flat area (a night sky, a title
/// card) puts one bin so far above the rest that a linear graph is a single
/// spike beside a flat line, and the tilt a grade puts in the rest is exactly
/// what the card is for.
fn hist_curves(bins: [[u32; HIST_BINS]; 3]) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            // Shared across the channels, so their relative weight is readable;
            // never zero, so an unpumped (all-zero) histogram is a flat line
            // rather than a division by nothing.
            let top = bins.iter().flatten().copied().max().unwrap_or(0).max(1) as f32;
            for (channel, counts) in bins.iter().enumerate() {
                let mut path = PathBuilder::stroke(px(1.5));
                for (bin, &count) in counts.iter().enumerate() {
                    let at = point(
                        o.x + s.width * (bin as f32 / (HIST_BINS - 1) as f32),
                        o.y + s.height * (1. - (count as f32 / top).sqrt()),
                    );
                    match bin {
                        0 => path.move_to(at),
                        _ => path.line_to(at),
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgb(HIST_INK()[channel]));
                }
            }
        },
    )
    .absolute()
    .size_full()
}

/// In-place radix-2 FFT of `re`/`im`, whose length must be a power of two.
///
/// Hand-written, and deliberately: a 1024-point transform once a frame is a
/// few tens of microseconds of plain arithmetic, and a dependency for it would
/// be a build cost this editor pays on every compile for one card's backdrop.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    // Decimation in time: the input is first put in bit-reversed order, after
    // which the butterflies run over neighbours.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut span = 2;
    while span <= n {
        let step = std::f32::consts::TAU / span as f32;
        for start in (0..n).step_by(span) {
            for k in 0..span / 2 {
                // e^{-i2πk/span}: the negative sign is the forward transform.
                let (sin, cos) = (-step * k as f32).sin_cos();
                let (a, b) = (start + k, start + k + span / 2);
                let (tr, ti) = (cos * re[b] - sin * im[b], cos * im[b] + sin * re[b]);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        span <<= 1;
    }
}

/// The played signal as one height per curve point, 0 (silence) to 1 (the top
/// of the box) -- the analyser the response curve is drawn on top of.
///
/// The newest [`EQ_FFT`] samples of the engine's tap, Hann-windowed so a tone
/// that does not land on a bin centre is one hump rather than a smear across
/// the axis. Each column takes the *loudest* bin between it and its
/// neighbours: the axis is logarithmic, so one column near 20 Hz is a fraction
/// of a bin while one near 20 kHz is hundreds, and averaging those would sink
/// every peak up there into the noise beside it.
///
/// Empty -- nothing to draw -- for a tap too short to transform, which is what
/// a session has just after a seek.
///
/// corner-cut: one transform length for the whole axis, so the bass end is a bin
/// (47 Hz at 48 kHz) wide however many columns are drawn across it -- a 60 Hz
/// hum and an 80 Hz one are the same hump down there. Upgrade path is the
/// analyser every mastering EQ uses: two or three transforms of different
/// lengths, each drawn over the octaves it resolves.
fn eq_spectrum(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    if samples.len() < EQ_FFT {
        return Vec::new();
    }
    let tail = &samples[samples.len() - EQ_FFT..];
    let mut re: Vec<f32> = tail
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let w = 0.5 * (1. - (std::f32::consts::TAU * i as f32 / EQ_FFT as f32).cos());
            s * w
        })
        .collect();
    let mut im = vec![0.; EQ_FFT];
    fft(&mut re, &mut im);
    // Magnitude per bin, scaled so a full-scale sine reads 0 dBFS: half the
    // energy is in the mirrored half of the transform, and the Hann window
    // takes another factor of two off.
    let mags: Vec<f32> = (0..EQ_FFT / 2)
        .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() * 4. / EQ_FFT as f32)
        .collect();
    let bin_hz = sample_rate as f32 / EQ_FFT as f32;
    let (floor, ceiling) = EQ_SPECTRUM_DB;
    let at = |along: f32| EQ_FREQ_LOW * (EQ_FREQ_HIGH / EQ_FREQ_LOW).powf(along);
    (0..=EQ_CURVE_STEPS)
        .map(|step| {
            let along = step as f32 / EQ_CURVE_STEPS as f32;
            let half = 0.5 / EQ_CURVE_STEPS as f32;
            // Bin 0 is DC and means nothing here, so the low end starts at 1.
            let low = (at(along - half) / bin_hz).round().max(1.) as usize;
            let high = (at(along + half) / bin_hz).round().max(1.) as usize;
            let peak = (low..=high)
                .filter_map(|k| mags.get(k))
                .fold(0f32, |a, &b| a.max(b));
            let db = 20. * peak.max(1e-9).log10();
            ((db - floor) / (ceiling - floor)).clamp(0., 1.)
        })
        .collect()
}

/// The analyser drawn as one filled shape from the floor of the box.
fn eq_spectrum_curve(levels: Vec<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let last = levels.len().saturating_sub(1).max(1) as f32;
            let mut path = PathBuilder::fill();
            path.move_to(point(o.x, o.y + s.height));
            for (i, level) in levels.iter().enumerate() {
                path.line_to(point(
                    o.x + s.width * (i as f32 / last),
                    o.y + s.height * (1. - level),
                ));
            }
            path.line_to(point(o.x + s.width, o.y + s.height));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, rgba(EQ_SPECTRUM_INK()));
            }
        },
    )
    .absolute()
    .size_full()
}

/// The cascade's frequency response, drawn as one line across the graph, with
/// each band's own response threaded dimly under it.
///
/// Every point comes from `EqParams::response_db`, which reads the very
/// coefficients the samples are filtered through: the curve cannot drift from
/// what is heard, because there is no second copy of the maths. A single band's
/// thread is that same call on a cascade of one, so the two cannot disagree
/// either -- and it is what makes a boost sitting inside a cut visible at all,
/// where the sum alone would draw a flat line and say nothing.
///
/// corner-cut: bands that overlap can sum past the ±`EQ_GAIN_LIMIT` axis and the
/// curve then rides the edge of the box; upgrade = a wider dB axis with the
/// handles and [`Player::drag_band`] rescaled to it.
fn eq_curve(params: EqParams, sample_rate: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let line = |of: &EqParams| -> Vec<_> {
                (0..=EQ_CURVE_STEPS)
                    .map(|step| {
                        let along = step as f32 / EQ_CURVE_STEPS as f32;
                        point(
                            o.x + s.width * along,
                            o.y + s.height
                                * eq_y(of.response_db(eq_freq(along), sample_rate)),
                        )
                    })
                    .collect()
            };
            // One thread per band, first, so the sum is drawn over them.
            for band in &params.bands {
                let mut bell = PathBuilder::stroke(px(1.));
                for (step, at) in line(&EqParams {
                    bands: vec![*band],
                })
                .into_iter()
                .enumerate()
                {
                    match step {
                        0 => bell.move_to(at),
                        _ => bell.line_to(at),
                    }
                }
                if let Ok(bell) = bell.build() {
                    window.paint_path(bell, rgba(EQ_BELL_INK()));
                }
            }
            let points = line(&params);
            // The area between the curve and 0 dB, closed along that line: a
            // boost and a cut wind opposite ways around it, which is exactly
            // what makes both of them fill and the flat parts stay empty.
            let mut area = PathBuilder::fill();
            area.move_to(point(o.x, o.y + s.height / 2.));
            for at in &points {
                area.line_to(*at);
            }
            area.line_to(point(o.x + s.width, o.y + s.height / 2.));
            area.close();
            if let Ok(area) = area.build() {
                window.paint_path(area, rgba(EQ_FILL_INK()));
            }
            let mut path = PathBuilder::stroke(px(2.));
            for (step, at) in points.into_iter().enumerate() {
                match step {
                    0 => path.move_to(at),
                    _ => path.line_to(at),
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(ACCENT_PRIMARY()));
            }
        },
    )
    .absolute()
    .size_full()
}



/// NLE timecode: `HH:MM:SS:FF`, the frame counted inside its own second.
fn timecode(t: f64, fps: f64) -> String {
    let t = t.max(0.);
    let secs = t as u64;
    let last = (fps.ceil() as u64).saturating_sub(1);
    let frame = ((t - secs as f64) * fps) as u64;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        frame.min(last)
    )
}

/// Wall clock for a progress line: `M:SS`, minutes past the hour included
/// rather than an hours field nobody reads on an export.
fn clock(secs: f32) -> String {
    let secs = secs.max(0.) as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// How long a stage may sit without moving before the import line stops
/// reading like a progress line and says outright that the wait is the file's,
/// not the window's. Five seconds: a 25 GB remux spends eleven in its header
/// alone, and a person watching a still line for that long has already decided
/// the editor hung.
const IMPORT_STALL: f32 = 5.;

/// Which of an import's two reads the worker is inside. Travels as one atomic,
/// the way an export's progress does ([`engine::ExportHandle`]) -- there is no
/// fraction to send, because neither read reports where in the file it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ImportStage {
    /// The container header: the sample tables, the cue index, the frame count.
    /// Eleven seconds on a cold 29 GB remux, a hundred and fifty milliseconds
    /// once the pages are warm -- the whole reason this runs off the UI thread.
    Header,
    /// The subtitle tracks inside a Matroska, which is a walk over the file's
    /// blocks rather than a header read (~200 ms on a two-hour film).
    Subtitles,
}

impl ImportStage {
    /// The atomic the worker writes; `u8` because that is what an
    /// [`AtomicU8`](std::sync::atomic::AtomicU8) carries.
    fn from_u8(n: u8) -> Self {
        match n {
            0 => Self::Header,
            _ => Self::Subtitles,
        }
    }

    /// What the line calls this stage.
    fn what(self) -> &'static str {
        match self {
            Self::Header => "reading the header",
            Self::Subtitles => "reading the subtitle tracks",
        }
    }
}

/// argv sorted into the file that becomes the timeline and the queue every
/// named file is read through, in the order they were named. The whole of what
/// a launch does before the window is on screen -- and it touches no disk,
/// which is the point: the header walk happens on a worker with the window
/// already up ([`Player::take_import`]).
fn launch_queue(
    args: impl Iterator<Item = PathBuf>,
) -> (Option<PathBuf>, std::collections::VecDeque<PathBuf>) {
    let queue: std::collections::VecDeque<PathBuf> = args.collect();
    (queue.front().cloned(), queue)
}

/// What a queued file turns into. Every file goes through one queue -- argv's
/// first file, argv's extras, a drop, the Import button -- and this is the fork
/// its worker is started on.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
enum Landing {
    /// The media argv named: it becomes the timeline, and the canvas, the fps,
    /// the title and the export path come from it.
    Open,
    /// A `.edith` argv named: a whole timeline restored, not a source.
    Project,
    /// Everything else: a row in the library, the timeline untouched.
    Import,
}

/// Which of the three a queued file is (`landing` above is the drag's).
/// `opening` is the one path argv named and is cleared as it lands, so a second
/// arrival of the same *media* path -- a drop of the film that is already open
/// -- is an import, which is what a drop has always been.
///
/// A `.edith` is never an import, whichever door it came through: it is a whole
/// timeline and there is nothing to add it to. Argv's, a dropped one and the
/// Import button's are one landing, so the seconds its open costs are the
/// worker's for all three ([`open_ahead`]) and the line above the panel names it
/// while it runs.
fn arrival(opening: Option<&std::path::Path>, path: &std::path::Path) -> Landing {
    match (is_project(path), opening == Some(path)) {
        (true, _) => Landing::Project,
        (false, true) => Landing::Open,
        (false, false) => Landing::Import,
    }
}

/// The whole of what an import shows while it runs: which file, which stage,
/// the clock that proves the window is still answering, and what is behind it
/// in the queue.
///
/// `since` is how long the *stage* has stood still, which is the only movement
/// an unmeasurable read has: past [`IMPORT_STALL`] the line says so in words,
/// because a bar that cannot move and a bar that has stopped look identical.
///
/// `opening` is the file argv named, which is read through the same queue and
/// says so in the same words -- except that it is being *opened*, and a line
/// that called it an import would be describing the wrong thing to the one
/// person who typed the name.
fn import_line(
    name: &str,
    stage: ImportStage,
    elapsed: f32,
    since: f32,
    waiting: usize,
    opening: bool,
) -> String {
    let tail = match waiting {
        0 => String::new(),
        n => format!(" · {n} more waiting"),
    };
    let what = stage.what();
    let verb = match opening {
        true => "OPENING",
        false => "IMPORTING",
    };
    match since >= IMPORT_STALL {
        true => format!(
            "{verb} {name} · still {what} — a big file is minutes of reading, and the window is \
             not frozen · {} elapsed{tail}",
            clock(elapsed)
        ),
        false => format!("{verb} {name} · {what} · {} elapsed{tail}", clock(elapsed)),
    }
}

/// What opening the silence card on a source costs.
#[derive(PartialEq, Eq, Debug)]
enum ScanPlan {
    /// Its levels are already read: the marks are arithmetic on this frame.
    Marks,
    /// Nothing read: a worker, and a card that says so meanwhile.
    Start,
    /// A worker is already reading this very source -- the other half of a take
    /// names the same file, and a second card on it waits for the first read
    /// rather than throwing away the minute already spent.
    Wait,
}

/// Which of the three [`ScanPlan`]s opening the card on `key` means. The whole
/// of the cache policy, and the reason a second film does not cost the first
/// one its levels: what is cached is asked per source, never "the last one".
fn scan_plan(cached: bool, running: Option<&ScanKey>, key: &ScanKey) -> ScanPlan {
    match (cached, running) {
        (true, _) => ScanPlan::Marks,
        (false, Some(at)) if at == key => ScanPlan::Wait,
        _ => ScanPlan::Start,
    }
}

/// The half-open source seconds a [`ScanKey`]'s frames name, at the project's
/// rate -- what [`engine::silence::levels`] is asked to read, and the same
/// arithmetic playback puts a clip's `in_frame` through. A rate that is not a
/// rate reads the whole file rather than an empty window: a scan of nothing is
/// worse than a scan of too much.
fn source_secs(key: &ScanKey, fps: f64) -> (f64, f64) {
    match fps.is_finite() && fps > 0. {
        true => (f64::from(key.2) / fps, f64::from(key.3) / fps),
        false => (0., f64::INFINITY),
    }
}

/// What the silence card says while its worker reads. Unlike an import this
/// one *has* a fraction -- a decode knows how far into the sound it has come --
/// so the line is where it is up to, out of what the header claims the track is
/// (`total` of 0 for a header that does not say, drawn as nothing rather than
/// as a guess). Both in seconds.
///
/// The stall clock is [`IMPORT_STALL`]'s, for its reason: past five seconds
/// without the mark moving, a line that cannot move and a line that has stopped
/// look identical, and only one of them is worth words.
fn silence_line(scanned: f32, total: f32, elapsed: f32, since: f32) -> String {
    let far = match total > 0. {
        true => format!("{} of ~{} scanned", clock(scanned), clock(total)),
        false => format!("{} scanned", clock(scanned)),
    };
    match since >= IMPORT_STALL {
        true => format!(
            "SCANNING · still reading the sound — a big film is minutes of decoding, and the \
             window is not frozen · {far} · {} elapsed",
            clock(elapsed)
        ),
        false => format!("SCANNING · {far} · {} elapsed", clock(elapsed)),
    }
}

/// How often a progress mark is worth keeping, how far back the rate is
/// measured, and the least span that may answer at all. An export crosses
/// hardware and software segments that run at different speeds, so the
/// estimate is a window's average and never the instant's.
const ETA_SAMPLE: f32 = 0.5;
const ETA_WINDOW: f32 = 8.;
const ETA_SPAN: f32 = 1.5;

/// Records where the export has got to and forgets what has fallen out of the
/// window. One mark per `ETA_SAMPLE`, a window's worth kept: a bounded list
/// whichever way the encode goes.
fn note_progress(marks: &mut Vec<(f32, f32)>, elapsed: f32, progress: f32) {
    if marks.last().is_none_or(|&(t, _)| elapsed - t >= ETA_SAMPLE) {
        marks.push((elapsed, progress));
    }
    while marks.len() > 2 && marks[0].0 < elapsed - ETA_WINDOW {
        marks.remove(0);
    }
}

/// Seconds left at the window's rate, or `None` while nothing measurable has
/// happened yet -- which the line says as "estimating…" rather than as a
/// number it would have to take back.
fn eta_secs(marks: &[(f32, f32)], elapsed: f32, progress: f32) -> Option<f32> {
    // A finished pass is not a guess: it is over.
    if progress >= 1. {
        return Some(0.);
    }
    if progress <= 0. || elapsed < ETA_SPAN {
        return None;
    }
    // Two rates, averaged. The window's follows the encode across a
    // hardware-to-software handover; the whole run's is what keeps a window
    // that is all stall from throwing the number minutes out and back. Neither
    // alone reads well: raw window rate spikes eightfold on either edge of a
    // stall, and the run average alone never notices a segment change.
    let overall = progress / elapsed;
    let recent = marks
        .first()
        .filter(|&&(t, _)| elapsed - t >= ETA_SPAN)
        .map_or(overall, |&(t, p)| (progress - p) / (elapsed - t));
    Some(2. * (1. - progress) / (recent + overall))
}

#[cfg(test)]
mod tests;

fn main() {
    // A keymap file that cannot be read leaves the defaults in force, and takes
    // the notice slot ahead of an open or import refusal: it is about every key
    // the window has, and those refusals are on stderr either way.
    let (keymap, notice) = Keymap::load();
    if let Some(text) = &notice {
        eprintln!("{text}");
    }
    // The palette the last session picked, before the first paint: a window that
    // opened cool and turned warm a frame later would be the theme announcing
    // itself. Silent on a missing or unreadable file -- the default is a whole
    // answer, and nothing of the user's is lost by it.
    ui::theme::load();
    // Nothing named on the command line is read here. The first file makes the
    // timeline -- a `.edith` restores a whole one, anything else *is* one --
    // and the rest are imports like any other: rows in the library, dragged
    // onto a lane when they are wanted there. All of them go through the queue
    // a drop uses ([`Player::import`]), which is the door with a progress line
    // on it, and their refusals arrive in the notice bar as a drop's do.
    //
    // Queued rather than opened because a 25 GB film cold is twelve seconds of
    // header walk, and it used to be twelve seconds of *no window at all* -- a
    // window that has not opened cannot say what it is waiting for. Now the
    // window is up in the time it takes to make one, naming the file and the
    // read that is running, and the timeline appears when that read lands
    // ([`Player::take_import`]).
    //
    // No argument at all opens the window empty, exactly as before: the library
    // then arrives by drop or by the Import button.
    let (arg, queue) = launch_queue(std::env::args().skip(1).map(PathBuf::from));
    let name: SharedString = arg
        .as_deref()
        .map_or_else(|| NO_FILE.into(), |arg| file_name(arg).into());

    Application::new().run(move |cx: &mut App| {
        // 720p: the picture's own size is not known yet -- knowing it is the
        // twelve seconds this window exists to be up during -- and a window
        // that resized itself under a hand already dragging it would be worse
        // than one that opened at the size the empty window has always used.
        let bounds = Bounds::centered(None, size(px(1280.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("edith".into()),
                    ..Default::default()
                }),
                // What a desktop groups this window under. The title beside it
                // is pushed from the render, because wayland takes neither of
                // them from the titlebar options above (gpui's wayland window
                // reads only `app_id`, window.rs:1202 / window.rs:939) and the
                // title has to follow the file being opened anyway.
                app_id: Some("edith".to_string()),
                ..Default::default()
            },
            |window, cx| {
                let queue = queue.clone();
                let player = cx.new(|cx| Player {
                    // Nothing to wait for yet: the file named on the command
                    // line is still queued, and the repaint that carries its
                    // poster frame to the screen is asked for when it lands
                    // (`open_media` -> `reset_after_reseek`).
                    seek_since: None,
                    resynced: None,
                    session: None,
                    // Full and unmuted, which is what the session it was just
                    // handed is already set to: nothing to push at startup.
                    volume: Volume::default(),
                    volume_bar: Rc::default(),
                    volume_dragging: false,
                    // Only ever used with a timeline; 30 keeps the empty
                    // timecode reading in frames rather than in NaN.
                    fps: 30.,
                    // The file being opened, from the first frame the window
                    // draws: the title bar and the header name it while its
                    // header is still being read.
                    name: name.clone(),
                    image: None,
                    sub_image: None,
                    held: None,
                    ruler: Rc::default(),
                    // A second is [`PPS_DEFAULT`] pixels wide until someone
                    // zooms or asks for the fit: a project opens at a scale, not
                    // at whatever its first import happens to be long.
                    scale: Scale::default(),
                    // Nobody has scrolled anything yet, so the follow has the
                    // view: the first frame is drawn where the head is.
                    panned: false,
                    selected: None,
                    context_menu: None,
                    picker: None,
                    library_menu: None,
                    selected_asset: None,
                    library_tab: LibraryTab::Media,
                    waves: HashMap::new(),
                    streams: HashMap::new(),
                    bitrates: HashMap::new(),
                    sizes: HashMap::new(),
                    syncs: HashMap::new(),
                    decoders: HashMap::new(),
                    export_seat: None,
                    hw_caps: None,
                    clipboard: None,
                    scrubbing: false,
                    trim: None,
                    grab: 0,
                    snap: true,
                    subs_on: true,
                    sub_track: 0,
                    snap_cue: None,
                    ghost: None,
                    lane_drop: None,
                    last_scrub: Instant::now(),
                    last_target: 0,
                    export: None,
                    cancelling: false,
                    export_started: None,
                    export_marks: Vec::new(),
                    // Both derived from the file when it lands, by the same
                    // `open_media`/`load_project` a drop goes through: an
                    // export beside the picture, a save beside it too.
                    export_path: PathBuf::new(),
                    project_path: PathBuf::new(),
                    keymap: keymap.clone(),
                    keys_open: false,
                    keys_search: String::new(),
                    keys_scroll: ScrollHandle::new(),
                    lanes_scroll: ScrollHandle::new(),
                    inspector_scroll: ScrollHandle::new(),
                    eq_scroll: ScrollHandle::new(),
                    export_open: false,
                    export_grouped: true,
                    export_refusals_inline: false,
                    eq_open: None,
                    // Replaced by the clip's own curve the moment the card
                    // opens; nothing reads it before that.
                    eq_params: EqParams::default(),
                    eq_band: 0,
                    eq_dragging: false,
                    eq_graph: Rc::default(),
                    eq_spectrum: true,
                    speed_open: None,
                    speed_bar: Rc::default(),
                    speed_dragging: false,
                    pending_speed: None,
                    mix_open: false,
                    mix_field: 0,
                    silence_open: None,
                    // The conservative defaults the engine documents: a first
                    // scan that leaves a little too much is one nobody undoes.
                    silence: engine::silence::Settings::default(),
                    silence_factor: Speed::MAX,
                    // The take, not the timeline: the narrower answer is the
                    // one a person can widen on purpose.
                    silence_scope: Scope::Take,
                    silence_field: 0,
                    // The reference named, which is what the card said before
                    // there was a choice about it.
                    silence_dbfs: true,
                    silence_marks: Vec::new(),
                    silence_levels: HashMap::new(),
                    silence_scan: None,
                    color_open: None,
                    color_band: 0,
                    color_dragging: false,
                    color_bars: std::array::from_fn(|_| Rc::default()),
                    pending_color: None,
                    // Empty until the first frame is pumped, which draws as a
                    // flat line rather than as a shape nothing measured.
                    histogram: [[0; HIST_BINS]; 3],
                    // What an export is until someone says otherwise: the
                    // bitrate the picture asks for.
                    quality: Quality::Auto,
                    custom_mbps: 0,
                    mbps_edit: None,
                    // ...and the rate the sound has always been written at.
                    audio_kbps: DEFAULT_AUDIO_KBPS,
                    // Picture and sound, which is what an export was before
                    // there was anything to pick.
                    format: Format::default(),
                    rebinding: None,
                    notices: notice.clone().map(SharedString::from).into_iter().collect(),
                    exported: None,
                    // The whole of argv, waiting for the first repaint to start
                    // it: the window is up before a byte of it is read.
                    importing: None,
                    imports: queue,
                    opening: arg.clone(),
                    // Nothing pushed yet, and never a real title: the first
                    // render is what names the window.
                    titled: String::new(),
                    displayed: 0,
                    dropped: 0,
                    started: None,
                    focus: cx.focus_handle(),
                });
                // Nothing else takes focus, and without it the key listener
                // above is never reached.
                window.focus(&player.read(cx).focus);
                player
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
