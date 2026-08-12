mod keymap;

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

/// Editor chrome: three grays and one accent, all darker than the picture so the
/// frame is what the eye lands on. `LETTERBOX` stays the video's own bed.
const LETTERBOX: u32 = 0x101010;
const CHROME: u32 = 0x242424;
const SURFACE: u32 = 0x333333;
const INK: u32 = 0xc8c8c8;
const ACCENT: u32 = 0x4a9eff;
/// The accent at surface brightness: a selected clip is tinted, not lit up.
const SELECTED: u32 = 0x2a4a6b;
/// One per source, so a clip that came from an imported file reads as coming
/// from somewhere else. Same brightness as `SURFACE` and only the hue moves:
/// the lane stays in the dark family the rest of the chrome lives in
/// (ledger:187). The first entry is a colour like the rest rather than `SURFACE`
/// itself -- a first import whose swatch is the panel's own background is a
/// swatch nobody can see, and a file with one colour is what the whole
/// association is built on.
const SOURCE_TINTS: [u32; 4] = [0x29333b, 0x3b3329, 0x293b33, 0x33293b];
/// The mirror of `SELECTED`: a drop the lane will not take, tinting the shadow
/// a drag draws ([`Ghost`]) so a refusal is seen before the release rather than
/// read afterwards. Same brightness as the rest of the chrome -- a warning, not
/// an alarm.
const REFUSE: u32 = 0x6b2a2a;
/// How solid that shadow is (`0xRRGGBBAA`): enough to read as a box, little
/// enough that the clip it is drawn over is still legible through it.
const GHOST_ALPHA: u32 = 0x66;
/// ...and the narrowest it is drawn: a library row whose length the engine has
/// not measured yet has a landing place but no width, and a head marker says
/// where it goes where a zero-width box would say nothing.
const GHOST_MIN: f32 = 2.;
/// One step lighter than whatever it sits on: the pointer's answer that this is
/// clickable. Two of them, because buttons stand on `SURFACE` and the scrub
/// strip stands on `CHROME`.
const HOVER: u32 = 0x3f3f3f;
const HOVER_DIM: u32 = 0x2c2c2c;
/// Secondary text -- shortcuts, dismissal hints. Dimmer than `INK` and still
/// past 4.5:1 on both `SURFACE` and `CHROME`.
const INK_DIM: u32 = 0xa0a0a0;

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
/// ponytail: "finer than the pixels" stops being true once the timeline is
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

/// The subtitle over the picture: white on a black plate, because a cue is read
/// against whatever the film happens to be showing under it and the chrome's own
/// greys are not a contrast the picture agrees to. 21:1 against the plate, which
/// is the only pair here that is not a chrome-on-chrome one.
const SUB_INK: u32 = 0xffffff;
const SUB_SHADE: u32 = 0x000000cc;
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

/// The silence scan a worker is running, as the card shows it. Same two clocks
/// as an [`Import`] and for the same reason -- one proves the window answers,
/// one says the read has stopped moving -- over a progress that *can* move:
/// a decode knows how far into the sound it has come, so the card says so.
struct SilenceScan {
    /// Source and stream being scanned, which is the cache key the levels land
    /// under and what tells a second open of the same clip from a new one.
    key: (PathBuf, usize),
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

/// The grid's ink: above the card's background and below everything drawn on
/// it, so the lines are a ruling rather than a thing to look at.
const EQ_GRID: u32 = 0x3a3a3a;

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

/// The analyser's fill, behind the curve: a dim blue-grey with enough alpha to
/// read as a haze the accent line sits on top of.
const EQ_SPECTRUM_INK: u32 = 0x7f95ad66;

/// The area under the response curve, same accent as the line at a tenth of
/// its weight: it is what makes a boost read as a hill rather than as a wire.
const EQ_FILL_INK: u32 = 0x4a9eff26;

/// One band's own response, drawn under the sum: a dim thread per band, so a
/// boost pushed against a cut can be *seen* as two bands rather than as the
/// flat line their total makes.
const EQ_BELL_INK: u32 = 0x4a9eff66;

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

/// What each channel's line is drawn in, in `[r, g, b]` order -- the channel it
/// counts, lightened enough to read on the dark box.
const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];

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
    /// Which decoder each source will run on, probed once at import and kept:
    /// the codec (`None` for a still) and the seat the engine picked for it.
    /// What a library row says *before* anything plays; the running answer is
    /// the session's own (`PlaybackSession::decode_backend`), which follows a
    /// fallback this cannot. Filled like `sizes`: presence means "asked", and
    /// `None` is a source with no decoder to name -- a song, or one the probe
    /// refused -- which must stay in the map or every repaint would ask again.
    decoders: HashMap<PathBuf, Option<(Option<Codec>, Backend)>>,
    /// Which encoder an export of the picked settings would open, and what it
    /// was asked about: the probe opens a real VA-API encoder (~100 ms), so it
    /// runs off the render thread and only while the export card is up. The
    /// inner `None` is "asked, not answered yet".
    export_seat: Option<(ExportSettings, (u32, u32), Option<&'static str>)>,
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
    /// The levels of every source scanned this session, kept so moving a
    /// threshold is arithmetic rather than another decode. Keyed by source and
    /// stream, and not one entry: two films on one timeline would otherwise
    /// evict each other, and the decode being paid twice is the fifty seconds
    /// this card exists to not spend.
    silence_levels: HashMap<(PathBuf, usize), Arc<Vec<f32>>>,
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
    /// What the last file action had to say. Holds its own bar above the panel
    /// until it is answered -- any key retires it, so does a click on it -- so a
    /// failure is read in full instead of blinking past.
    notice: Option<SharedString>,
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
    /// Wall clock of the first displayed frame -- the real-speed measurement.
    started: Option<Instant>,
    focus: FocusHandle,
}

impl Player {
    /// Catches the display up to the clock: everything already due is taken off
    /// the channel and only the last of them is shown, which *is* the
    /// drop-when-behind policy. A frame that is not due yet waits in `held`.
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
        loop {
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
            // Nothing to cancel while nothing is exporting; the export guard in
            // the key handler is what answers this one while there is.
            ActionId::CancelExport => {}
            ActionId::ShowActions => self.show_actions(cx),
        }
    }

    /// The magnet off and on again, in words: a snap that stops working
    /// silently reads as a bug, and one that starts working silently reads as
    /// one too. The line goes with it -- nothing is being promised any more.
    fn toggle_snap(&mut self, cx: &mut Context<Self>) {
        self.snap = !self.snap;
        self.snap_cue = None;
        self.ghost = None;
        self.notice = Some(match self.snap {
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
        self.notice = Some(
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
            self.notice = Some("NOTHING UNDER THE PLAYHEAD — move it onto a clip first".into());
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
            self.notice = Some("no timeline to fit — open a file first".into());
            cx.notify();
            return;
        };
        let target = self
            .selected
            .filter(|(lane, _)| lane.kind == LaneKind::Video)
            .or_else(|| session.video_clip_at(session.now()));
        let Some((lane, idx)) = target else {
            self.notice = Some("no clip under the playhead to fit".into());
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
            self.notice = Some(format!("FIT POLICY: {} on {w}x{h}", fit_label(fit)).into());
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
        cx.notify();
    }

    /// All the way back out: the whole timeline across the bed, and the one
    /// thing that reads the timeline's own length to decide how wide a second
    /// is drawn.
    fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        self.scale = self.view().fit();
        cx.notify();
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
            self.notice = Some("no timeline to resize — open a file first".into());
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
            self.notice = Some(format!("PROJECT: {width}x{height}").into());
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
            self.notice = Some(format!("PROJECT: {} fps", fps_label(fps)).into());
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
            self.notice = Some(format!("HDR: {} — affects HDR media", tone_label(preset)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Opens a choice list on a setting, where it was asked for. One floating
    /// thing at a time: the click that opens it is the click that closes
    /// whatever menu it was opened from.
    fn open_picker(&mut self, of: Pick, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.context_menu = None;
        self.library_menu = None;
        self.picker = Some(Picker { of, at });
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
            self.notice = Some("no timeline to grade — open a file first".into());
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
            None => self.notice = Some("no clip under the playhead to grade".into()),
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
            self.notice = Some("no timeline to re-time — open a file first".into());
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
            None => self.notice = Some("no clip under the playhead to re-time".into()),
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
                Err(e) => self.notice = Some(e.to_string().into()),
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
    /// Either half of a take will do: the scan reads the *source*, and both
    /// halves of an A/V take name the same file.
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
            self.notice = Some("no timeline to scan — open a file first".into());
            cx.notify();
            return;
        };
        match self
            .selected
            .or_else(|| session.video_clip_at(session.now()))
            .map(|clip| audio_half(session, clip))
        {
            Some((lane, idx)) => {
                let source = self.session.as_ref().and_then(|session| {
                    let clip = session.lane_clips(lane).get(idx)?;
                    session.sources().get(clip.source).cloned()
                });
                // A still is asked *before* the decoder is: handing a png to the
                // mp4 demuxer answers "a box with a larger size than it", which
                // is a true sentence about a container and nothing a person can
                // act on. A picture has no sound for the same reason a silent
                // video has none, so it is refused in the same words.
                let Some(source) = source else {
                    cx.notify();
                    return;
                };
                if engine::is_image(&source.path) {
                    self.notice = Some(unscannable(lane, idx, &source.path).into());
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
                let key = (source.path.clone(), source.audio_stream);
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
            None => self.notice = Some("no clip under the playhead to scan".into()),
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
    fn start_silence_scan(&mut self, key: (PathBuf, usize), cx: &mut Context<Self>) {
        self.cancel_silence_scan();
        self.silence_marks.clear();
        let progress = Arc::new(engine::silence::Progress::default());
        let scan = cx.background_executor().spawn({
            let (key, progress) = (key.clone(), Arc::clone(&progress));
            async move { engine::silence::levels_with_progress(&key.0, key.1, &progress) }
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
                            this.notice = Some(unscannable(lane, idx, &key.0).into());
                        }
                        this.close_silence();
                    }
                    Err(e) => {
                        this.close_silence();
                        this.notice = Some(format!("SCAN FAILED: {e}").into());
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
            .get(&(source.path.clone(), source.audio_stream))
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
            self.notice = Some(
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
                self.notice = Some(
                    format!(
                        "{count} SILENCES CUT {reach} — {} shorter, {} takes it back",
                        secs_label(saved),
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notice = Some(e.to_string().into()),
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
                self.notice = Some(
                    format!(
                        "{count} SILENCES AT {rate} {reach} — {} takes it back",
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notice = Some(e.to_string().into()),
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
            self.notice = Some(refusal.into());
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
            self.notice = Some(
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
            self.notice = Some("LAST BAND — flatten it instead (r), or close the card".into());
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
        if let Some(session) = &mut self.session {
            session.cut_at(session.now());
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
                self.notice = Some(
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
                    self.notice =
                        Some("NOTHING DETACHED — that clip is not grouped with another".into());
                }
            }
            (Some(_), None) => {
                self.notice = Some("NOTHING DETACHED — click the take to take apart first".into())
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
                    self.notice = Some(format!("NOT GROUPED — {e}").into());
                }
            }
            (Some(_), Some(_), None) => {
                self.notice = Some(
                    "NOTHING TO GROUP WITH — no clip on another track covers exactly these frames"
                        .into(),
                )
            }
            (Some(_), None, _) => {
                self.notice = Some("NOTHING GROUPED — click one of the halves first".into())
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
            self.notice = Some("NOTHING DELETED — that clip is no longer there".into());
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
                    self.notice = Some("NOTHING LIFTED — that half is no longer there".into());
                }
            }
            (Some(_), None) => {
                self.notice = Some("NOTHING LIFTED — click the half to remove first".into())
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

    /// Probes the encoder an export would open, once per (settings,
    /// resolution) and only while the export card is up -- it opens the very
    /// VA-API encoder the export would, which is what makes the card's line a
    /// measurement instead of a promise, and also what makes it too slow for
    /// the render thread. Written before the spawn, like the probes above, so
    /// the repaints during it start no second one.
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
        let meta = *session.meta();
        // Cloned rather than copied: the settings carry the picked subtitle
        // rows, which is a `Vec` ([`engine::export::ExportSettings`]).
        let key = (settings.clone(), (meta.width, meta.height));
        if self
            .export_seat
            .as_ref()
            .is_some_and(|(asked, size, _)| (asked, size) == (&key.0, &key.1))
        {
            return;
        }
        self.export_seat = Some((key.0.clone(), key.1, None));
        let probed = cx
            .background_executor()
            .spawn(async move { engine::export::planned_video(&meta, &settings) });
        cx.spawn(async move |this, cx| {
            let probed = probed.await;
            this.update(cx, |this, cx| {
                // Only if the card is still asking the same question: a format
                // changed while the plugin opened has a probe of its own.
                if let Some(seat) = this
                    .export_seat
                    .as_mut()
                    .filter(|(asked, size, _)| (asked, size) == (&key.0, &key.1))
                {
                    seat.2 = probed;
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
                self.notice = Some(
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
                self.notice = Some(
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
    /// ponytail: the bed now runs past the last frame whenever the timeline is
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
            tint: file_tint(self.sources(), path).unwrap_or(SURFACE),
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
            self.notice = Some(why.into());
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
            Err(e) => self.notice = Some(format!("NOTHING ADDED — {e}").into()),
            Ok(false) => {
                self.notice = Some("NOTHING ADDED — that file could not be placed here".into())
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
        self.notice = Some(text.into());
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
        // ponytail: its atlas tile is not released -- `window.drop_image` wants
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
        // The fork is made here, once, and carried to the landing: an import is
        // *read* on the worker and opened again warm, while the file argv named
        // is *opened* on the worker and handed over whole. Both leave the UI
        // thread free for the twelve seconds a cold 25 GB header walk takes;
        // opening the timeline outright is what keeps it from paying the walk
        // twice on a warm one.
        let what = arrival(self.opening.as_deref(), &path);
        let read = cx.background_executor().spawn({
            let (path, stage) = (path.clone(), Arc::clone(&stage));
            async move { open_ahead(what, &path, &stage) }
        });
        let now = Instant::now();
        self.importing = Some(Import {
            path: path.clone(),
            started: now,
            stage,
            seen: ImportStage::Header,
            since: now,
        });
        cx.spawn(async move |this, cx| {
            let landed = read.await;
            this.update(cx, |this, cx| {
                this.importing = None;
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
        let subs = match landed {
            Landed::Read(subs) => subs,
            what => {
                self.opening = None;
                match what {
                    Landed::Project(opened) => self.install_project(path, opened, cx),
                    Landed::Media(opened) => {
                        let text = self.install_media(path, opened, true);
                        eprintln!("{text}");
                        self.notice = Some(text.into());
                        cx.notify();
                    }
                    Landed::Read(_) => unreachable!("matched above"),
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
        let text = match self.session.as_mut().map(|session| session.import(path)) {
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
            Some(Err(e)) => format!("IMPORT FAILED: {e}"),
            None => self.open_media(path, false, subs),
        };
        eprintln!("{text}");
        self.notice = Some(text.into());
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
                // The same fork the drop handler makes: a project replaces the
                // timeline, media joins the library.
                Ok(Some(path)) if is_project(&path) => this.load_project(&path, cx),
                Ok(Some(path)) => this.import(&path, cx),
                // Cancelled: the user already knows what happened.
                Ok(None) => {}
                Err(text) => {
                    eprintln!("{text}");
                    this.notice = Some(text.into());
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
            self.notice = Some(text.into());
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
                    this.notice = Some(text.into());
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
        self.notice = Some(format!("READING {} for subtitles…", file_name(path)).into());
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
                    .map(|session| parsed.map(|tracks| session.add_subtitle_tracks(tracks)));
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
            .map(|session| subs.map(|tracks| session.add_subtitle_tracks(tracks)));
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
            Some(Ok(0)) => format!("{} is already on the timeline", file_name(path)),
            Some(Ok(n)) => format!(
                "SUBTITLES {} — {n} track(s), showing over the picture, {} hides them",
                file_name(path),
                self.keymap.display(ActionId::ToggleSubtitles)
            ),
            Some(Err(e)) => format!("SUBTITLE IMPORT FAILED: {e}"),
            None => "NO SUBTITLES ADDED — open a file for them to run against first".to_string(),
        };
        eprintln!("{text}");
        self.notice = Some(text.into());
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
    /// way back is importing the file again. The notice says that rather than
    /// promising a ctrl+z that would do nothing.
    fn remove_subtitle_track(&mut self, track: usize, cx: &mut Context<Self>) {
        // The one availability oracle, for the same reason the × on a row and
        // the stroke are one call: an empty list is not a failure, it is an
        // action with nothing to act on, and the engine's "there is no subtitle
        // track 0" is an index nobody typed. A real removal that fails still
        // says what the engine said, below.
        if let Some(why) = self.enable(ActionId::RemoveSubtitleTrack, None).why() {
            let text = format!("NO SUBTITLES REMOVED — {why}");
            eprintln!("{text}");
            self.notice = Some(text.into());
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
                // ponytail: its atlas tile is not released -- `close_session`'s
                // note, for its reason and with its upgrade path.
                self.sub_image = None;
                format!("{name} REMOVED — importing the file again brings it back")
            }
            Some(Err(e)) => format!("NO SUBTITLES REMOVED — {e}"),
            None => "NO SUBTITLES REMOVED — open a file first".to_string(),
        };
        eprintln!("{text}");
        self.notice = Some(text.into());
        cx.notify();
    }

    /// Swaps the whole timeline for one restored from a `.edith`. Like an
    /// import this arrives by drop and so checks the export guard for itself.
    /// The new session is built before anything is replaced, so a refusal is
    /// shown as the engine worded it and leaves what is playing alone.
    fn load_project(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let opened = PlaybackSession::open_project(path).map_err(|e| e.to_string());
        self.install_project(path, opened, cx);
    }

    /// The second half of it, for [`Player::install_media`]'s reason: a
    /// `.edith` named on the command line is opened on a worker and only
    /// installed here.
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
        self.notice = Some(text.into());
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
        self.notice = Some(text.into());
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
                self.notice = Some(
                    format!(
                        "{} ADDED — drag a clip onto it, {} takes it back",
                        lane.label(),
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            None => self.notice = Some("NO TRACK ADDED — open a file first".into()),
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
        self.notice = Some(text.into());
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
                self.notice = Some("NO TRACK REMOVED — open a file first".into());
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
            self.notice = Some(text.into());
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
        self.notice = Some(text.into());
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
                None => self.notice = Some(NOTHING_TO_PLAY.into()),
            }
            cx.notify();
            return;
        }
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
            self.notice = Some("NOTHING TO EXPORT — open a file first".into());
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
            self.notice = Some(format!("NOT {} — {why}", format_label(format)).into());
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
            self.notice = Some(why.into());
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
            self.notice = Some(why.into());
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
    /// (the engine's own 1..20 Mbps), and picking the row is part of the step --
    /// a stepper that moves a number nobody is using would move nothing.
    fn nudge_mbps(&mut self, step: i32) {
        self.custom_mbps =
            (self.custom_mbps as i32 + step).clamp(MBPS_MIN as i32, MBPS_MAX as i32) as u32;
        self.quality = Quality::Custom;
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
                        this.notice = Some(text.into());
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
            self.notice = Some("NOTHING TO EXPORT — open a file first".into());
            cx.notify();
            return;
        };
        // An emptied timeline is a timeline; it is simply not a file. Refused by
        // name here rather than written as a project of no frames -- and the
        // engine refuses it again on the worker (`export::start`), so a caller
        // that is not this button cannot get past it either. Two fences on
        // purpose: this one is the one with a keystroke to blame.
        if session.is_empty() {
            self.notice = Some("NOTHING TO EXPORT — the timeline is empty".into());
            cx.notify();
            return;
        }
        // The format row can be refused *after* it was picked -- mp4 is the
        // default and an audio-only timeline (or a second audio lane) is one
        // edit away -- so the button asks again rather than starting a worker
        // that will only settle with the same refusal minutes later.
        if let Some(why) = format_refusal(session, self.format) {
            self.notice = Some(format!("NOT EXPORTED — {why}").into());
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
        self.notice = Some(text.into());
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
        self.scale = self.view().following(position);

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
                // Any key retires the last message, whatever it was -- and owes
                // the repaint itself: a notice no longer keeps the render loop
                // alive, and the arms below that do notify are not all of them
                // (an unbound key, or the copy chord, changes nothing else).
                if this.notice.take().is_some() {
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
                    // A project replaces the timeline, media joins the library.
                    if is_project(path) {
                        this.load_project(path, cx);
                    } else {
                        this.import(path, cx);
                    }
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
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                // A handle is 10 px across and the pointer leaves it at once, so
                // the equalizer drag is tracked here for the ruler's reason.
                if this.eq_dragging {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_band(event.position, cx);
                    } else {
                        // Released outside the window: the up below never came,
                        // so this is where the gesture ends -- and it still owes
                        // the one write the whole drag is worth.
                        this.eq_dragging = false;
                        this.commit_eq(cx);
                    }
                    return;
                }
                // A clip edge is 6 px wide and the pointer leaves it on the
                // first drag, so the gesture is tracked here for the same
                // reason -- and it ends here too when the button came up
                // outside the window, still owing its one edit.
                if this.trim.is_some() {
                    match event.pressed_button {
                        Some(MouseButton::Left) => this.trim_to(event.position.x, cx),
                        _ => this.commit_trim(cx),
                    }
                    return;
                }
                // A colour slider is 4 px tall and the pointer leaves it just as
                // fast; every sample is live, so the release owes no write of
                // its own -- what the last sample set is what the clip carries.
                if this.color_dragging {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_color(event.position.x, false, cx);
                    } else {
                        // The release happened outside the window, so this is
                        // where the gesture ends -- and it may not end on a
                        // sample the worker was too busy to take.
                        this.color_dragging = false;
                        this.flush_drag(cx);
                    }
                    return;
                }
                // The speed bar, the same 4 px and the same live writes: the
                // press took the undo step and every sample since is live.
                if this.speed_dragging {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_speed(event.position.x, false, cx);
                    } else {
                        this.speed_dragging = false;
                        this.flush_drag(cx);
                    }
                    return;
                }
                // The volume slider, the same live writes: what the hand is on
                // is what the speakers are doing, and there is nothing to undo.
                if this.volume_dragging {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_volume(event.position.x, cx);
                    } else {
                        this.volume_dragging = false;
                    }
                    return;
                }
                if !this.scrubbing {
                    return;
                }
                if event.pressed_button == Some(MouseButton::Left) {
                    this.scrub_to(event.position.x, false, cx);
                } else {
                    // A release outside the window never reaches the handler
                    // below, so the first button-up move is when we learn the
                    // drag is over. Without this the next hover would scrub.
                    this.scrubbing = false;
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    if std::mem::take(&mut this.eq_dragging) {
                        // The release lands exactly, then the gesture is written
                        // once -- the append-only table's whole reason.
                        this.drag_band(event.position, cx);
                        this.commit_eq(cx);
                        return;
                    }
                    if this.trim.is_some() {
                        // The release lands exactly, then the gesture is
                        // written once -- one edit, one undo step.
                        this.trim_to(event.position.x, cx);
                        this.commit_trim(cx);
                        return;
                    }
                    if std::mem::take(&mut this.color_dragging) {
                        // The release lands exactly where the hand let go, and
                        // it is a live write like every other sample: the undo
                        // step the gesture rolls back to was the press's. The
                        // flush is what makes "exactly" true while the worker is
                        // still busy -- the sample above would only be held.
                        this.drag_color(event.position.x, false, cx);
                        this.flush_drag(cx);
                        return;
                    }
                    if std::mem::take(&mut this.speed_dragging) {
                        this.drag_speed(event.position.x, false, cx);
                        this.flush_drag(cx);
                        return;
                    }
                    if std::mem::take(&mut this.volume_dragging) {
                        this.drag_volume(event.position.x, cx);
                        return;
                    }
                    if std::mem::take(&mut this.scrubbing) {
                        this.scrub_to(event.position.x, true, cx);
                    }
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(LETTERBOX))
            .text_color(rgb(INK))
            .text_size(px(13.))
            .child(
                div()
                    .flex_none()
                    .h(px(HEADER_H))
                    .flex()
                    .items_center()
                    .px(px(12.))
                    .bg(rgb(CHROME))
                    .child(self.name.clone()),
            )
            // The library beside the picture rather than under it: a media pool
            // is a column in every editor that has one, and the timeline below
            // already owns the full width. `library_w` is what keeps the
            // picture the majority of the row at every window size.
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
                            .overflow_hidden()
                            // The bed the cue plate is placed against: it hangs
                            // off the bottom of the picture region, which is the
                            // one box that is the picture and nothing else.
                            .relative()
                            .flex()
                            .justify_center()
                            .items_center()
                            .children(
                                self.image
                                    .clone()
                                    .map(|i| {
                                        img(i)
                                            .size_full()
                                            .object_fit(gpui::ObjectFit::Contain)
                                            .into_any_element()
                                    })
                                    // With no file open the letterbox is the
                                    // whole window, and a black rectangle says
                                    // only that something is broken -- so it
                                    // says what it wants instead. The window is
                                    // already the drop target.
                                    .or_else(|| {
                                        self.session
                                            .is_none()
                                            .then(|| empty_hint().into_any_element())
                                    }),
                            )
                            // After the picture, so the plate is drawn over it
                            // rather than under: siblings paint in order.
                            .children(self.subtitle_overlay(position, window)),
                    ),
            )
            // Above the panel and only when there is one to show, so it costs
            // the picture nothing the rest of the time. The import's line sits
            // over the notice's: a notice is about something that has already
            // happened, and this is about something still happening.
            .children(self.import_bar())
            // The same slot and the same reason: work still going on, said out
            // loud because a still picture is the only other evidence of it.
            .children(self.seek_bar())
            .children(self.notice_bar(cx))
            .child(self.panel(position, duration, state, cx))
            // Over the panel it was opened on, and under the cards: it is only
            // ever up while neither of them is (`modal`).
            .children(self.context_card(window.viewport_size(), cx))
            // The library's own menu, the same way and for the same reason:
            // over the panel it was opened on, under the cards, and never up
            // while one of them is.
            .children(self.library_card(window.viewport_size(), cx))
            // Last, so they are over everything -- they take no room in the
            // column, and only one of the two is ever up.
            .children(self.keys_overlay(cx))
            .children(self.export_card(window.viewport_size(), cx))
            .children(self.eq_card(window.viewport_size(), cx))
            .children(self.color_card(cx))
            .children(self.speed_card(cx))
            .children(self.silence_card(cx))
            .children(self.mix_card(cx))
            // Last, so it floats over whatever opened it -- the panel's button
            // or a clip menu -- rather than under it.
            .children(self.picker_card(window.viewport_size(), cx))
    }
}

impl Player {
    /// The media library: a row per source the timeline knows, in the order
    /// they arrived, each wearing the tint its clips wear in the lanes -- the
    /// swatch *is* what says which boxes down there came from this file. A
    /// click picks a row, the button under the list drops that source in at the
    /// playhead, and a row dragged onto either lane does the same thing through
    /// the same call.
    ///
    /// Import lives here, because this is the list it adds to. Plain divs like
    /// the rest of this window: nothing in it takes focus, so the root keeps
    /// the keyboard and the play key still works after a row is clicked
    /// (ledger:182).
    fn library(&self, width: f32, viewport_h: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let exporting = self.exporting().is_some();
        let sources = self
            .session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources);
        // Every source matches the first in size and rate or it was refused at
        // the door (the import policy, ledger:436), so the session's own meta
        // describes every row and nothing has to be probed to say so.
        let meta = self.session.as_ref().map(PlaybackSession::meta);
        // Its own length, not one derived from what is on the lanes: a row
        // imported and never placed is a row with a length, and it is the
        // length a drag would put down.
        let rows: Vec<_> = library_rows(
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
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let picked = self
                .selected_asset
                .as_ref()
                .is_some_and(|p| *p == (row.path.clone(), row.stream));
            let name: SharedString = row.name.clone().into();
            let says: String = match (&row.unusable, meta) {
                // A greyed row says why in full, where its length would be:
                // the list is the one place the file's own tracks are named.
                (Some(why), _) => format!("{} — {why}", row.path.display()),
                // A file with no picture has no size and no frame rate to
                // report, and only one lane it can go on: saying so is the
                // difference between a hint and a lie.
                (None, _) if engine::is_audio(&row.path) => format!(
                    "{} — audio only · drag onto the audio lane, or Add at playhead",
                    row.path.display()
                ),
                // A still is the mirror: its own size (the timeline's meta
                // describes video, and a picture placed on that canvas is not
                // the same shape), no frame rate, and one kind of lane.
                (None, _) if engine::is_image(&row.path) => format!(
                    "{} — still image{} · drag onto a video lane, or Add at playhead",
                    row.path.display(),
                    match self.sizes.get(&row.path).copied().flatten() {
                        Some((w, h)) => format!(" · {w}x{h}"),
                        None => String::new(),
                    }
                ),
                // The file's *own* frame rate, not the timeline's: a clip shot
                // at another rate plays at the speed it was shot at, and the
                // row that says where it came from has to say which rate that
                // was.
                (None, Some(meta)) => format!(
                    "{} — {}x{} @ {:.2} fps · drag it where you want it, or Add at playhead",
                    row.path.display(),
                    meta.width,
                    meta.height,
                    self.session
                        .as_ref()
                        .map_or(meta.frame_rate, |session| session.file_fps(&row.path))
                ),
                (None, None) => row.path.display().to_string(),
            };
            // The menu is the third way into a row, after the click and the
            // drag, and a right-click nothing advertises is one nobody finds.
            let tip: SharedString = format!("{says} · right-click for more").into();
            let ghost = name.clone();
            // What the second line says: the stream, then either its length or
            // the reason it cannot be used.
            let under = match &row.unusable {
                Some(why) => join_detail(&row.detail, why),
                // A still has no length to report -- the ten minutes it is
                // *held* to is a wall, not a duration -- so the line says what
                // it is and how big it is instead.
                None if engine::is_image(&row.path) => join_detail(
                    &row.detail,
                    &match self.sizes.get(&row.path).copied().flatten() {
                        Some((w, h)) => format!("still image · {w}x{h}"),
                        None => "still image".to_string(),
                    },
                ),
                None => join_detail(
                    &row.detail,
                    &timecode(f64::from(row.frames) / self.fps, self.fps),
                ),
            };
            let usable = row.unusable.is_none();
            let (path, stream) = (row.path.clone(), row.stream);
            let dragged = (path.clone(), stream);
            let menu_path = path.clone();
            div()
                .id(("asset", i))
                .flex_none()
                .h(px(ROW_H))
                .flex()
                .items_center()
                .gap(px(6.))
                .pr(px(6.))
                .rounded(px(3.))
                // A row that cannot be placed takes no click and no drag, and
                // reads as unavailable rather than merely unlucky.
                .when(!usable, |d| d.text_color(rgb(INK_DIM)).opacity(0.55))
                .when(usable, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(rgb(HOVER)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.selected_asset = Some((path.clone(), stream));
                            cx.notify();
                        }))
                        // The drag carries the row's file and stream and the
                        // row's name: one for the drop to insert, one for the
                        // pointer to carry, so what is being dragged is legible
                        // on the way down.
                        .on_drag(AssetDrag(dragged.0, dragged.1), move |_, _, _, cx| {
                            cx.new(|_| Tip(ghost.clone()))
                        })
                })
                // The right button hangs the row's own menu at the pointer.
                // Every row takes it, greyed ones included: a file that cannot
                // join this timeline can still be revealed, described and taken
                // out of the list, and Add is the one item that then refuses --
                // in the engine's words, where the row's grey already says why.
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        if this.modal() {
                            return;
                        }
                        // Picked as a left-click would pick it, so the row the
                        // menu is about is the row that reads as chosen -- but
                        // only a row that *can* be picked, which is what keeps
                        // the Add button under the list honest.
                        if usable {
                            this.selected_asset = Some((menu_path.clone(), stream));
                        }
                        this.library_menu = Some(LibraryMenu {
                            path: menu_path.clone(),
                            stream,
                            at: event.position,
                            details: false,
                        });
                        cx.notify();
                    }),
                )
                .when(picked, |d| d.bg(rgb(SELECTED)).border_1())
                .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                // Full height and hard against the edge: the tint reads as
                // the lane's colour continuing into the list, not as a chip
                // that happens to be near it.
                .child(
                    div()
                        .flex_none()
                        .w(px(SWATCH_W))
                        .h_full()
                        .rounded(px(2.))
                        .bg(rgb(source_tint(row.tint))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        // Two lines rather than two columns: at the width
                        // this panel yields to, a name and a timecode side
                        // by side leave room for neither.
                        // Cut out of the middle, not the end: two episodes off
                        // one release are the same words up to the number, and
                        // the number is at the end.
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.))
                                .child(clip_middle(&name, row_text_w(width))),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(10.))
                                .text_color(rgb(INK_DIM))
                                .child(under),
                        ),
                )
        })
        .collect();
        div()
            .flex_none()
            .w(px(width))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(8.))
            .bg(rgb(CHROME))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(11.))
                            .text_color(rgb(INK_DIM))
                            .child("Media"),
                    )
                    .child(control(
                        "import",
                        None,
                        "Import",
                        "adds a file to this list — or drop one on the window".to_string(),
                        !exporting,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.pick_and_import(cx)),
                    )),
            )
            .child(
                div()
                    .id("library-rows")
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .overflow_y_scroll()
                    // Never a blank column: with nothing imported the list is
                    // where the way in is said.
                    .when(rows.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child("No media yet — Import, or drop a file on the window"),
                        )
                    })
                    .children(rows),
            )
            // Under the media it belongs to: a subtitle track is not a source --
            // it goes on no lane and is dragged nowhere -- but it is a thing the
            // timeline holds, and this is the list of those.
            .children(self.subtitle_section(width, viewport_h, cx))
            .child(control(
                "add-asset",
                None,
                "Add at playhead",
                match self.selected_asset {
                    Some(_) => "inserts the picked file at the playhead".to_string(),
                    None => "click a file above first — or drag one where you want it".to_string(),
                },
                can_add(
                    self.selected_asset.as_ref(),
                    self.session.is_some(),
                    exporting,
                ),
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some((path, stream)) = this.selected_asset.clone() {
                        // No lane: the button means "wherever this belongs",
                        // which for a file with no picture is the audio lane.
                        this.insert_source(&path, stream, None, None, cx);
                    }
                }),
            ))
    }

    /// The subtitle tracks this timeline holds, under the media list: one row
    /// each, the picked one marked, and a click makes another one the picked one
    /// -- which is the whole of choosing between the two tracks of a film. A row
    /// per track and no cycle: three of them is an ordinary number for a remux,
    /// and a key that steps through three is a key nobody can aim.
    ///
    /// A track that could not be read is *here*, greyed and saying why, exactly
    /// as a media row the timeline cannot take is: PGS subtitles are pictures,
    /// and a film carrying four of them says so instead of listing nothing.
    ///
    /// Every row names the file it came out of itself, in words and in the tint
    /// that file's clips wear on the lanes -- but only where there is more than
    /// one file to tell apart, the way [`row_name`] numbers audio streams only
    /// where a file gave several. Where the window is tall enough for it
    /// ([`sub_headers_fit`]) each file's block is headed by its name as well: a
    /// label and nothing more, no click and nothing to fold, so the rows under
    /// it are the only things anybody has to aim at. At the 640x360 floor the
    /// headers are gone and the rows still say whose they are.
    ///
    /// `None` when there are none -- an empty heading is a section about
    /// nothing.
    fn subtitle_section(
        &self,
        width: f32,
        viewport_h: f32,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let tracks = self.session.as_ref()?.subtitles();
        if tracks.is_empty() {
            return None;
        }
        let groups = subtitle_rows(tracks);
        // One file's tracks need no prefix saying which file: it is the only
        // one, and every row would carry the same word.
        let several_files = groups.len() > 1;
        let headed = several_files && sub_headers_fit(viewport_h);
        let text_w = row_text_w(width);
        let rows: Vec<_> = groups
            .into_iter()
            .map(|SubGroup { name, path, rows }| {
                // The file's own colour, the one its media rows and its clips
                // wear -- and none at all for a standalone `.srt`, which came
                // off no file on this timeline.
                let tint = file_tint(self.sources(), &path);
                let numbered = rows.len() > 1;
                // The name twice over: all of it the header can hold, and the
                // share of a row a prefix may take in front of the label.
                let head = clip_middle(&name, text_w);
                let prefix = clip_middle(&name, text_w * SUB_STEM_SHARE);
                let rows: Vec<_> = rows
                    .into_iter()
                    .map(|row| {
                        let track = row.track;
                        let picked = track == self.sub_track;
                        let usable = row.refused.is_none();
                        // Two tracks off one remux that both say "eng" are told
                        // apart by their number and by nothing else -- the same
                        // count [`sub_pick_name`] echoes.
                        let title = match numbered {
                            true => format!("{} {}", row.label, row.number),
                            false => row.label,
                        };
                        // A standalone `.srt` is named after its own file, so
                        // the file in front of it says the same word twice
                        // ("Legend.of.… · Legend.of.…"). [`sub_pick_name`]'s
                        // rule, on the row it is about.
                        let owned = several_files && !title.starts_with(name.as_str());
                        // The whole path, never clipped: the row says which
                        // file, and the tooltip says which one on disk.
                        let tip: SharedString = match &row.refused {
                            Some(why) => format!("{} — {why}", path.display()),
                            None => format!(
                                "{} — click to show this track over the picture",
                                path.display()
                            ),
                        }
                        .into();
                        // Named, because a × is the same glyph on every row and
                        // the tooltip is what says which track it takes off.
                        let remove_tip: SharedString =
                            format!("Remove {title} — importing the file again brings it back")
                                .into();
                        div()
                            // The *flat* index into the session's add-order
                            // list, which is what a pick is and what a save
                            // writes: the grouping moved the row on screen and
                            // never the track it stands for.
                            .id(("subtitle-track", track))
                            .flex_none()
                            .h(px(ROW_H))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pr(px(6.))
                            .rounded(px(3.))
                            .when(!usable, |d| d.text_color(rgb(INK_DIM)).opacity(0.55))
                            .when(usable, |d| {
                                d.cursor_pointer()
                                    .hover(|s| s.bg(rgb(HOVER)))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.sub_track = track;
                                        // Picking a track is asking to see it: a
                                        // click that changed nothing on screen
                                        // because the toggle was off would read
                                        // as a dead row.
                                        this.subs_on = true;
                                        cx.notify();
                                    }))
                            })
                            .when(picked, |d| d.bg(rgb(SELECTED)))
                            .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                            // The media rows' bar, same width and hard against
                            // the same edge: one association across the panel
                            // and the lanes. Kept as room rather than dropped
                            // where there is no tint, so a standalone `.srt`
                            // still lines its words up with the rest.
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(SWATCH_W))
                                    .h_full()
                                    .rounded(px(2.))
                                    .when_some(tint, |d, tint| d.bg(rgb(tint))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .flex()
                                            .text_size(px(11.))
                                            // In front, because the column
                                            // truncates from the right: an
                                            // ownership word at the end is a
                                            // word the floor never shows.
                                            .when(owned, |d| {
                                                d.child(
                                                    div()
                                                        .flex_none()
                                                        // Said twice where a
                                                        // header says it above:
                                                        // still there, out of
                                                        // the way.
                                                        .when(headed, |d| {
                                                            d.text_color(rgb(INK_DIM))
                                                        })
                                                        .child(format!("{prefix} · ")),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .truncate()
                                                    .child(title),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(10.))
                                            .text_color(rgb(INK_DIM))
                                            .child(row.detail),
                                    ),
                            )
                            // The way back off the timeline, on every row and on
                            // the last one too -- a list of subtitles is allowed
                            // to be empty, unlike a lane. A `HIT_MIN` target and
                            // never hidden, the lane header's ×, and it stops
                            // the click there: the row under it picks a track,
                            // and picking the track that has just gone would
                            // leave the pick naming nothing.
                            .child(
                                div()
                                    .id(("subtitle-remove", track))
                                    .flex_none()
                                    .w(px(HIT_MIN))
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(HOVER)))
                                    .tooltip(move |_, cx| cx.new(|_| Tip(remove_tip.clone())).into())
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.remove_subtitle_track(track, cx);
                                        },
                                    ))
                                    .child("×"),
                            )
                    })
                    .collect();
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    // The header: which film these came out of, in words and not
                    // in colour alone. No id, no click, nothing to fold -- a
                    // label, which is why it is allowed under `HIT_MIN`.
                    .when(headed, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .h(px(SUB_HEAD_H))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(10.))
                                .text_color(rgb(INK_DIM))
                                .when_some(tint, |d, tint| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .w(px(SWATCH_W))
                                            .h_full()
                                            .rounded(px(2.))
                                            .bg(rgb(tint)),
                                    )
                                })
                                .child(div().flex_1().min_w(px(0.)).truncate().child(head)),
                        )
                    })
                    .children(rows)
            })
            .collect();
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(rgb(INK_DIM))
                        .child(match (self.subs_on, sub_pick_name(tracks, self.sub_track)) {
                            // Which of them is on screen, named by its film:
                            // the heading over a list of five is where the one
                            // being shown is worth saying out loud.
                            (true, Some(pick)) => format!("Subtitles — {pick}"),
                            (true, None) => "Subtitles".to_string(),
                            // The toggle's state where the tracks are listed: a
                            // list of subtitles nothing on screen is showing has
                            // to say that it is showing none.
                            (false, _) => "Subtitles — hidden".to_string(),
                        }),
                )
                .child(
                    div()
                        .id("subtitle-rows")
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .max_h(px(SUB_ROWS_H))
                        .overflow_y_scroll()
                        .children(rows),
                ),
        )
    }

    /// The cues of the picked track that are on screen at `at`, over the picture
    /// and nothing else: bottom-centred where every player puts them, white on a
    /// plate so the film underneath cannot swallow them, and each cue its own
    /// plate so two at one moment stack rather than run together.
    ///
    /// `None` -- no element at all -- while the toggle is off, with no track
    /// picked, and between cues: the picture is what this window is for, and a
    /// permanent empty band across it would be in the way of exactly that.
    ///
    /// The cues are the *timeline's* ([`PlaybackSession::timeline_cues`]) and
    /// not the track's own: on a cut timeline an embedded track's cues ride the
    /// pictures they belong to, and this is the same map the export writes the
    /// file with -- so what is read here is what the file says.
    ///
    /// A cue off a PGS track is a *picture* and not a line
    /// ([`engine::subtitle::CueImage`]), and is drawn as one: the disc's whole
    /// canvas fitted over the picture region exactly as the picture itself is,
    /// which puts every cue where the disc put it relative to its own frame.
    ///
    /// ponytail: exact only while the canvas and the encode are the same shape
    /// -- a 16:9 canvas over a 2.39:1 encode fits to the region's height and
    /// the film to its width, so a cue sits a little low on a scope film. The
    /// upgrade path is the picture's own rect, which wants `VideoMeta`'s aspect
    /// and the measured bounds rather than the shared `Contain`.
    fn subtitle_overlay(
        &mut self,
        at: f64,
        window: &mut Window,
    ) -> Option<impl IntoElement + use<>> {
        // One way out, and it lets the drawn picture go on the way: the toggle
        // going off, the file closing and the gap between two cues are the same
        // "nothing on screen", and an 8 MB atlas tile may not survive any of
        // them (an early return above this leaked one per toggle-off).
        let mapped = match self.session.as_ref().filter(|_| self.subs_on) {
            Some(session) => session.timeline_cues(self.sub_track),
            None => Vec::new(),
        };
        let cues = cues_at(&mapped, at);
        if cues.is_empty() {
            self.drop_sub_image(window);
            return None;
        }
        // The first picture cue up, decoded once and kept: two bitmap cues at
        // one moment is a thing PGS composes into one display set, so there is
        // never a second picture to stack under the first.
        let picture = cues
            .iter()
            .find_map(|cue| Some((cue.start_us, cue.image.as_ref()?)))
            .and_then(|(start_us, image)| self.sub_picture(start_us, image, window));
        // A picture is fitted onto the whole region and a plate hangs off the
        // bottom of it, and a track is one or the other -- so they are two
        // shapes and not one with the parts switched off.
        if let Some(image) = picture {
            // A *flex* box with the canvas as its one growing item: a percentage
            // size (`size_full`) inside an absolutely placed box has nothing to
            // be a percentage of and lays the picture out to nothing, while a
            // flex item is sized by the box itself. Fitted the way the picture
            // above it is -- `Contain` over the same box -- so a canvas of the
            // picture's own shape lands exactly on it.
            return Some(div().absolute().inset_0().flex().child(
                img(image).flex_1().h_full().object_fit(gpui::ObjectFit::Contain),
            ));
        }
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(SUB_BOTTOM))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                // The plate takes no click: the picture behind it is still the
                // drop target the whole window is.
                .children(cues.into_iter().filter(|c| c.image.is_none()).map(|cue| {
                    div()
                        .max_w(relative(0.9))
                        .px(px(6.))
                        .rounded(px(3.))
                        .bg(rgba(SUB_SHADE))
                        .text_size(px(SUB_TEXT))
                        .text_color(rgb(SUB_INK))
                        .text_align(TextAlign::Center)
                        // A line of the cue is a line on screen: the break the
                        // parser kept is not whitespace to be re-flowed. What a
                        // *long* line does is wrap inside its own div, which is
                        // what the width cap above is for.
                        .children(
                            cue.text
                                .split('\n')
                                .map(|line| div().min_h(px(SUB_LINE_H)).child(line.to_string())),
                        )
                })),
        )
    }

    /// The cue starting at `start_us` as a drawable picture, decoded on the
    /// first repaint it is up for and kept until another cue takes its place
    /// ([`Player::sub_image`]). `None` for a display set the decoder refuses,
    /// which draws nothing rather than failing the frame.
    ///
    /// Its atlas tile is released as the video's is: every [`RenderImage`] gets
    /// a fresh id and its own tile, so a film's worth of cues would grow the
    /// sprite atlas by the whole film.
    fn sub_picture(
        &mut self,
        start_us: i64,
        image: &engine::subtitle::CueImage,
        window: &mut Window,
    ) -> Option<Arc<RenderImage>> {
        let key = (self.sub_track, start_us);
        if let Some((up, ready)) = &self.sub_image
            && *up == key
        {
            return Some(ready.clone());
        }
        let mut rgba = image.rgba()?;
        // gpui's atlas is BGRA with straight alpha; PGS decodes to RGBA with
        // straight alpha. The same swap the video frames get.
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let buf = image::RgbaImage::from_raw(image.width, image.height, rgba)?;
        let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
        self.drop_sub_image(window);
        self.sub_image = Some((key, next.clone()));
        Some(next)
    }

    /// Lets go of the drawn cue and its atlas tile. Called where the cue stops
    /// being on screen, which is every gap between two of them: an 8 MB tile
    /// per cue is not a thing to leave behind a film.
    fn drop_sub_image(&mut self, window: &mut Window) {
        if let Some((_, old)) = self.sub_image.take() {
            let _ = window.drop_image(old);
        }
    }

    /// What is decoding the picture right now, for the transport line: the
    /// backend is the running worker's own (it is written where a hardware
    /// session falls back to software, so this follows reality), and the codec
    /// comes from the clip under the playhead. Empty when nothing is playing --
    /// the question is about what is happening, not about what would.
    fn live_decode(&self, position: f64, playing: bool) -> String {
        let Some(session) = self.session.as_ref().filter(|_| playing) else {
            return String::new();
        };
        let backend = session.decode_backend();
        let codec = session
            .video_clip_at(position)
            .and_then(|(lane, idx)| session.lane_clips(lane).get(idx).map(|clip| clip.source))
            .and_then(|source| session.sources().get(source))
            .and_then(|source| self.decoders.get(&source.path).copied().flatten())
            .and_then(|(codec, _)| codec);
        match backend {
            // Neither is a decode, and saying "SW" of them would be a lie.
            Backend::Gap => "gap · nothing to decode".to_string(),
            Backend::Still => "still · one decode, held".to_string(),
            _ => format!("{} decode", decode_label(codec, backend)),
        }
    }

    /// Transport, edit and file buttons, timecode, playhead, clips lane.
    fn panel(
        &self,
        position: f64,
        duration: f64,
        state: Transport,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Where the playhead is *on the bed*, in pixels from its left edge.
        // Clamped to the bed because it is drawn as a width as well as an
        // offset, and the view follows the playhead anyway, so it is never off
        // the bed for long.
        let bed_w = f32::from(self.ruler.get().size.width);
        let filled = self.scale.px_at(position).clamp(0., bed_w);
        // An export owns the hint slot and the ruler while it runs: the
        // percentage and the accent bar are the same number, so the playhead
        // fill doubles as the progress bar for free.
        let exporting = self.exporting().is_some();
        // Everything but Import and Keys needs a timeline to act on: with none
        // open they are dimmed rather than silently doing nothing.
        let live = state != Transport::Stopped && !exporting;
        // The project's own picture size, for the button that cycles it: read
        // per render like everything else here, so a cycle shows on the button
        // that made it.
        let resolution = self.session.as_ref().map(PlaybackSession::resolution);
        let key = |action| self.keymap.display(action);
        // The lanes the project has, or the pair a fresh one starts with so the
        // panel reads the same before a file is open as after.
        let lanes = self
            .session
            .as_ref()
            .map_or_else(|| vec![Lane::V1, Lane::A1], PlaybackSession::lanes);
        // A loop rather than a `map`: each row takes `cx` in turn, where a
        // closure would hold it for as long as the iterator lives.
        let mut rows = Vec::new();
        for &lane in &lanes {
            rows.push(self.lane_row(lane, filled, cx));
        }
        // Built from the same playhead pixel the lanes are, and before the
        // export takes `filled` over below: the strip is a picture of the
        // timeline, not of an export's progress.
        let strip = self.subtitle_strip(filled);
        let strip_h = subtitle_strip_h(strip.is_some());
        let (hint, filled) = if let Some(export) = self.exporting() {
            let progress = export.progress();
            let elapsed = self
                .export_started
                .map_or(0., |t| t.elapsed().as_secs_f32());
            // Two numbers that must both be honest: the one that counts up is
            // measured, the one that counts down is a guess and says so.
            let left = eta_secs(&self.export_marks, elapsed, progress).map_or_else(
                || "estimating…".to_owned(),
                |s| format!("~{} left", clock(s)),
            );
            (
                format!(
                    // Clocks, then the way out, then what is encoding: at the
                    // 640 px floor the tail is what truncation eats, and the
                    // codec pair is the one part of this line the card already
                    // said. The escape must not be what goes missing.
                    "EXPORTING {}% · {} elapsed · {left} — {} cancels · {} · {}",
                    (progress * 100.) as u32,
                    clock(elapsed),
                    key(ActionId::CancelExport),
                    // The row that was picked; the engine's line below names the
                    // seats alone, since the library is what identifies a codec.
                    format_label(self.format),
                    // What the worker actually opened, so a fallback to the
                    // software encoder shows here rather than being invisible.
                    export
                        .encoders()
                        .unwrap_or_else(|| "opening the encoder".to_string()),
                ),
                progress,
            )
        } else {
            // The strokes no button carries; the rest ride on the buttons'
            // tooltips. Keys first: at a 640 px window the tail is what a
            // truncation eats, and the two hints at the end are also on the
            // ruler's and Import's tooltips.
            // While it plays, what is decoding it goes first: it is the
            // answer that changes as the playhead crosses a cut, and the tail
            // of this line is what a narrow window truncates.
            (
                join_detail(
                    &self.live_decode(position, state.is_playing()),
                    &format!(
                        "{} copy · {} paste · {} undo · click the bar to seek · drop a file to \
                         import",
                        key(ActionId::Copy),
                        key(ActionId::Paste),
                        key(ActionId::Undo)
                    ),
                ),
                filled,
            )
        };
        div()
            .flex_none()
            .h(px(panel_h(lanes.len()) + strip_h))
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .bg(rgb(CHROME))
            // Transport | edit | file: three groups, so the eye can skip two of
            // them. Every button says what it does; the tooltip adds its key.
            //
            // The row scrolls rather than losing its tail: at the 640 px floor
            // this window is sized for it is wider than the panel, and a button
            // off the right edge is a button a pointer cannot press. A plain
            // wheel moves it -- gpui puts a vertical delta on the x axis when
            // that is the only one scrolling (div.rs:2424).
            //
            // ...and "it scrolls" is not "it can be found": the button beside
            // the row is pinned outside it and never scrolls away, so Export,
            // Save and the rest are one press from the smallest window even
            // when the row's tail is off the edge.
            // `toolbar_fits_the_smallest_window` is the guard.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("controls")
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .overflow_x_scroll()
                            .child(control(
                                "transport",
                                Some(transport_glyph(state).into_any_element()),
                                if state.is_playing() { "Pause" } else { "Play" },
                                if nothing_to_play(self.session.as_ref()) && !state.is_playing() {
                                    format!("{} — put a clip on a lane first", key(ActionId::Play))
                                } else {
                                    key(ActionId::Play)
                                },
                                // An empty timeline has nothing to play, so the button
                                // says so by being dim rather than by starting a clock
                                // against nothing (the key press answers with the
                                // notice). Still live while it *is* playing: a delete
                                // can empty the timeline mid-play and that has to stop.
                                live && (state.is_playing() || !nothing_to_play(self.session.as_ref())),
                                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_or_restart(cx)),
                            ))
                            .child(separator())
                            .child(control(
                                "cut",
                                Some(cut_glyph().into_any_element()),
                                "Cut",
                                format!(
                                    "{} — splits the clip under the playhead",
                                    key(ActionId::Cut)
                                ),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| this.cut(cx)),
                            ))
                            .child(control(
                                "delete",
                                Some(delete_glyph().into_any_element()),
                                "Delete",
                                if self.selected.is_some() {
                                    key(ActionId::Delete)
                                } else {
                                    format!("{} — click a clip below first", key(ActionId::Delete))
                                },
                                live && self.selected.is_some(),
                                cx.listener(|this, _: &ClickEvent, _, cx| this.delete_selected(cx)),
                            ))
                            // The way back from every one of them. It was a stroke and
                            // nothing else, which made the whole edit group a one-way
                            // door for anyone working with a pointer.
                            .child(control(
                                "undo",
                                None,
                                "Undo",
                                format!("{} — takes the last edit back", key(ActionId::Undo)),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| this.undo(cx)),
                            ))
                            // With the edit group, beside the buttons that change the
                            // edit list: a track is a row of it, and adding one is an
                            // undoable edit like the rest. Two buttons rather than one
                            // with a choice, because the choice is the whole action.
                            .child(control(
                                "add-video-lane",
                                None,
                                "+ V",
                                format!(
                                    "{} — adds a video track under the ones there",
                                    key(ActionId::AddVideoLane)
                                ),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.add_lane(LaneKind::Video, cx)
                                }),
                            ))
                            .child(control(
                                "add-audio-lane",
                                None,
                                "+ A",
                                format!(
                                    "{} — adds an audio track under the ones there",
                                    key(ActionId::AddAudioLane)
                                ),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.add_lane(LaneKind::Audio, cx)
                                }),
                            ))
                            // The third kind of track this timeline carries, and
                            // the only one that arrived through a drop alone: a
                            // subtitle file nobody thought to drag was a track
                            // the editor could not be asked for. What it opens is
                            // a chooser rather than an empty row -- a subtitle
                            // track is its file, and there is nothing to add
                            // before one is named.
                            .child(control(
                                "add-subtitle-track",
                                None,
                                "+ S",
                                // Dim, and saying why it is dim, out of the one
                                // oracle -- the very words the key answers with
                                // (`Player::pick_and_add_subtitles`), so the
                                // tooltip and the notice cannot come to differ.
                                match self.enable(ActionId::AddSubtitleTrack, None).why() {
                                    Some(why) => {
                                        format!("{} — {why}", key(ActionId::AddSubtitleTrack))
                                    }
                                    None => format!(
                                        "{} — adds every subtitle track in a file you pick",
                                        key(ActionId::AddSubtitleTrack)
                                    ),
                                },
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.pick_and_add_subtitles(cx)
                                }),
                            ))
                            // With the transport, not the edit group: it changes what
                            // the file sounds like, never what it is. Reads as its own
                            // state, so muted is legible without a tooltip.
                            .child(control(
                                "volume",
                                None,
                                self.volume.label(),
                                format!(
                                    "{} mutes · {} louder · {} quieter",
                                    key(ActionId::ToggleMute),
                                    key(ActionId::VolumeUp),
                                    key(ActionId::VolumeDown)
                                ),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.set_volume(|volume| volume.muted = !volume.muted, cx)
                                }),
                            ))
                            // The level itself, to drag. The button beside it stays the
                            // mute -- one gesture each -- and the label above is what
                            // this writes, live, so the number follows the hand.
                            .child(volume_slider(
                                self.volume,
                                self.volume_bar.clone(),
                                live,
                                cx,
                            ))
                            .child(separator())
                            // The size every clip is composed onto, which is also the
                            // size the export comes out at. Five sizes, so a click opens
                            // the list of them with this one marked and picks whichever
                            // was meant: a button that stepped one on per press made the
                            // user click round the ladder to get back to where they
                            // were. The stroke still steps, for the hand already on it.
                            .child(control(
                                "resolution",
                                None,
                                resolution.map_or_else(|| "Size".to_string(), |(_, h)| format!("{h}p")),
                                match resolution {
                                    Some((w, h)) => format!(
                                        "click to pick a size — the project is {w}x{h}; {} steps to the next",
                                        key(ActionId::Resolution)
                                    ),
                                    None => format!("{} — open a file first", key(ActionId::Resolution)),
                                },
                                live,
                                cx.listener(|this, event: &ClickEvent, _, cx| {
                                    this.open_picker(Pick::Resolution, event.position(), cx)
                                }),
                            ))
                            // The rate every clip is counted in, which is also the rate
                            // the export is written at -- the other half of "the project
                            // is not the media", and the one the panel could only ever
                            // read out before. A click opens the rates with this one
                            // marked; picking one conforms the whole timeline to it.
                            .child(control(
                                "fps",
                                None,
                                match self.session.is_some() {
                                    true => format!("{} fps", fps_label(self.fps)),
                                    false => "Rate".to_string(),
                                },
                                match self.session.is_some() {
                                    true => format!(
                                        "click to pick a frame rate — the project is cut at {} fps",
                                        fps_label(self.fps)
                                    ),
                                    false => "the project's frame rate — open a file first".to_string(),
                                },
                                live,
                                cx.listener(|this, event: &ClickEvent, _, cx| {
                                    this.open_picker(Pick::Fps, event.position(), cx)
                                }),
                            ))
                            // ...and which rendition its HDR media is watched in, the
                            // third thing that is the project's rather than the media's.
                            // Listed whatever is on the timeline, and the tooltip says
                            // who it acts on: a control that came and went with the
                            // media would be one nobody could find when they wanted it.
                            .child(control(
                                "tonemap",
                                None,
                                match &self.session {
                                    Some(session) => format!("HDR {}", tone_label(session.tone())),
                                    None => "HDR".to_string(),
                                },
                                match &self.session {
                                    Some(session) => format!(
                                        "click to pick the HDR rendition — the project is {}; SDR media is untouched",
                                        tone_label(session.tone())
                                    ),
                                    None => "the HDR rendition — open a file first".to_string(),
                                },
                                live,
                                cx.listener(|this, event: &ClickEvent, _, cx| {
                                    this.open_picker(Pick::Tone, event.position(), cx)
                                }),
                            ))
                            // How much of the timeline the panel below is showing --
                            // beside the resolution, since neither of them edits
                            // anything: they are both what is being looked at. The
                            // middle one says how much of it is on the bed, and is the
                            // way back to the whole of it.
                            .child(control(
                                "zoom-out",
                                None,
                                "−",
                                format!(
                                    "{} — show more of the timeline; stops with all of it on the bed",
                                    key(ActionId::ZoomOut)
                                ),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.zoom(1. / ZOOM_STEP, None, cx)
                                }),
                            ))
                            .child(control(
                                "zoom-fit",
                                None,
                                // How much timeline is on the bed, not a multiple of
                                // "the whole of it": the scale no longer knows what the
                                // whole of it is, and a number that changed on every
                                // import was a number that lied.
                                span_label(self.view().span()),
                                format!(
                                    "{} — fit the whole timeline; showing {} to {}",
                                    key(ActionId::ZoomFit),
                                    timecode(self.scale.start, self.fps),
                                    timecode(
                                        (self.scale.start + self.view().span()).min(duration),
                                        self.fps
                                    )
                                ),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| this.zoom_fit(cx)),
                            ))
                            .child(control(
                                "zoom-in",
                                None,
                                "+",
                                format!(
                                    "{} — magnify around the playhead; ctrl+wheel on the bar zooms at the pointer",
                                    key(ActionId::ZoomIn)
                                ),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| this.zoom(ZOOM_STEP, None, cx)),
                            ))
                            // The magnet, beside the zoom for the zoom's own reason: it
                            // changes nothing on the timeline, it changes how the hand
                            // meets it. The label is the state, as the volume's is --
                            // a toggle that looks the same either way says nothing.
                            .child(control(
                                "snap",
                                None,
                                if self.snap { "Snap" } else { "No snap" },
                                format!(
                                    "{} — {}",
                                    key(ActionId::ToggleSnap),
                                    match self.snap {
                                        true => "drags and trims land on clip edges, the playhead and the start",
                                        false => "drags and trims land exactly where the hand leaves them",
                                    }
                                ),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_snap(cx)),
                            ))
                            // The cues over the picture, off and on -- beside the zoom
                            // and the snap, since like both of them it changes what is
                            // being looked at and nothing about the edit. The label is
                            // the state, as the snap's is, and it says what there is to
                            // toggle when there is nothing.
                            .child(control(
                                "subs",
                                None,
                                match (self.subtitle_track().is_some(), self.subs_on) {
                                    (false, _) => "No subs",
                                    (true, true) => "Subs",
                                    (true, false) => "Subs off",
                                },
                                match self.enable(ActionId::ToggleSubtitles, None).why() {
                                    Some(why) => format!(
                                        "{} — {why}; drop a .srt on the window, or open an mkv that carries them",
                                        key(ActionId::ToggleSubtitles)
                                    ),
                                    None => format!(
                                        "{} — {}",
                                        key(ActionId::ToggleSubtitles),
                                        match self.subs_on {
                                            true => "the cue under the playhead is drawn over the picture",
                                            false => "the cues are on the timeline and off the picture",
                                        }
                                    ),
                                },
                                live && self.enable(ActionId::ToggleSubtitles, None).yes(),
                                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_subtitles(cx)),
                            ))
                            // Import is not here: it belongs to the media list it adds
                            // to, and two doors into one action is a question about
                            // which one is the real one.
                            //
                            // While one runs, this button is the way out of it: the
                            // progress line promised esc and nothing else, which left a
                            // pointer no way to stop an export it had started.
                            .child(control(
                                "export",
                                None,
                                if exporting { "Cancel" } else { "Export" },
                                if exporting {
                                    format!(
                                        "{} — stops the export; the part-written file goes",
                                        key(ActionId::CancelExport)
                                    )
                                } else {
                                    format!(
                                        "{} — quality and destination, then writes the timeline out",
                                        key(ActionId::Export)
                                    )
                                },
                                if exporting {
                                    true
                                } else {
                                    live && self.export.is_none()
                                },
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    if this.exporting().is_some() {
                                        this.cancel_export();
                                        cx.notify();
                                    } else {
                                        this.open_export(cx);
                                    }
                                }),
                            ))
                            .child(control(
                                "save",
                                None,
                                "Save",
                                format!("{} — writes the project file", key(ActionId::Save)),
                                live,
                                cx.listener(|this, _: &ClickEvent, _, cx| this.save_project(cx)),
                            ))
                            // Closed while an export runs, which is what keeps a waiting
                            // row from swallowing the escape the progress line promises
                            // cancels the export.
                            .child(control(
                                "keys",
                                None,
                                "Actions",
                                // The pointer's way to every action there is, including
                                // the ones no button here has room for.
                                format!(
                                    "{} — do any action, or change the key that does it",
                                    key(ActionId::ShowActions)
                                ),
                                !exporting,
                                cx.listener(|this, _: &ClickEvent, _, cx| match this.keys_open {
                                    true => {
                                        this.keys_open = false;
                                        this.rebinding = None;
                                        cx.notify();
                                    }
                                    false => this.show_actions(cx),
                                }),
                            ))
                    )
                    // The door to everything the row cannot show at this
                    // window: the same card the Actions button opens, by the
                    // same call, pinned where a scroll cannot take it.
                    .child(control(
                        "controls-more",
                        None,
                        "⋯",
                        format!(
                            "{} — every action, including the ones scrolled off this row",
                            key(ActionId::ShowActions)
                        ),
                        !exporting,
                        cx.listener(|this, _: &ClickEvent, _, cx| match this.keys_open {
                            true => {
                                this.keys_open = false;
                                this.rebinding = None;
                                cx.notify();
                            }
                            false => this.show_actions(cx),
                        }),
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    // Fixed width and one line: changing digits must not push
                    // the row around, nor wrap and change its height.
                    // The timeline's own length, never the drawn one: a tail
                    // being dragged inflates the bed to the room that edge has
                    // ([`Player::drawn_duration`]), and for a still that room is
                    // ten minutes -- a total that jumped to 10:00:00 the moment
                    // a picture's edge was pressed, and back on release.
                    .child(div().flex_none().w(px(TIME_W)).truncate().child(format!(
                        "{} / {}",
                        timecode(position, self.fps),
                        timecode(
                            self.session
                                .as_ref()
                                .map_or(0., PlaybackSession::timeline_duration),
                            self.fps
                        )
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(11.))
                            .text_color(rgb(INK_DIM))
                            .child(hint),
                    ),
            )
            // Press to seek, drag to scrub: the move and release halves live on
            // the root, since the pointer leaves the bar immediately. The bar
            // stays 6 px to look at; the strip that takes the click is 24, so
            // it can be hit without aiming (WCAG 2.5.8).
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(HEADER_GAP))
                    // The lanes' header column, empty here: the ruler's own bar
                    // has to start where their beds start, or the playhead
                    // would point at a different moment in each row.
                    .child(div().flex_none().w(px(HEADER_W)))
                    .child(
                        div()
                            .id("ruler")
                            .flex_1()
                            .min_w(px(0.))
                            .h(px(RULER_HIT_H))
                            .flex()
                            .flex_col()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(HOVER_DIM)))
                            // The strip carries no text, so the tooltip is the only
                            // place it can say what it is.
                            .tooltip(|_, cx| {
                                cx.new(|_| Tip("Seek — click or drag; ctrl+wheel zooms".into()))
                                    .into()
                            })
                            // Ctrl+wheel is what every timeline zooms with, and
                            // the point held still is the one under the pointer
                            // rather than the playhead. Only with ctrl: a bare
                            // wheel here is the window's to scroll, and the
                            // controls row above scrolls on exactly that.
                            .on_scroll_wheel(cx.listener(
                                |this, event: &ScrollWheelEvent, _, cx| {
                                    if !event.modifiers.control {
                                        return;
                                    }
                                    let dy = match event.delta {
                                        ScrollDelta::Lines(d) => d.y,
                                        ScrollDelta::Pixels(d) => f32::from(d.y),
                                    };
                                    if dy == 0. {
                                        return;
                                    }
                                    let anchor = px_along(event.position.x, this.ruler.get());
                                    let factor = if dy > 0. { ZOOM_STEP } else { 1. / ZOOM_STEP };
                                    this.zoom(factor, Some(anchor), cx);
                                },
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    this.scrubbing = true;
                                    this.scrub_to(event.position.x, true, cx);
                                }),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(6.))
                                    .rounded(px(3.))
                                    .bg(rgb(SURFACE))
                                    .child(bounds_probe(self.ruler.clone()))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(px(filled))
                                            .rounded(px(3.))
                                            .bg(rgb(ACCENT)),
                                    ),
                            ),
                    ),
            )
            // Every lane the project has, in its own order -- and its own
            // column, so a project with more lanes than the panel is tall
            // scrolls its tracks instead of pushing the picture off the window.
            // The gap is the panel's own, so two lanes lay out exactly as they
            // did when they were two children of it.
            .child(
                div()
                    .id("lanes")
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .max_h(px(lanes_h(LANES_MAX)))
                    .overflow_y_scroll()
                    .children(rows),
            )
            // Under the tracks and outside their scrolling column: a subtitle is
            // not a track -- nothing can be dropped on it and nothing dragged
            // along it -- and a strip that scrolled away with the sixth lane
            // would be a strip nobody could see while editing.
            .children(strip)
    }

    /// The running import holds its own bar above the notice's, for the notice
    /// bar's reason: the message is the point. Not a notice, though -- nothing
    /// dismisses it, because it is about work still going on, and it leaves by
    /// itself when the file lands.
    ///
    /// The bar under the words is a *sweep*, not a fill: neither read reports
    /// where in the file it is, so a fill would have to invent the one number
    /// this cannot know. What it does say truthfully is "something is still
    /// running", which is exactly the question a frozen-looking window raises.
    fn import_bar(&self) -> Option<impl IntoElement> {
        let import = self.importing.as_ref()?;
        let elapsed = import.started.elapsed().as_secs_f32();
        let line = import_line(
            &file_name(&import.path),
            import.seen,
            elapsed,
            import.since.elapsed().as_secs_f32(),
            self.imports.len(),
            arrival(self.opening.as_deref(), &import.path) != Landing::Import,
        );
        // A quarter of the bar, crossing it every three seconds and wrapping.
        // Tied to the elapsed clock, so it moves for as long as the read does
        // and stops dead the instant the file lands.
        const SWEEP: f32 = 0.25;
        let at = (elapsed / 3.).fract() * (1. + SWEEP) - SWEEP;
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(12.))
                .py(px(6.))
                .bg(rgb(SURFACE))
                .child(div().flex_1().min_w(px(0.)).child(line))
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(2.))
                        .bg(rgb(HOVER))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left(relative(at.max(0.)))
                                // Clipped at both ends by hand: the segment
                                // enters from the left edge and leaves past the
                                // right one, and a width that overhung would
                                // paint outside the track.
                                .w(relative((at + SWEEP).min(1.) - at.max(0.)))
                                .h(px(2.))
                                .bg(rgb(ACCENT)),
                        ),
                ),
        )
    }

    /// The line a long-standing seek shows, in the import bar's place and by the
    /// import bar's rules: nothing dismisses it, and it leaves by itself the
    /// moment the frame lands. No sweep under it -- an open reports nothing about
    /// where it has got to, and the clock is the honest half of that bar anyway.
    fn seek_bar(&self) -> Option<impl IntoElement> {
        let line = seek_line(self.seek_since.map(|t| t.elapsed()))?;
        Some(
            div()
                .flex_none()
                .px(px(12.))
                .py(px(6.))
                .bg(rgb(SURFACE))
                .child(line),
        )
    }

    /// A notice holds its own bar, full width, until it is answered: any key
    /// retires it (the key handler) and so does a click on it. Its own surface
    /// because the message is the point -- a failure cut to the timecode's slot
    /// is a failure nobody read.
    ///
    /// The export's own line is more than a message: it names a file that is
    /// now on disk, so the same click that retires it shows that file in the
    /// desktop's file manager.
    fn notice_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let notice = self.notice.clone()?;
        // The path travels with the text it was written for: a later notice
        // holds the bar and the click is a plain dismissal again.
        let exported = self
            .exported
            .clone()
            .filter(|_| notice.starts_with(EXPORT_DONE));
        let hint = match exported {
            Some(_) => "click — open file location",
            None => "click or press any key to dismiss",
        };
        Some(
            div()
                .id("notice")
                .when(exported.is_some(), |d| {
                    d.tooltip(|_, cx| {
                        cx.new(|_| {
                            Tip("Open file location — shows the export in the file manager".into())
                        })
                        .into()
                    })
                })
                .flex_none()
                .flex()
                .items_start()
                .gap(px(12.))
                .px(px(12.))
                .py(px(6.))
                .bg(rgb(SURFACE))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    // Another process starting: off the UI thread, and it
                    // outlives the notice it was asked from.
                    if let Some(path) = exported.clone() {
                        cx.background_executor()
                            .spawn(async move { show_in_file_manager(&path) })
                            .detach();
                    }
                    this.notice = None;
                    cx.notify();
                }))
                .child(div().flex_1().min_w(px(0.)).child(notice))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(rgb(INK_DIM))
                        .child(hint),
                ),
        )
    }

    /// Every binding, and the way to change one: a row per entry, click it and
    /// the next stroke becomes its chord. Over the whole window, because while
    /// it is up nothing under it may answer -- the scrim stops the clicks
    /// (gpui dispatches mouse listeners topmost-first, window.rs:3705) and the
    /// key handler's own branch stops the strokes.
    ///
    /// Plain divs, like every control here: nothing in it takes focus, so the
    /// root is still the one reading the keyboard -- which is exactly what a
    /// waiting row depends on.
    fn keys_overlay(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.keys_open {
            return None;
        }
        // The way out, spelled the way the rows spell a chord.
        let out = keymap::Chord {
            key: ESCAPE.to_string(),
            ctrl: false,
        }
        .pretty();
        // Every action there is, under its heading, and the strokes the modal
        // cards answer to beside them -- `keys_rows` is the whole of the order
        // and every word in it comes off the registry.
        //
        // A row is two targets, not one: its label *does* the action, which is
        // the pointer's way to the ones no button carries, and its stroke
        // changes that stroke. An action with two strokes reads as one line
        // ("x or delete") and a rebind replaces that whole set.
        let mut rows: Vec<AnyElement> = Vec::new();
        let row = || {
            div()
                .flex()
                // The floor, not the height: a row that needed two lines
                // would otherwise paint over the one under it -- which it did
                // anyway until this said `flex_none`, a row inside a capped
                // scrolling list being shrunk to fit by default (the export
                // card's rows had the same shape and the same overlap).
                .flex_none()
                .min_h(px(KEYS_ROW_H))
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(6.))
                .rounded(px(3.))
        };
        let found = keys_filter(&self.keys_search, &self.keymap);
        // What the search box says: the typed text, and how much of the list is
        // left under it -- a filter that found nothing has to say so, or the
        // card reads as a list that lost its rows.
        let search = if self.keys_search.is_empty() {
            "search: type to filter · ↑ ↓ scroll the list · a click away closes".to_string()
        } else {
            let rows = found
                .iter()
                .filter(|(_, r)| !matches!(r, KeyRow::Head(_)))
                .count();
            match rows {
                0 => format!(
                    "search: {} — nothing matches · {out} clears",
                    self.keys_search
                ),
                n => format!("search: {} — {n} shown · {out} clears", self.keys_search),
            }
        };
        for (i, key_row) in found {
            rows.push(match key_row {
                KeyRow::Head(category) => div()
                    .flex_none()
                    .px(px(6.))
                    .pt(px(4.))
                    .text_size(px(11.))
                    .text_color(rgb(INK_DIM))
                    .child(category.label())
                    .into_any_element(),
                KeyRow::Act(action) => {
                    let capturing = self.rebinding == Some(action);
                    // Why the label half will not answer, if it will not: the
                    // registry's one answer, the same the clip menu dims by.
                    let refusal = self.enable(action, None);
                    let out = out.clone();
                    row()
                        .when(capturing, |d| d.bg(rgb(SELECTED)))
                        .child(
                            div()
                                .id(("do", i))
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .min_h(px(KEYS_ROW_H))
                                .items_center()
                                // One line, cut where the stroke's column
                                // starts: a label longer than the room it has
                                // printed straight over the stroke beside it,
                                // and two overprinted words are less readable
                                // than one truncated one.
                                .truncate()
                                .child(action.label())
                                // The reason rides on the label rather than in
                                // the stroke column, which the rebind half
                                // needs whatever the editor's state is: an
                                // action nobody can ask for right now is still
                                // one whose key may be changed.
                                .when_some(refusal.why(), |d, why| {
                                    let why: SharedString = why.into();
                                    d.tooltip(move |_, cx| cx.new(|_| Tip(why.clone())).into())
                                })
                                .when(!refusal.yes(), |d| d.opacity(0.4).cursor_not_allowed())
                                .when(refusal.yes(), |d| {
                                    d.cursor_pointer().hover(|s| s.bg(rgb(HOVER))).on_click(
                                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            // The card goes first: several of
                                            // these open a card of their own,
                                            // and every edit moves the indices
                                            // the menus are holding.
                                            this.keys_open = false;
                                            this.rebinding = None;
                                            this.act(action, cx);
                                            cx.notify();
                                        }),
                                    )
                                }),
                        )
                        .child(
                            // ponytail: the column is as wide as the stroke it
                            // prints, so a one-character chord gives this half a
                            // hit area under the 24px WCAG 2.5.8 floor -- tall
                            // enough, narrow. Upgrade: a min_w of HIT_MIN here.
                            div()
                                .id(("bind", i))
                                .flex_none()
                                .flex()
                                .min_h(px(KEYS_ROW_H))
                                .items_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(HOVER)))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.rebinding = Some(action);
                                    cx.notify();
                                }))
                                .child(if capturing {
                                    div()
                                        .text_color(rgb(INK_DIM))
                                        .child(format!("press a key — {out} cancels"))
                                } else {
                                    div().child(self.keymap.display(action))
                                }),
                        )
                        .into_any_element()
                }
                KeyRow::Fixed(f) => {
                    let f = &keymap::FIXED[f];
                    row()
                        .child(f.label)
                        // Dim, and no hover: this one is not a row you can click.
                        .child(div().text_color(rgb(INK_DIM)).child(f.chord.clone()))
                        .into_any_element()
                }
            });
        }
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                // The picture behind is out of reach, and looks it. The press is
                // swallowed here so a button under the scrim cannot take it --
                // and it closes the card on the way, which is the pointer's exit
                // from every card here ([`Player::close_card`]).
                .bg(rgba(0x101010cc))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w(px(KEYS_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        // Title and instruction are two children, never one
                        // wrapping line: a fixed-height slot whose text wrapped
                        // painted its second line over the first row.
                        .child(div().flex_none().px(px(6.)).child("Actions & keys"))
                        // One status line, under the title where the eye starts.
                        // A refusal takes it over -- it is the more urgent of the
                        // two, and the notice bar it would otherwise appear in is
                        // under the scrim.
                        //
                        // The row list below is capped and scrolls, so neither a
                        // wrapped refusal here nor another action added to `ALL`
                        // can push the card past a 360 px window any more.
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(
                                    self.notice
                                        .clone()
                                        .unwrap_or_else(|| {
                                            "click an action to do it · click its key to change it"
                                                .into()
                                        }),
                                ),
                        )
                        // The search box: no focus and no text field -- the
                        // card's key handler is the field, exactly as the
                        // export card's bitrate is typed into it. Its own line
                        // above the list, so the rows below it are always the
                        // answer to what it says.
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(search),
                        )
                        // Capped and scrolling rather than as tall as the action
                        // list happens to be: the list grows with the editor,
                        // the smallest window does not.
                        .child(
                            div()
                                .id("keys-rows")
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .max_h(px(KEYS_ROWS_H))
                                // The line the list scrolls under: a row half
                                // out of the viewport sat against the search
                                // line with nothing between them, and read as a
                                // row painted over the heading rather than as a
                                // list with more above it.
                                .mt(px(4.))
                                .border_t_1()
                                .border_color(rgb(CHROME))
                                .pt(px(4.))
                                .overflow_y_scroll()
                                // The wheel's offset and the arrow keys' are
                                // the same one, so the two cannot disagree
                                // about where the list is.
                                .track_scroll(&self.keys_scroll)
                                .children(rows),
                        ),
                ),
        )
    }

    /// What an export is going to be, before there is one: the codec, the box
    /// it goes into, the bitrate, where it lands -- and, above the button, the
    /// two lines that state the file all of that adds up to. The same scrim and
    /// row shape as the keybindings overlay (two cards of different builds over
    /// one window read as two different programs) and the same plain divs, so
    /// the root keeps the keyboard and the custom row's field is typed into
    /// through it ([`NumberEdit`]).
    ///
    /// Every row carries the key that picks it, so the card is drivable end to
    /// end without a pointer *and* without a legend to memorise -- and every
    /// row is clickable, the bitrate's steppers included, so it is drivable end
    /// to end without a keyboard as well.
    ///
    /// Two shapes of it, behind `g` and `r`: sections against one flat list,
    /// and the codecs with no encoder collapsed into a footer against a dimmed
    /// row each. Grouped and collapsed is what opens.
    fn export_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.export_open {
            return None;
        }
        // The cap is what keeps the card inside the smallest window; a taller
        // one gets a taller list rather than a scroll past empty space, and the
        // card is still only as tall as the rows it has.
        let rows_h = (f32::from(viewport.height) - EXPORT_FIXED_H - 24.).max(EXPORT_ROWS_H);
        let row = |id: (&'static str, usize)| {
            div()
                .id(id)
                .flex()
                // The floor, not the height: the destination's path wraps on a
                // long name and must not paint over the row under it. Which it
                // did: inside a capped, scrolling list a row shrinks to fit by
                // default, so a wrapped detail was drawn over the row beneath
                // it (the Sound row's "the source's own packets are copied…"
                // over Subtitles). The list scrolls -- a row is as tall as what
                // it has to say.
                .flex_none()
                .min_h(px(KEYS_ROW_H))
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(6.))
                .rounded(px(3.))
        };
        // A row that cannot be picked is dimmed and takes no click, exactly as
        // an inapplicable item in the clip menu is: it still says its piece.
        let live = |d: Stateful<Div>, enabled: bool| {
            d.when(!enabled, |d| {
                d.cursor_not_allowed().text_color(rgb(INK_DIM))
            })
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(rgb(HOVER))))
        };
        // A row as this card writes them: the mark saying which one is picked,
        // the key that picks it, its name, and what the choice means. The mark
        // is a glyph and not only a colour -- a background alone is gone the
        // moment a hover lands on the row, and invisible to anyone who cannot
        // tell the two greys apart (WCAG 1.4.1).
        let entry = |id: (&'static str, usize),
                     key: &str,
                     label: SharedString,
                     detail: SharedString,
                     picked: bool,
                     enabled: bool| {
            // The small print of a picked row sits on the highlight, where the
            // dim ink is only 3.3:1 -- the row it lands on lifts it (WCAG
            // 1.4.3, and the fit test pins both numbers).
            let ink = match picked {
                true => INK,
                false => INK_DIM,
            };
            live(row(id), enabled)
                .when(picked, |d| d.bg(rgb(SELECTED)))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.))
                        .child(div().w(px(10.)).child(match picked {
                            true => "✓",
                            false => " ",
                        }))
                        .child(
                            div()
                                .w(px(EXPORT_KEY_W))
                                .text_size(px(11.))
                                .text_color(rgb(ink))
                                .child(SharedString::from(key.to_string())),
                        )
                        .child(label),
                )
                // Wraps rather than runs off the row: a refusal is the longest
                // thing in this column and the half of it past the edge is the
                // half that says what to do instead.
                .child(
                    div()
                        .min_w(px(0.))
                        .flex_shrink()
                        .text_size(px(11.))
                        .text_color(rgb(ink))
                        .child(detail),
                )
        };
        let header = |text: &'static str| {
            div()
                .flex_none()
                .px(px(6.))
                .pt(px(4.))
                .text_size(px(10.))
                .text_color(rgb(INK_DIM))
                .child(text)
                .into_any_element()
        };
        let mut list: Vec<AnyElement> = Vec::new();
        if self.export_grouped {
            list.push(header("FORMAT"));
        }
        // The codecs first: which one is being written decides whether every
        // row under it means anything. One a *this* timeline cannot be written
        // as (an audio-only edit, for a picture codec) reads exactly like one
        // there is no encoder for -- dimmed, unclickable, and carrying its own
        // reason where its detail was, with nothing to click to find that out.
        let mut refusals: Vec<String> = Vec::new();
        for (i, (boxes, key, label, detail)) in FORMATS.into_iter().enumerate() {
            let format = same_box(boxes, self.format);
            let refused = format
                .zip(self.session.as_ref())
                .and_then(|(f, s)| format_refusal(s, f));
            // A codec with no encoder here at all is the other kind of refusal,
            // and the one the footer collects: it can never become pickable, so
            // a row each is dead rows above the fold.
            if format.is_none() {
                refusals.push(format!("{label} — {detail}"));
                if !self.export_refusals_inline {
                    continue;
                }
            }
            let detail: SharedString = match &refused {
                Some(why) => why.clone().into(),
                None => detail.into(),
            };
            let format = format.filter(|_| refused.is_none());
            let mut r = entry(
                ("format", i),
                key,
                label.into(),
                detail,
                boxes.contains(&self.format),
                format.is_some(),
            );
            if let Some(format) = format {
                r = r.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_format(format);
                    cx.notify();
                }));
            }
            list.push(r.into_any_element());
        }
        if self.export_grouped {
            list.push(header("DETAILS"));
        }
        // The container, and only where the picked codec has more than one box:
        // a row offering a single choice reads as a choice.
        let boxes = containers(self.format);
        if boxes.len() > 1 {
            let next = next_container(self.format);
            list.push(
                entry(
                    ("container", 0),
                    "c",
                    "Container".into(),
                    format!(
                        "{} — c for {}",
                        self.format.ext().to_uppercase(),
                        next.ext().to_uppercase()
                    )
                    .into(),
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.cycle_container();
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        match bitrate_refusal(self.format) {
            // One dimmed row carrying the reason, rather than five rows of a
            // figure that will not be written: the quality rows are the
            // *picture's* bitrate, and this file has no picture in it.
            Some(why) => list.push(
                entry(
                    ("quality", 0),
                    "q",
                    "Quality".into(),
                    why.into(),
                    false,
                    false,
                )
                .into_any_element(),
            ),
            None => {
                for (i, quality) in Quality::ALL.into_iter().enumerate() {
                    // The custom row is a field: `n` opens it, a click in it
                    // opens it too, and while it is open the row shows what is
                    // being typed with a caret in it rather than the number in
                    // force. The other four are picked whole.
                    let field = self.mbps_edit.as_ref().filter(|_| quality == Quality::Custom);
                    let mut r = entry(
                        ("quality", i),
                        match quality {
                            Quality::Custom => "n",
                            _ => "q",
                        },
                        quality.label().into(),
                        match field {
                            Some(edit) => edit.detail().into(),
                            None => quality.detail(self.custom_mbps).into(),
                        },
                        self.quality == quality,
                        true,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            match quality {
                                Quality::Custom => this.edit_mbps(),
                                _ => this.quality = quality,
                            }
                            cx.notify();
                        },
                    ));
                    if quality == Quality::Custom {
                        r = r.child(self.mbps_steppers(cx));
                    }
                    list.push(r.into_any_element());
                }
            }
        }
        // The other half of the file, and the one this card used to write at a
        // fixed rate without saying so: what the *sound* is coded at, for every
        // format that codes it -- the AAC inside a video export as much as an
        // MP3. Four rates, so the pointer gets the *list* of them
        // ([`Pick::AudioRate`]) rather than a button clicked round -- the
        // resolution row's rule, and the key still steps as `ctrl+r` still
        // does. Dimmed with the reason where this timeline has no rate to pick,
        // like the quality rows are.
        let sound = self.audio_rate_refusal();
        let mut r = entry(
            ("sound", 0),
            "b",
            "Sound".into(),
            match sound {
                Some(why) => why.into(),
                None => format!("{} kbps — b steps", self.audio_kbps).into(),
            },
            false,
            sound.is_none(),
        );
        if sound.is_none() {
            r = r.on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.open_picker(Pick::AudioRate, event.position(), cx)
            }));
        }
        list.push(r.into_any_element());
        // What happens to the subtitles on this timeline, in one line: how many
        // are written into the file and under what names, or the reason each one
        // is not. Which tracks travel is not a pick -- everything with a cue on
        // the timeline goes ([`Player::export_subs`]) -- so the row says rather
        // than offers, like the machine lines below.
        // ...and the ones that do not travel are named here too, which they were
        // not: `export_subs` had already filtered a track with no cue on *this*
        // timeline out of the engine's sight, so the card said nothing about a
        // row the list was still showing ([`subtitle_plan`]).
        let plan = self.subtitle_line();
        // The row used to end in "click for <fmt> in MKV" whenever the picture
        // was going into an mp4, on the grounds that only Matroska carries a
        // text track. An mp4 carries one now (`Mp4Muxer::write_subtitles` writes
        // `tx3g`), so that was a refusal offering a way out of nothing: the
        // container the card is already set to embeds them. The refusals left in
        // `planned_subtitles` are the true ones -- a sound-only format has
        // nowhere to put a track, and a PGS track is pictures whatever the box.
        list.push(
            entry(
                ("subtitles", 0),
                "",
                "Subtitles".into(),
                plan.into(),
                false,
                false,
            )
            .into_any_element(),
        );
        let destination = entry(
            ("destination", 0),
            "d",
            "Destination".into(),
            file_name(&self.export_path).into(),
            false,
            true,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_destination(cx)))
        .into_any_element();
        // The card's own two layout switches. They were `g` and `r` and nothing
        // else, while the status line under the title advertised both of them:
        // a hand on the mouse read what they do and had nothing to press.
        list.push(
            entry(
                ("export-layout", 0),
                "g",
                "Layout".into(),
                match self.export_grouped {
                    true => "sections — g for one flat list".into(),
                    false => "one flat list — g for sections".into(),
                },
                self.export_grouped,
                true,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.export_grouped = !this.export_grouped;
                cx.notify();
            }))
            .into_any_element(),
        );
        list.push(
            entry(
                ("export-refusals", 0),
                "r",
                "Codecs with no encoder".into(),
                match self.export_refusals_inline {
                    true => "a dimmed row each — r collapses them".into(),
                    false => "one line at the foot — r shows a row each".into(),
                },
                self.export_refusals_inline,
                true,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.export_refusals_inline = !this.export_refusals_inline;
                cx.notify();
            }))
            .into_any_element(),
        );
        match self.export_grouped {
            true => list.push(destination),
            // Flat: the same rows with no headers over them, and the
            // destination back at the top where the card used to open with it.
            false => list.insert(0, destination),
        }
        if !self.export_refusals_inline && !refusals.is_empty() {
            // Last, and one line: the reason travels with the name (a footer
            // that only listed them would be the "why not?" the rows exist to
            // answer), but nothing here can ever be picked, so it sits under
            // every row that can rather than above them.
            list.push(
                div()
                    .flex_none()
                    .px(px(6.))
                    .py(px(2.))
                    .text_size(px(11.))
                    .text_color(rgb(INK_DIM))
                    .child(format!("cannot write: {}", refusals.join(" · ")))
                    .into_any_element(),
            );
        }
        // What is doing the work, on this machine and in this build: the GPU
        // half is asked of the driver through the plugin (a different answer on
        // every machine, and "none" where there is no plugin at all), the build
        // half is the crates that were compiled in. Last in the list, because it
        // is what a user checks rather than what they pick -- and a listing
        // rather than rows, since none of it can be clicked.
        if self.export_grouped {
            list.push(header("THIS MACHINE"));
        }
        let note = |text: String| {
            div()
                .flex_none()
                .px(px(6.))
                .py(px(2.))
                .text_size(px(11.))
                .text_color(rgb(INK_DIM))
                .child(text)
                .into_any_element()
        };
        list.push(note(format!(
            "GPU: {}",
            self.hw_caps.clone().unwrap_or_else(|| "asking…".into())
        )));
        list.push(note(format!("Built in: {}", engine::caps::software())));
        // What the rows add up to, which is the one thing that has to be right:
        // codec, box, size, rate, sound, where it goes and about how big. Two
        // lines, outside the scrolling list, so it is on screen whatever the
        // list is scrolled to and whatever is picked.
        let picture = self
            .session
            .as_ref()
            .map(|s| (s.resolution(), s.meta().frame_rate));
        let audio = self
            .session
            .as_ref()
            .map_or("", |s| s.planned_audio(self.format));
        let settings =
            export_settings(self.quality, self.custom_mbps, self.format, self.audio_kbps);
        let size = estimated_bytes(
            settings.bitrate.filter(|_| self.format.has_video()),
            self.session
                .as_ref()
                .map_or(0., PlaybackSession::timeline_duration),
        );
        let head = summary_head(self.format, picture, audio);
        let tail = summary_tail(
            &self.export_path,
            size,
            self.export_seat.as_ref().and_then(|(.., seat)| *seat),
            self.format.has_video(),
        );
        // The button says the refusal *before* it is pressed: the picked codec
        // can go invalid one edit after it was picked (a cleared video lane),
        // and a button that looks ready until it is pressed is the lie the
        // dimmed rows above it are there to avoid.
        let blocked = self
            .session
            .as_ref()
            .and_then(|s| format_refusal(s, self.format));
        let action: SharedString = match &blocked {
            Some(why) => {
                format!("Cannot export — {}", why.split(" — ").next().unwrap_or(why)).into()
            }
            None => "Export".into(),
        };
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(0x101010cc))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w(px(EXPORT_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        .child(div().flex_none().px(px(6.)).child("Export"))
                        // The status line, where a refusal from the save dialog
                        // lands: the notice bar it would otherwise take is under
                        // the scrim. The keys are on the rows, so this says how
                        // the card as a whole is answered rather than listing
                        // them a second time.
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(match (&self.mbps_edit, &self.notice) {
                                    // A field being typed into says so here as
                                    // well as in its row: this line is outside
                                    // the scrolling list and on screen at every
                                    // window size, and at 360 px the custom row
                                    // itself can be below the fold -- a number
                                    // typed where it cannot be seen is the
                                    // blind capture this field replaced.
                                    (Some(edit), _) => {
                                        SharedString::from(format!("Custom bitrate {}", edit.detail()))
                                    }
                                    (None, Some(notice)) => notice.clone(),
                                    (None, None) => "the keys are on the rows · enter exports · \
                                                     a click away or esc closes · g/r change the layout"
                                        .into(),
                                }),
                        )
                        // Capped and scrolling like the keybindings list: the
                        // rows are more than a 360 px window has room for, and
                        // it is the list that scrolls, never the card that
                        // grows -- the summary and the button below stay put.
                        .child(
                            div()
                                .id("export-rows")
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .max_h(px(rows_h))
                                .overflow_y_scroll()
                                .children(list),
                        )
                        .child(div().flex_none().px(px(6.)).text_size(px(11.)).child(head))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(tail),
                        )
                        .child(
                            div()
                                .id("export-confirm")
                                .mt(px(4.))
                                .flex()
                                .h(px(CONTROL_H))
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .bg(rgb(match blocked.is_some() {
                                    true => CHROME,
                                    false => SELECTED,
                                }))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(HOVER)))
                                .on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.start_export(cx)
                                    }),
                                )
                                .child(action),
                        ),
                ),
        )
    }

    /// The custom bitrate's two pointer buttons. The typed digits were the last
    /// control in this card a mouse could not reach at all -- and a number that
    /// can only be typed is a number a hand on the pointer has to leave the
    /// card to change. `HIT_MIN` square, like every other target here.
    fn mbps_steppers(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let step = |id: &'static str, label: &'static str, by: i32, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .w(px(HIT_MIN))
                .h(px(HIT_MIN))
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .bg(rgb(CHROME))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.nudge_mbps(by);
                    cx.notify();
                }))
                .child(label)
        };
        div()
            .flex()
            .gap(px(4.))
            .child(step("mbps-down", "−", -1, cx))
            .child(step("mbps-up", "+", 1, cx))
    }

    /// The equalizer of one audio clip: its frequency response drawn as a
    /// curve, a handle per band sitting on it, each band's own bell under the
    /// sum, and a row that reads and moves the picked band's three numbers. The
    /// curve is the clip's actual filter (`EqParams::response_db` reads the
    /// coefficients the samples go through), and it is redrawn on every pointer
    /// sample of a drag, so the shape bends under the hand.
    ///
    /// Wider than the other cards and wider still on a bigger window
    /// ([`eq_card_w`]): every pixel across is frequency resolution, which none
    /// of the row-shaped cards have any use for. The same scrim and the same
    /// plain divs -- nothing here takes focus, so the root keeps the keyboard
    /// and the card's own strokes (a digit, the arrows, `a`, `x`, `f`, `r`,
    /// `s`) reach it.
    ///
    /// Every change is written at the clip as it is made
    /// ([`Player::commit_eq`]), so what the card shows is always what is
    /// playing: there is no OK button to forget, and closing it changes
    /// nothing. What takes a curve back off is undo, like every other edit.
    fn eq_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.eq_open?;
        // The rate the engine filters this timeline at, so the drawn curve is
        // the one those coefficients make -- near the top of the axis a 44.1 kHz
        // clip and a 48 kHz one are not the same shape.
        let sample_rate = self.timeline_audio().map_or(48_000, |(rate, _)| rate);
        let picked = self.eq_params.bands.get(self.eq_band);
        // What is playing, drawn behind the curve -- and only while something
        // *is* playing: the tap freezes with the device, and a still spectrum
        // under a paused timeline would look like sound that is not there.
        let spectrum = self
            .eq_spectrum
            .then_some(self.session.as_ref())
            .flatten()
            .filter(|session| session.is_playing())
            .and_then(PlaybackSession::audio_tap)
            .map(|(samples, rate)| eq_spectrum(&samples, rate))
            .filter(|levels| !levels.is_empty())
            .map(eq_spectrum_curve);
        let handles: Vec<_> = self
            .eq_params
            .bands
            .iter()
            .enumerate()
            .map(|(i, band)| {
                div()
                    .absolute()
                    .left(relative(eq_x(band.freq_hz)))
                    .top(relative(eq_y(band.gain_db)))
                    // Centred on its own point: it hangs off the graph's corner,
                    // so it is pulled back by half of itself both ways.
                    .ml(px(-EQ_HANDLE / 2.))
                    .mt(px(-EQ_HANDLE / 2.))
                    .w(px(EQ_HANDLE))
                    .h(px(EQ_HANDLE))
                    .rounded(px(EQ_HANDLE / 2.))
                    .bg(rgb(match i == self.eq_band {
                        true => ACCENT,
                        false => INK_DIM,
                    }))
            })
            .collect();
        let graph = div()
            // Ided like the band rows it replaces: what the pointer presses on
            // is one element with its own hitbox, which is what a drag is
            // tracked from.
            .id("eq-graph")
            .relative()
            .flex_none()
            .h(px(EQ_GRAPH_H))
            .rounded(px(3.))
            .bg(rgb(HOVER_DIM))
            .cursor_pointer()
            // The press picks the band under it *and* is already the first
            // sample of the drag, so a plain click sets a value. A second click
            // takes that band back to flat instead: the gesture that undoes one
            // handle, with no modifier to remember.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.eq_band = this.nearest_band(event.position.x);
                    if event.click_count >= 2 {
                        this.eq_dragging = false;
                        this.nudge_band(|b| b.gain_db = 0., cx);
                        return;
                    }
                    this.eq_dragging = true;
                    this.drag_band(event.position, cx);
                }),
            )
            .child(bounds_probe(self.eq_graph.clone()))
            // The analyser first, so everything else is drawn on top of it.
            .children(spectrum)
            // The decades, so a hump can be read as "around 200 Hz" without
            // dropping the eye to the labels along the bottom. The two ends
            // are the box's own edges and rule nothing.
            .children(
                EQ_TICKS
                    .iter()
                    .filter(|(freq, _)| *freq > EQ_FREQ_LOW && *freq < EQ_FREQ_HIGH)
                    .map(|(freq, _)| {
                        div()
                            .absolute()
                            .left(relative(eq_x(*freq)))
                            .w(px(1.))
                            .h_full()
                            .bg(rgb(EQ_GRID))
                    })
                    .collect::<Vec<_>>(),
            )
            // Half way to each limit, each carrying its own number: the curve
            // is a curve of decibels, and until now only one of them was drawn.
            .children(EQ_DB_GRID.map(|db| {
                div()
                    .absolute()
                    .top(relative(eq_y(db)))
                    .w_full()
                    .h(px(1.))
                    .bg(rgb(EQ_GRID))
                    .child(
                        div()
                            .absolute()
                            .left(px(4.))
                            .top(px(-11.))
                            .text_size(px(9.))
                            .text_color(rgb(INK_DIM))
                            .child(format!("{db:+.0}")),
                    )
            }))
            // 0 dB: the line a boost is a boost *from*.
            .child(
                div()
                    .absolute()
                    .top(relative(0.5))
                    .w_full()
                    .h(px(1.))
                    .bg(rgb(HOVER)),
            )
            .child(eq_curve(self.eq_params.clone(), sample_rate))
            .children(handles)
            .children(EQ_TICKS.map(|(freq, label)| {
                div()
                    .absolute()
                    .left(relative(eq_x(freq)))
                    // Pulled back so the mark reads as sitting *at* its
                    // frequency; the two ends then hug the corners.
                    .ml(px(-12.))
                    .bottom(px(1.))
                    .w(px(24.))
                    .text_align(TextAlign::Center)
                    .text_size(px(9.))
                    .text_color(rgb(INK_DIM))
                    .child(label)
            }))
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(4.))
                    .text_size(px(9.))
                    .text_color(rgb(INK_DIM))
                    .child(format!("+{EQ_GAIN_LIMIT:.0} dB")),
            );
        // The bottom of the axis is not named: -12 dB would land in the same
        // corner as the 20 Hz tick, and the two lines above it (+6 and -6)
        // already say what the box is worth per pixel.
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(0x101010cc))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w(px(eq_card_w(f32::from(viewport.width))))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        // Which clip, because the card is modal and the lane it
                        // was opened from is behind a scrim by the time it is up.
                        .child(div().flex_none().px(px(6.)).child(format!(
                            "Equalizer — {} clip {}",
                            lane.label(),
                            idx + 1
                        )))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(self.notice.clone().unwrap_or_else(|| {
                                    "drag a handle, or a digit picks a band — ←→ moves it, ↑↓ its gain, shift+←→ its width; a adds, x removes, f flattens it, r all, s spectrum; a click away or esc closes".into()
                                })),
                        )
                        .child(graph)
                        // Which band the keyboard is holding and every number it
                        // is set to, each with the pair of buttons that moves it:
                        // the curve shows the sum, and a band pushed against one
                        // pulling the other way is not readable off it.
                        .child(self.eq_numbers(picked, cx))
                        .child(
                            div()
                                .mt(px(4.))
                                .flex()
                                .gap(px(4.))
                                .child(
                                    div()
                                        .id("eq-reset")
                                        .flex()
                                        .flex_1()
                                        .h(px(CONTROL_H))
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(3.))
                                        .bg(rgb(SELECTED))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(HOVER)))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            for band in &mut this.eq_params.bands {
                                                band.gain_db = 0.;
                                            }
                                            this.commit_eq(cx);
                                        }))
                                        .child("Flatten all"),
                                )
                                // The two that change how many bands there are.
                                // The engine takes any cascade -- the count was
                                // only ever fixed because this card had no way
                                // to say otherwise.
                                .child(
                                    div()
                                        .id("eq-add")
                                        .flex()
                                        .flex_1()
                                        .h(px(CONTROL_H))
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(3.))
                                        .bg(rgb(CHROME))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(HOVER)))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.add_band(cx)
                                        }))
                                        .child("Add band"),
                                )
                                .child(
                                    div()
                                        .id("eq-remove")
                                        .flex()
                                        .flex_1()
                                        .h(px(CONTROL_H))
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(3.))
                                        .bg(rgb(CHROME))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(HOVER)))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.remove_band(cx)
                                        }))
                                        .child("Remove band"),
                                )
                                // The analyser's switch, next to the one other
                                // button the card has: `s` does the same, and a
                                // toggle only a keystroke can reach is one most
                                // people never find.
                                .child(
                                    div()
                                        .id("eq-spectrum")
                                        .flex()
                                        .flex_1()
                                        .h(px(CONTROL_H))
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(3.))
                                        .bg(rgb(match self.eq_spectrum {
                                            true => SELECTED,
                                            false => HOVER_DIM,
                                        }))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(HOVER)))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.eq_spectrum = !this.eq_spectrum;
                                            cx.notify();
                                        }))
                                        .child(match self.eq_spectrum {
                                            true => "Spectrum on",
                                            false => "Spectrum off",
                                        }),
                                ),
                        ),
                ),
        )
    }

    /// The picked band's three numbers -- where it sits, how far it pushes and
    /// how wide it is -- each beside the pair of buttons that moves it. The
    /// arrows do the same three things, but a value only a key can change is a
    /// value a hand on the pointer cannot reach at all, which is the same reason
    /// [`Player::mbps_steppers`] exists.
    fn eq_numbers(&self, picked: Option<&Band>, cx: &mut Context<Self>) -> impl IntoElement {
        let row = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(6.))
            .text_size(px(11.));
        let Some(band) = picked.copied() else {
            return row.child("no bands — a adds one");
        };
        let step = |id: &'static str,
                    label: &'static str,
                    change: fn(&mut Band),
                    cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .w(px(HIT_MIN))
                .h(px(HIT_MIN))
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .bg(rgb(CHROME))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.nudge_band(change, cx)
                }))
                .child(label)
        };
        let number = |value: String,
                      ids: (&'static str, &'static str),
                      by: (fn(&mut Band), fn(&mut Band)),
                      cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(div().child(value))
                .child(step(ids.0, "−", by.0, cx))
                .child(step(ids.1, "+", by.1, cx))
        };
        row.child(format!(
            "Band {} of {}",
            self.eq_band + 1,
            self.eq_params.bands.len()
        ))
        .child(number(
            band_label(&band),
            ("eq-freq-down", "eq-freq-up"),
            (
                |b| b.freq_hz /= EQ_FREQ_STEP,
                |b| b.freq_hz *= EQ_FREQ_STEP,
            ),
            cx,
        ))
        .child(number(
            format!("{:+.1} dB", band.gain_db),
            ("eq-gain-down", "eq-gain-up"),
            (|b| b.gain_db -= EQ_STEP, |b| b.gain_db += EQ_STEP),
            cx,
        ))
        // Q is width, so its buttons are labelled by what they *do* to the
        // hump rather than by which way the number goes: a wider band is a
        // smaller Q, and nobody should have to know that to use the card.
        .child(number(
            format!("Q {:.2}", band.q),
            ("eq-q-wider", "eq-q-narrower"),
            (|b| b.q /= EQ_Q_STEP, |b| b.q *= EQ_Q_STEP),
            cx,
        ))
        .child(
            div()
                .id("eq-flat-band")
                .flex()
                .h(px(HIT_MIN))
                .px(px(8.))
                .items_center()
                .rounded(px(3.))
                .bg(rgb(CHROME))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
                .on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.nudge_band(|b| b.gain_db = 0., cx)
                    }),
                )
                .child("Flatten this"),
        )
    }

    /// The colour card: the graded frame's histogram over a row per control,
    /// each row a bar the pointer drags straight to a value -- no stepper
    /// buttons, because a slider is a thing to pull, and the arrow keys still
    /// move the same value for anyone not using a pointer. Same scrim, surface
    /// and row shape as the other two cards, and the same plain divs, so the
    /// root keeps the keyboard.
    ///
    /// The values are read from the project every render: what is drawn is what
    /// the decoder is grading with, never a copy that could drift from it. The
    /// graph above them is counted off the frame that came *back* through that
    /// grade ([`histogram`]), so pulling exposure tilts it while the hand moves.
    fn color_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (lane, idx) = self.color_open?;
        let params = self.color_params();
        let rows: Vec<_> = COLOR_BANDS
            .iter()
            .enumerate()
            .map(|(i, &(label, low, high))| {
                let mut read = params;
                let value = *band_mut(&mut read, i);
                let frac = ((value - low) / (high - low)).clamp(0., 1.);
                let picked = i == self.color_band;
                div()
                    .id(("color-row", i))
                    .flex()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .gap(px(8.))
                    .px(px(6.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .when(picked, |d| d.bg(rgb(SELECTED)))
                    .hover(|s| s.bg(rgb(HOVER)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.color_band = i;
                        cx.notify();
                    }))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(label))
                    .child(
                        // The bar is 4 px to look at and a whole row to hit
                        // (WCAG 2.5.8), the same split the ruler makes between
                        // what is drawn and what is grabbed. The press is
                        // already the first sample of the drag, so a plain click
                        // sets the value it landed on.
                        div()
                            .id(("color-bar", i))
                            .relative()
                            .w(px(COLOR_BAR_W))
                            .h(px(KEYS_ROW_H))
                            .flex()
                            .items_center()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.color_band = i;
                                    this.color_dragging = true;
                                    this.drag_color(event.position.x, true, cx);
                                }),
                            )
                            .child(bounds_probe(self.color_bars[i].clone()))
                            .child(
                                div()
                                    .w_full()
                                    .h(px(4.))
                                    .rounded(px(2.))
                                    .bg(rgb(CHROME))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(frac))
                                            .rounded(px(2.))
                                            .bg(rgb(ACCENT)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w(px(44.))
                            .text_size(px(11.))
                            .text_color(rgb(INK_DIM))
                            .child(format!("{value:.2}")),
                    )
            })
            .collect();
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(0x101010cc))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w(px(COLOR_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        .child(div().flex_none().px(px(6.)).child(format!(
                            "Colour — {} clip {}",
                            lane.label(),
                            idx + 1
                        )))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(
                                    "drag a bar, or ↑↓ picks one and ←→ moves it, r resets — a click away or esc closes",
                                ),
                        )
                        // The frame as it is being graded, over the controls
                        // grading it: the three lines are what the picture is
                        // made of, and every sample of a drag reseeks, so they
                        // move with the bar under the hand.
                        .child(
                            div()
                                .flex_none()
                                .h(px(HIST_H))
                                .rounded(px(3.))
                                .bg(rgb(HOVER_DIM))
                                .relative()
                                .child(hist_curves(self.histogram)),
                        )
                        .children(rows)
                        .child(
                            div()
                                .id("color-reset")
                                .mt(px(4.))
                                .flex()
                                .h(px(CONTROL_H))
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .bg(rgb(SELECTED))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(HOVER)))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.set_color(ColorParams::default(), cx);
                                }))
                                .child("Reset"),
                        ),
                ),
        )
    }

    /// The speed card: one bar from a quarter speed to four times it, the rates
    /// people name as buttons under it, and the clip's new length in frames --
    /// which is the number a person is actually choosing. Built like the colour
    /// card down to the scrim and the bar's own hit height, because it is the
    /// same kind of card: one continuous value on one clip, live at the clip as
    /// it moves.
    ///
    /// Honest about what it does: the sound is *resampled*, so the pitch goes up
    /// with the rate, which is what the tape in the title means.
    fn speed_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (lane, idx) = self.speed_open?;
        let speed = self.card_speed();
        let session = self.session.as_ref()?;
        let clip = session.lane_clips(lane).get(idx).copied()?;
        let lo = f32::from(Speed::MIN.permille());
        let hi = f32::from(Speed::MAX.permille());
        let frac = ((f32::from(speed.permille()) - lo) / (hi - lo)).clamp(0., 1.);
        let presets: Vec<_> = SPEED_PRESETS
            .into_iter()
            .map(|permille| {
                let at = Speed::from_permille(permille);
                div()
                    .id(("speed-preset", usize::from(permille)))
                    .flex_1()
                    .flex()
                    .h(px(CONTROL_H))
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .bg(rgb(match at == speed {
                        true => SELECTED,
                        false => CHROME,
                    }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(HOVER)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_speed(at, cx);
                    }))
                    .child(format!("{at}"))
            })
            .collect();
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(0x101010cc))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w(px(COLOR_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        .child(div().flex_none().px(px(6.)).child(format!(
                            "Speed (tape) — {} clip {}",
                            lane.label(),
                            idx + 1
                        )))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(
                                    "drag the bar or ←→ moves it, r is 1.00x — the pitch moves with the rate; a click away or esc closes",
                                ),
                        )
                        .child(
                            // 4 px to look at and a whole row to hit (WCAG
                            // 2.5.8), the split the colour sliders and the ruler
                            // both make.
                            div()
                                .id("speed-bar")
                                .relative()
                                .w_full()
                                .h(px(KEYS_ROW_H))
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        this.speed_dragging = true;
                                        this.drag_speed(event.position.x, true, cx);
                                    }),
                                )
                                .child(bounds_probe(self.speed_bar.clone()))
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(4.))
                                        .rounded(px(2.))
                                        .bg(rgb(CHROME))
                                        .child(
                                            div()
                                                .h_full()
                                                .w(relative(frac))
                                                .rounded(px(2.))
                                                .bg(rgb(ACCENT)),
                                        ),
                                ),
                        )
                        .child(div().flex().gap(px(4.)).children(presets))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                // What the choice *is*, in the numbers the
                                // timeline is measured in: the source range
                                // never moves, the room it takes does.
                                .child(format!(
                                    "{speed} — {} source frames over {} on the timeline ({})",
                                    clip.len(),
                                    clip.frames(),
                                    timecode(f64::from(clip.frames()) / self.fps, self.fps)
                                )),
                        ),
                ),
        )
    }

    /// The mix card: one fader per audio track and the master limiter under
    /// them -- the two settings that belong to the *sound of the whole
    /// timeline* rather than to any clip on it.
    ///
    /// A track's fader moves everything on that track by the same amount,
    /// every frequency of it: it is not the equalizer (one take, one band) and
    /// it is not the volume in the panel, which is what this machine monitors
    /// at and is written to no file. The limiter is over the sum of them all,
    /// which is where a mix can pass full scale and where a clamp used to
    /// square it off.
    ///
    /// The silence card's shape, down to the steppers: a row is a label, a
    /// value and the two presses that move it, and the arrows pick a row and
    /// move it too. The rows scroll rather than the card growing past the
    /// window -- a timeline may hold more tracks than a 360 px window has room
    /// for faders.
    fn mix_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.mix_open {
            return None;
        }
        let session = self.session.as_ref();
        let lanes = self.mix_lanes();
        let limiter = session.map_or_else(Limiter::default, PlaybackSession::limiter);
        let mut rows: Vec<(String, String)> = lanes
            .iter()
            .map(|&lane| {
                let db = session.map_or(0., |s| s.lane_gain_db(lane));
                (format!("{} plays at", lane.label()), format!("{db:+.0} dB"))
            })
            .collect();
        // The ceiling in dBFS and the faders in dB, the silence card's rule:
        // a ceiling is a level below full scale, a fader is a change.
        rows.push((
            "Limiter ceiling".into(),
            format!("{:+.0} dBFS", limiter.ceiling_db),
        ));
        rows.push((
            "Limiter".into(),
            match limiter.on {
                true => "on".into(),
                false => "off".into(),
            },
        ));
        let rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(n, (label, value))| {
                div()
                    .id(("mix-row", n))
                    .flex()
                    .flex_none()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .justify_between()
                    .px(px(6.))
                    .rounded(px(3.))
                    .bg(rgb(match n == self.mix_field {
                        true => SELECTED,
                        false => CHROME,
                    }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(HOVER)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.mix_field = n;
                        cx.notify();
                    }))
                    .child(div().text_color(rgb(INK_DIM)).child(label))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(value)
                            .children([-1, 1].map(|steps: i32| {
                                div()
                                    .id(("mix-step", n * 2 + usize::from(steps > 0)))
                                    .flex_none()
                                    .w(px(HIT_MIN))
                                    .h(px(KEYS_ROW_H))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .bg(rgb(CHROME))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(HOVER)))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        // Picked as well as moved, the silence
                                        // card's rule: the row a press lands on
                                        // is the row the arrows carry on from.
                                        this.mix_field = n;
                                        this.nudge_mix(steps, cx);
                                    }))
                                    .child(match steps > 0 {
                                        true => "+",
                                        false => "−",
                                    })
                            })),
                    )
            })
            .collect();
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(0x101010cc))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w(px(COLOR_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .max_h(px(360. - 24.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .child("Mix — track volumes and the master limiter"),
                        )
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(
                                    "− and + move a setting, or ↑↓ picks one and ←→ moves it (hold to run it) — a track fader moves everything on that track; a click away or esc closes",
                                ),
                        )
                        .child(
                            div()
                                .id("mix-rows")
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .overflow_y_scroll()
                                .children(rows),
                        )
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                // What the choice *is*: the limiter's own line,
                                // because "on" alone says nothing about what it
                                // does to a mix that never reaches the ceiling.
                                .child(match limiter.on {
                                    true => format!(
                                        "the mix is held under {:+.0} dBFS — quieter passages are untouched",
                                        limiter.ceiling_db
                                    ),
                                    false => "the limiter is out of circuit — a hot mix clips at full scale".to_string(),
                                }),
                        ),
                ),
        )
    }

    /// The silence card: what the scan is looking for, what it found, and the
    /// two things that can be done about it.
    ///
    /// Its scrim is the lightest of the cards' on purpose. Every other card is
    /// about the clip it names and can black the timeline out; this one is
    /// *about* the timeline -- the marks under it are the whole preview -- so
    /// the bed stays readable and the card sits up in the picture area rather
    /// than over the lanes.
    fn silence_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (lane, idx) = self.silence_open?;
        let cfg = self.silence;
        // The unit is a label, never a conversion: the threshold is a level
        // below full scale whichever of the two the row says (`silence_dbfs`).
        let unit = match self.silence_dbfs {
            true => "dBFS",
            false => "dB",
        };
        let rows = [
            ("Apply to", self.silence_scope.label(&self.silence_lanes())),
            (
                "Silence is under",
                format!("{:.0} {unit}", cfg.threshold_db),
            ),
            ("Level read in", format!("{unit} (0 = full scale)")),
            (
                "Forgive quiet shorter than",
                format!("{:.2} s", cfg.min_silence),
            ),
            (
                "Keep either side of speech",
                format!("{:.2} s", cfg.padding),
            ),
            (
                "Swallow kept slivers under",
                format!("{:.2} s", cfg.min_keep),
            ),
            ("Speed-up plays at", format!("{}", self.silence_factor)),
        ];
        let found = self.silence_marks.len();
        let secs =
            f64::from(self.silence_marks.iter().map(|&(_, len)| len).sum::<u32>()) / self.fps;
        // The line under the rows: where a scan still running has got to -- a
        // card is up from the frame it is asked for, numbers or no numbers --
        // or what the settings found in levels already read.
        let status = match &self.silence_scan {
            Some(scan) => silence_line(
                scan.seen as f32 / 10.,
                scan.progress.total.load(std::sync::atomic::Ordering::Relaxed) as f32 / 10.,
                scan.started.elapsed().as_secs_f32(),
                scan.since.elapsed().as_secs_f32(),
            ),
            None => match found {
                0 => "nothing quiet enough for long enough".to_string(),
                1 => format!("1 silence, {}", secs_label(secs)),
                n => format!("{n} silences, {}", secs_label(secs)),
            },
        };
        let rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(n, (label, value))| {
                div()
                    .id(("silence-row", n))
                    .flex()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .justify_between()
                    .px(px(6.))
                    .rounded(px(3.))
                    .bg(rgb(match n == self.silence_field {
                        true => SELECTED,
                        false => CHROME,
                    }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(HOVER)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.silence_field = n;
                        cx.notify();
                    }))
                    .child(div().text_color(rgb(INK_DIM)).child(label))
                    // The value and the two steps that move it. Every other
                    // card has something to drag or press; this one had the
                    // arrow keys and nothing else, so a row was a setting a
                    // pointer could pick but never change. One press each, the
                    // same call the arrows make -- the hold-to-run is the
                    // keyboard's own and is not a thing a button has.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(value)
                            .children([-1, 1].map(|steps: i32| {
                                div()
                                    .id(("silence-step", n * 2 + usize::from(steps > 0)))
                                    .flex_none()
                                    .w(px(HIT_MIN))
                                    .h(px(KEYS_ROW_H))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .bg(rgb(CHROME))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(HOVER)))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        // Picked as well as moved: the row a
                                        // press lands on is the row the arrows
                                        // carry on from.
                                        this.silence_field = n;
                                        this.nudge_silence(steps);
                                        cx.notify();
                                    }))
                                    .child(match steps > 0 {
                                        true => "+",
                                        false => "−",
                                    })
                            })),
                    )
            })
            .collect();
        // The two buttons the ask names, side by side: a mode toggle would hide
        // one of them behind the other, and there are only two.
        let button = |n: usize, text: String, act: fn(&mut Self, &mut Context<Self>)| {
            div()
                .id(("silence-apply", n))
                .flex_1()
                .flex()
                .min_h(px(KEYS_ROW_H))
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .bg(rgb(match found {
                    0 => CHROME,
                    _ => SELECTED,
                }))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| act(this, cx)))
                .child(text)
        };
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(HEADER_H + 8.))
                // Light enough to read the lanes and the marks on them through:
                // the preview is the point of this card.
                .bg(rgba(0x10101055))
                // Click away closes it, as on every card here -- and the marks
                // go with it, which is what makes this one a call and not a flag.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .w(px(COLOR_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        .child(div().flex_none().px(px(6.)).child(format!(
                            "Silences — {} clip {}",
                            lane.label(),
                            idx + 1
                        )))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(
                                    "− and + move a setting, or ↑↓ picks one and ←→ moves it (hold to run it) — the marks on the lane are what would go; a click away or esc closes",
                                ),
                        )
                        .children(rows)
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_color(rgb(match (&self.silence_scan, found) {
                                    (None, 1..) => ACCENT,
                                    _ => INK_DIM,
                                }))
                                .child(status),
                        )
                        .child(div().flex().gap(px(4.)).children([
                            button(0, "Cut them out (enter)".into(), Self::cut_silences),
                            button(
                                1,
                                format!("Play them at {} (f)", self.silence_factor),
                                Self::speed_silences,
                            ),
                        ])),
                ),
        )
    }

    /// The menu a right-click on a library row opens: what can be done with the
    /// *file* rather than with a clip of it, and a turn-over side saying what
    /// that file is. Built like [`Player::context_card`] down to the scrim, the
    /// row height and the clamp, because it is the same menu on the other panel
    /// -- a click away or any stroke closes it.
    fn library_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let menu = self.library_menu.clone()?;
        let path = menu.path.clone();
        let row = |n: usize| {
            div()
                .id(("library-menu", n))
                .flex()
                .min_h(px(MENU_ROW_H))
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(6.))
                .rounded(px(3.))
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        // What every item of this menu is answered from, read once.
        let ctx = self.row_ctx(&path, menu.stream);
        if menu.details {
            // What the library knows about this row and nothing probed for the
            // card: the streams table is filled once per file at import.
            let info = self
                .streams
                .get(&path)
                .and_then(|of_file| of_file.iter().find(|s| s.index == menu.stream));
            let frames = self
                .session
                .as_ref()
                .map_or(0, |session| session.file_frames(&path));
            // How many clips play from this exact row -- the number that
            // decides whether Remove is refused, so the card answers the
            // question the refusal would otherwise raise.
            let placed = ctx.placed;
            // A still is described by what it has -- a picture, a size, and a
            // longest it may be held for -- where a media file is described by
            // its streams and its length. Same card, the rows that mean
            // something for this kind of source.
            let image = engine::is_image(&path);
            let kind = match self.sizes.get(&path).copied().flatten() {
                Some((w, h)) => format!("still image · {w}x{h}"),
                None => "still image".to_string(),
            };
            for (label, value) in [
                ("File", file_name(&path)),
                ("Path", path.display().to_string()),
                match image {
                    true => ("Picture", kind),
                    false => (
                        "Audio",
                        info.map_or_else(|| "no track of its own".to_string(), stream_detail),
                    ),
                },
                (
                    "Bitrate",
                    bitrate_detail(
                        self.bitrates.get(&path).copied().flatten(),
                        self.streams.get(&path).map_or(0, Vec::len),
                    ),
                ),
                match image {
                    true => (
                        "Longest hold",
                        timecode(f64::from(frames) / self.fps, self.fps),
                    ),
                    false => ("Length", timecode(f64::from(frames) / self.fps, self.fps)),
                },
                ("On the timeline", format!("{placed} clips")),
            ] {
                rows.push(
                    row(rows.len())
                        .child(label)
                        .child(
                            div()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(value),
                        )
                        .into_any_element(),
                );
            }
        } else {
            // The oracle's list, exactly as the clip menu takes its rows from
            // `menu_items`: an item that means nothing for the file that was
            // right-clicked is not a row, and one this moment refuses is drawn
            // dimmed and says why in place of its hint.
            for item in row_items(ctx) {
                let refusal = row_enable(item, ctx);
                let enabled = refusal.yes();
                rows.push(
                    row(rows.len())
                        .child(item.label())
                        .child(
                            div()
                                .min_w(px(0.))
                                .truncate()
                                .text_color(rgb(INK_DIM))
                                .child(refusal.why().unwrap_or_else(|| item.hint())),
                        )
                        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(rgb(HOVER)))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.act_on_row(item, cx);
                                }))
                        })
                        .into_any_element(),
                );
            }
        }
        // Placed by the height it is drawn to, and drawn to what the window has
        // room for -- the clip menu's rule, one function for all three.
        let list_h = menu_rows_h(rows.len(), viewport);
        let (x, y) = menu_at(menu.at, viewport, MENU_PAD * 2. + list_h);
        let full: SharedString = path.display().to_string().into();
        Some(
            scrim()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.library_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.library_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .id("library-menu-card")
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(MENU_W))
                        .flex()
                        .flex_col()
                        .p(px(MENU_PAD))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        // Painted after the scrim, so this listener runs first
                        // and a press meant for an item never closes the menu
                        // out from under its own click (`context_card`).
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(MouseButton::Right, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .when(menu.details, |d| {
                            d.tooltip(move |_, cx| cx.new(|_| Tip(full.clone())).into())
                        })
                        // Scrolls where the window has no room for the list,
                        // like the clip menu's -- an item hanging off the bottom
                        // edge is an item nobody can click.
                        .child(
                            div()
                                .id("library-menu-rows")
                                .flex()
                                .flex_col()
                                .max_h(px(list_h))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                ),
        )
    }

    /// The menu a right-click on a clip opens: what that clip can be given,
    /// each item beside the stroke that does the very same thing, and a
    /// turn-over side that says what the clip *is*. An item that would do
    /// nothing where the playhead is standing is dimmed and takes no click
    /// rather than disappearing, so the menu reads the same every time.
    ///
    /// Every item goes through [`Player::act`], the table the key handler uses:
    /// an item is its stroke, asked for with the mouse. Plain divs like the
    /// rest of this window, so the root keeps the keyboard and escape still
    /// reaches the handler that closes this.
    fn context_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let menu = self.context_menu?;
        let session = self.session.as_ref()?;
        let clip = *session.lane_clips(menu.lane).get(menu.idx)?;
        let source = session.sources().get(clip.source).cloned()?;
        let secs = |frames: u32| timecode(f64::from(frames) / self.fps, self.fps);
        let row = |n: usize| {
            div()
                .id(("menu", n))
                .flex()
                // The floor, not the height: a long label wrapping must not
                // paint over the item under it.
                .min_h(px(MENU_ROW_H))
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(6.))
                .rounded(px(3.))
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        if menu.details {
            // Read-only, so no ids and no hover: this side is a card, not a
            // list of things to click. Each value is one truncated line, which
            // is what keeps the height below the one the clamp was given.
            for (label, value) in [
                ("File", file_name(&source.path)),
                ("Path", source.path.display().to_string()),
                (
                    "Source range",
                    format!("{} – {}", secs(clip.in_frame), secs(clip.out_frame)),
                ),
                // How long it is *where it sits*, which its rate decides -- and
                // the rate itself beside it, because a clip half its source's
                // length could be either a trim or a 2x.
                ("This clip", secs(clip.frames())),
                ("Speed", format!("{} (tape)", clip.speed)),
                ("Source duration", secs(session.file_frames(&source.path))),
                (
                    "Bitrate",
                    bitrate_detail(
                        self.bitrates.get(&source.path).copied().flatten(),
                        self.streams.get(&source.path).map_or(0, Vec::len),
                    ),
                ),
            ] {
                rows.push(
                    row(rows.len())
                        .child(label)
                        .child(
                            div()
                                .min_w(px(0.))
                                .truncate()
                                // A size smaller than the labels: a timecode
                                // pair is 25 characters and has to fit beside
                                // its label inside `MENU_W`.
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(value),
                        )
                        .into_any_element(),
                );
            }
        } else {
            // A grade on a waveform, an equalizer on a picture, a silence scan
            // on a still: things that do not exist for what was right-clicked,
            // so the menu is the list of what this clip can do rather than the
            // registry with most of it struck through. One filter, in
            // `menu_items`, so there is no second answer to keep in step. The
            // state refusals below stay, dimmed and saying why -- the next
            // click of the playhead lights them.
            let ctx = self.ctx(Some((menu.lane, menu.idx)));
            for action in menu_items(ctx) {
                // The registry's own answer, the same one the actions card
                // dims a row with -- and a row that takes no click says *why*
                // rather than printing a stroke that would do nothing.
                let refusal = enable(action, ctx);
                let enabled = refusal.yes();
                // The one item that is not about this clip says so, and says it
                // here rather than in the registry: the stroke is global too,
                // but its row in the keys menu is not sitting on a clip.
                let label = if matches!(action, ActionId::ToggleMute | ActionId::Paste) {
                    format!("{} (global)", action.label())
                } else {
                    action.label().to_string()
                };
                rows.push(
                    row(rows.len())
                        .child(label)
                        .child(match refusal.why() {
                            // One truncated line, like the details side: a
                            // reason that wrapped would make the card taller
                            // than the height `menu_at` placed it by.
                            Some(why) => div()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(why),
                            None => div()
                                .text_color(rgb(INK_DIM))
                                .child(self.keymap.display(action)),
                        })
                        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(rgb(HOVER)))
                                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                    // Closed first: the action moves the very
                                    // indices this menu is holding.
                                    this.context_menu = None;
                                    // The one item that is a *choice* and not a
                                    // doing: four policies, so the pointer gets
                                    // the list of them on this clip rather than
                                    // the next one stepped to behind the click.
                                    // The stroke still steps -- same door.
                                    if action == ActionId::Fit {
                                        this.open_picker(
                                            Pick::Fit(menu.lane, menu.idx),
                                            event.position(),
                                            cx,
                                        );
                                    } else {
                                        this.act(action, cx);
                                    }
                                }))
                        })
                        .into_any_element(),
                );
            }
            rows.push(
                row(rows.len())
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(HOVER)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(menu) = &mut this.context_menu {
                            menu.details = true;
                        }
                        cx.notify();
                    }))
                    .child("Properties")
                    // No stroke reaches this one, and a blank column would read
                    // as one that was forgotten.
                    .child(div().text_color(rgb(INK_DIM)).child("…"))
                    .into_any_element(),
            );
        }
        // The height the card is *placed* by and the height its list is drawn
        // to are one number: placed by a taller one, the card would hang off the
        // window's floor -- the very thing the clamp is for.
        let list_h = menu_rows_h(rows.len(), viewport);
        let (x, y) = menu_at(menu.at, viewport, MENU_PAD * 2. + list_h);
        let full: SharedString = source.path.display().to_string().into();
        Some(
            scrim()
                // Click away closes it, either button, and the press is
                // swallowed so nothing under the menu also takes it. No tint,
                // unlike the modal cards: the timeline this menu is about has
                // to stay readable behind it.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.context_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.context_menu = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        // Only so the details side can carry a tooltip, which
                        // gpui gives to identified elements alone.
                        .id("menu-card")
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(MENU_W))
                        .flex()
                        .flex_col()
                        .p(px(MENU_PAD))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        // Painted after the scrim, so this listener runs first
                        // (gpui bubbles mouse events in reverse, window.rs:3705)
                        // and a press meant for an item never closes the menu
                        // out from under its own click.
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(MouseButton::Right, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        // The one value the details side has to truncate.
                        .when(menu.details, |d| {
                            d.tooltip(move |_, cx| cx.new(|_| Tip(full.clone())).into())
                        })
                        // The list scrolls where the card would otherwise grow
                        // past the window's floor -- an item hanging off the
                        // bottom edge is an item nobody can click.
                        .child(
                            div()
                                .id("menu-rows")
                                .flex()
                                .flex_col()
                                .max_h(px(list_h))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                ),
        )
    }

    /// The open choice list: every value of one enumerated setting at once, the
    /// one in force marked, and a click on any of them picks *that* one -- what
    /// a button stepping one value on per click could never say. Built on the
    /// clip menu's machinery down to the scrim, the placement and the scroll
    /// cap, so it hangs and closes exactly as the menus do and fits the same
    /// 640x360 floor. The stroke for the setting is untouched: this is the
    /// pointer's door to it, not a second setting.
    fn picker_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let picker = self.picker?;
        let rows: Vec<AnyElement> = self
            .choices(picker.of)
            .into_iter()
            .enumerate()
            .map(|(n, (choice, label, detail, picked))| {
                div()
                    .id(("picker-row", n))
                    .flex()
                    // The floor, not the height, and `HIT_MIN` of it: a row of a
                    // list is a click target like every other one here (WCAG
                    // 2.5.8).
                    .min_h(px(MENU_ROW_H))
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(6.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(HOVER)))
                    // The mark is a glyph as well as a highlight, like the
                    // export card's rows: a background alone is gone under a
                    // hover and invisible to anyone who cannot tell the two
                    // greys apart (WCAG 1.4.1).
                    .when(picked, |d| d.bg(rgb(SELECTED)))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.))
                            .child(div().w(px(10.)).child(match picked {
                                true => "✓",
                                false => " ",
                            }))
                            .child(label),
                    )
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(11.))
                            // On the picked row the dim ink sits on the
                            // highlight, where it is only 3.3:1 (WCAG 1.4.3).
                            .text_color(rgb(match picked {
                                true => INK,
                                false => INK_DIM,
                            }))
                            .child(detail),
                    )
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.choose(choice, cx)),
                    )
                    .into_any_element()
            })
            .collect();
        // The window's own room, and the list scrolls only where the window has
        // none -- the clip menu's rule, one function for both.
        let list_h = menu_rows_h(rows.len(), viewport);
        let (x, y) = menu_at(picker.at, viewport, MENU_PAD * 2. + list_h);
        Some(
            scrim()
                // Click away closes it, either button, swallowed so nothing
                // under the list also takes the press -- the clip menu's rule.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.picker = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.picker = None;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .id("picker-card")
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(MENU_W))
                        .flex()
                        .flex_col()
                        .p(px(MENU_PAD))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        // Painted after the scrim, so this listener runs first
                        // and a press meant for a row never closes the list out
                        // from under its own click (`context_card`).
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(MouseButton::Right, |_: &MouseDownEvent, _, cx| {
                            cx.stop_propagation()
                        })
                        .child(
                            div()
                                .id("picker-rows")
                                .flex()
                                .flex_col()
                                .max_h(px(list_h))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                ),
        )
    }

    /// One lane of the edit list made visible: a fixed header saying which lane
    /// it is, then a bed with a box per clip, placed and sized by its share of
    /// the timeline. A cut adds a box without moving anything, a delete closes
    /// the hole, a lift leaves one. A clip too narrow for its name says what it
    /// is by its tint, and the tooltip is where every box says what clicking it
    /// does. Never focusable, so the root keeps focus and the play binding still
    /// works after a click (ledger:182).
    /// The subtitle strip: where the picked track's cues are along the very same
    /// bed the lanes are drawn on, through the very same [`Scale`], so a cue
    /// lines up with the take it belongs to at every zoom.
    ///
    /// Display only -- no drag, no trim, no drop. What it is *for* is the
    /// question "is there a subtitle here", which a timeline could not answer at
    /// all before: the cues are the project's and are edited in the file they
    /// came from.
    ///
    /// `None` with no track picked, so a timeline without subtitles is the panel
    /// it has always been.
    fn subtitle_strip(&self, filled: f32) -> Option<impl IntoElement + use<>> {
        let track = self.subtitle_track()?;
        let scale = self.scale;
        // The pick with its film on it, not the raw tag: two films' "eng"
        // tracks read alike otherwise, and a header saying "und" names a
        // language nobody speaks.
        let label = sub_pick_name(self.session.as_ref()?.subtitles(), self.sub_track)?;
        // The colour the file's own rows and clips wear, so the strip says
        // whose subtitles these are before the tooltip is asked. `None` for a
        // standalone `.srt` -- nobody's stream, and the first film's colour
        // would be a lie about where it came from.
        let tint = file_tint(self.sources(), &track.path).unwrap_or(SURFACE);
        // The whole of what the row says in words, since a 40 px column can hold
        // three characters of it: which track of which file, how many cues, the
        // file itself in full -- one stem is two films when they are two cuts of
        // it -- and, for a track that could not be read, the engine's own reason,
        // where the library row's grey already says the same thing.
        let tip: SharedString = match track.refused.is_some() {
            true => format!(
                "Subtitles: {label} — {} — {}",
                subtitle_detail(track),
                track.path.display()
            ),
            false => format!(
                "Subtitles: {label} — {}, {} hides them — {}",
                subtitle_detail(track),
                self.keymap.display(ActionId::ToggleSubtitles),
                track.path.display()
            ),
        }
        .into();
        // Where the cues are on *this* timeline and not in the file they came
        // from ([`PlaybackSession::timeline_cues`]) -- the same map the plate
        // over the picture and the export both go through, which is what keeps
        // a mark under the take it is spoken over after a cut.
        let cues: Vec<(f32, f32)> = self
            .session
            .as_ref()?
            .timeline_cues(self.sub_track)
            .iter()
            .map(|cue| cue_box(scale, cue))
            .collect();
        Some(
            div()
                .flex_none()
                .h(px(SUB_LANE_H))
                .flex()
                .gap(px(HEADER_GAP))
                .child(
                    // Identified for the tooltip's sake and for nothing else:
                    // the whole of what a 40 px column can say is three
                    // characters, and the rest of it hangs off the hover.
                    div()
                        .id("subtitle-lane")
                        .flex_none()
                        .w(px(HEADER_W))
                        .h_full()
                        .flex()
                        .items_center()
                        // From the left rather than centred, unlike a lane's
                        // `V1`: a track label is a language or a file name, and
                        // a centred truncation eats both of its ends.
                        .px(px(2.))
                        .rounded(px(3.))
                        .bg(rgb(tint))
                        .text_size(px(9.))
                        .text_color(rgb(INK_DIM))
                        .truncate()
                        .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                        .child(label),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w(px(0.))
                        .h_full()
                        .rounded(px(3.))
                        .bg(rgb(LETTERBOX))
                        .overflow_hidden()
                        .children(cues.into_iter().map(|(left, width)| {
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .left(px(left))
                                .w(px(width))
                                .rounded(px(2.))
                                .bg(rgb(SELECTED))
                        }))
                        // The playhead again, last and in the lanes' own colour:
                        // the strip is only worth anything beside them if it
                        // reads as the same moment.
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .left(px(filled))
                                .w(px(1.))
                                .bg(rgb(ACCENT)),
                        ),
                ),
        )
    }

    fn lane_row(
        &self,
        lane: Lane,
        // Where the playhead is on the bed, in pixels: worked out once by the
        // panel so the ruler's line and every lane's draw the same one.
        filled: f32,
        cx: &mut Context<Self>,
        // Borrows nothing it was given (`use<>`): the rows are built one after
        // another into a list, and a row still holding `cx` would be the only
        // one that could be built.
    ) -> impl IntoElement + use<> {
        // The mapping, copied out once: every box in the row is placed through
        // it, so all of them move together when it does. No bed width is needed
        // to place them any more -- a second is so many pixels wherever it is.
        let scale = self.scale;
        // How much bed there is to be seen on, measured off the ruler's own
        // probe like every other question about it: what a box draws *inside*
        // itself is clipped to this ([`visible_slice`]), because a box at a deep
        // zoom is far wider than the strip it is being watched through.
        let bed = f32::from(self.ruler.get().size.width);
        // Where the snap line stands, in the same pixels every box is placed
        // through -- and only while a gesture is actually live: gpui drops a
        // drag without telling anyone, so this asks whether one is in flight
        // (`App::has_active_drag`) rather than remembering that one was.
        let cue = self
            .snap_cue
            .filter(|_| self.trim.is_some() || cx.has_active_drag())
            .map(|frame| scale.px_at(f64::from(frame) / self.fps));
        // The shadow, on the one lane the pointer is over -- and, like the line,
        // only while the drag that asked for it is still in flight.
        let ghost = self
            .ghost
            .filter(|g| g.lane == lane && cx.has_active_drag());
        let clips = self
            .session
            .as_ref()
            .map_or(&[][..], |session| session.lane_clips(lane));
        // The group ids some *other* lane carries: a clip whose id is in here
        // has a half elsewhere, and one whose is not is a detached half however
        // many lanes there are.
        let others: Vec<u32> = self.session.as_ref().map_or_else(Vec::new, |session| {
            session
                .lanes()
                .into_iter()
                .filter(|&other| other != lane)
                .flat_map(|other| session.lane_clips(other))
                .filter_map(|clip| clip.link)
                .collect()
        });
        let name = lane.label();
        let row_id: SharedString = format!("{name}-clip").into();
        let remove_id: SharedString = format!("{name}-remove").into();
        let remove_tip: SharedString = format!(
            "Remove {name} — it must be empty first, and {} brings it back",
            self.keymap.display(ActionId::Undo)
        )
        .into();
        let sources = self
            .session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources);
        let (sel, sel_link) = (self.selected, self.selected_link());
        let audio = lane.kind == LaneKind::Audio;
        // What this track plays at, on the header it belongs to: shown only
        // when it is not unity, because a column 40 px wide has room for a
        // number or for a name, and the name is what a header is for. The
        // press opens the mix card on this very track.
        let gain_db = self
            .session
            .as_ref()
            .map_or(0., |session| session.lane_gain_db(lane));
        let gain_tip: SharedString = format!(
            "{name} plays at {gain_db:+.0} dB — opens the mix ({}); the whole track, every frequency, unlike the equalizer",
            self.keymap.display(ActionId::Mix)
        )
        .into();
        let tip: SharedString = format!(
            "Select (or {} under the playhead, {}/{} along the lane) — drag it to move it, an end to trim, {} removes the take, {} leaves a gap, {} rejoins a cut",
            self.keymap.display(ActionId::Select),
            self.keymap.display(ActionId::SelectPrev),
            self.keymap.display(ActionId::SelectNext),
            self.keymap.display(ActionId::Delete),
            self.keymap.display(ActionId::Lift),
            self.keymap.display(ActionId::Regroup)
        )
        .into();
        div()
            .flex_none()
            .h(px(LANE_H))
            .flex()
            .gap(px(HEADER_GAP))
            // The fixed column the ruler above is offset by as well. Full lane
            // height, so it reads as the bed continuing rather than as a chip.
            .child(
                div()
                    .flex_none()
                    .w(px(HEADER_W))
                    .h_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .bg(rgb(SURFACE))
                    .text_size(px(11.))
                    .text_color(rgb(INK_DIM))
                    .child(match audio {
                        // A button, not a label: the one setting a track has of
                        // its own used to be reachable from nowhere.
                        true => div()
                            .id(("mix-lane", lane.ord))
                            .flex_1()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(HOVER)))
                            .tooltip(move |_, cx| cx.new(|_| Tip(gain_tip.clone())).into())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.open_mix(Some(lane), cx)
                            }))
                            .child(match gain_db == 0. {
                                true => name.clone(),
                                false => format!("{name} {gain_db:+.0}"),
                            })
                            .into_any_element(),
                        false => div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .child(name.clone())
                            .into_any_element(),
                    })
                    // The one thing a header does: take this track away again.
                    // A `HIT_MIN` target rather than a glyph-sized one, and it
                    // stays put on a track holding clips instead of hiding --
                    // the refusal names them, and a control that vanishes
                    // teaches nothing.
                    .child(
                        div()
                            .id(remove_id)
                            .flex_none()
                            .w_full()
                            .h(px(HIT_MIN))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(HOVER)))
                            .tooltip(move |_, cx| cx.new(|_| Tip(remove_tip.clone())).into())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.remove_lane(lane, cx)
                            }))
                            .child("×"),
                    ),
            )
            .child(
                // Clips are placed at their own start rather than queued edge
                // to edge: a lift leaves a hole in the lane, and the bare bed
                // showing through it *is* how a gap looks.
                div()
                    .relative()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .rounded(px(3.))
                    .bg(rgb(LETTERBOX))
                    .overflow_hidden()
                    // A library row let go over a lane is the same insert the
                    // Add button makes, through the same call -- but where the
                    // pointer let it go, not at the playhead: a hand that
                    // carried a file to a place on the bed named that place.
                    // gpui hands a drop no event, so the pointer is read off
                    // the window, which took it from the release that fired
                    // this (gpui window.rs:3602).
                    .on_drop(cx.listener(move |this, drag: &AssetDrag, window, cx| {
                        // Onto the edges near it, exactly as a clip carried by
                        // hand lands: the line drawn while it was in flight is
                        // the frame it goes down on.
                        let at = this.place_frame(window.mouse_position().x).0;
                        this.insert_source(&drag.0.clone(), drag.1, Some(lane), Some(at), cx)
                    }))
                    .drag_over::<AssetDrag>(|s, _, _, _| s.bg(rgb(HOVER_DIM)))
                    // The shadow of the row in flight, drawn by the lane the
                    // pointer is inside: `on_drag_move` fires on every painted
                    // element while a drag of its type is live, wherever the
                    // pointer is, and hands each one its own box -- which is how
                    // a lane knows the pointer is over *it* (gpui div.rs:282).
                    // The root cleared it a moment ago, so exactly one lane
                    // draws one.
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<AssetDrag>, _, cx| {
                            if !event.bounds.contains(&event.event.position) {
                                return;
                            }
                            let path = event.drag(cx).0.clone();
                            this.preview_ghost_asset(&path, lane, event.event.position.x, cx);
                        },
                    ))
                    // ...and a clip let go over a lane lands the same way: on
                    // the track it was dropped on, at the frame it was carried
                    // to -- its own included, which is the drag that moves a
                    // take along its track.
                    .on_drop(cx.listener(move |this, drag: &ClipDrag, window, cx| {
                        // Against the lane as it is *now* ([`Player::dragged`]),
                        // and then snapped by `move_clip` like any other drop:
                        // which clip is being moved and where it lands are two
                        // questions, and this is the first one.
                        let Some(idx) = this.dragged(drag) else {
                            return;
                        };
                        this.move_clip(drag.lane, idx, lane, window.mouse_position().x, cx)
                    }))
                    .drag_over::<ClipDrag>(|s, _, _, _| s.bg(rgb(HOVER_DIM)))
                    // ...and the same shadow for the clip in the hand, seated on
                    // this lane when the pointer is inside it.
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<ClipDrag>, _, cx| {
                            if !event.bounds.contains(&event.event.position) {
                                return;
                            }
                            let drag = *event.drag(cx);
                            this.preview_ghost(&drag, lane, event.event.position.x, cx);
                        },
                    ))
                    .children(clips.iter().enumerate().map(|(i, clip)| {
                        // The clip as the lane holds it, for the drag payload:
                        // what a drop looks itself up by has to be the placed
                        // clip, never the preview an edge drag is drawing.
                        let placed = *clip;
                        // What a drag on an edge is showing, which is the clip
                        // itself while nothing is being dragged.
                        let clip = &self.trimmed(lane, i, *clip);
                        // Its *timeline* length, which a speed halves or
                        // quadruples: the box is as wide as the clip is long
                        // where it sits, not as long as the source it reads.
                        let (start, len) = (
                            f64::from(clip.start) / self.fps,
                            f64::from(clip.frames()) / self.fps,
                        );
                        let on = marked((lane, i), clip.link, sel, sel_link);
                        // A group with a half in the other lane wears its tint;
                        // one without is outlined, so a detached half is visible
                        // as detached before anyone clicks it.
                        let grouped = clip.link.is_some_and(|link| others.contains(&link));
                        // Tinted by *file*, not by source entry: two audio
                        // streams of one file are two sources, and the library
                        // gives them one swatch because they are one file.
                        let tint = self.clip_tint(clip.source);
                        // What the clip is worth in pixels, and how wide its box
                        // is drawn -- the two part company on a take too short
                        // to be hit at this zoom ([`clip_width`]).
                        let span = scale.width_px(len);
                        let width = clip_width(span);
                        let left = scale.px_at(start);
                        // The slice of this box that is on the bed: where its
                        // name, its badge and its waveform go, so none of the
                        // three is drawn out at a zoomed-in box's own edges.
                        let (vis_x, vis_w) = visible_slice(left, width, bed);
                        let label = sources.get(clip.source).map(|s| file_name(&s.path));
                        let wave = sources
                            .get(clip.source)
                            .and_then(|s| self.waves.get(&(s.path.clone(), s.audio_stream)))
                            .cloned();
                        // The source seconds that slice plays -- not the clip's
                        // whole range: the envelope is drawn for the part of the
                        // box that can be seen, at the resolution of the pixels
                        // it actually has, and never one column per two pixels
                        // of a box millions of pixels wide.
                        let along = |x: f32| match width > 0. {
                            true => {
                                f64::from(clip.in_frame)
                                    + f64::from(clip.out_frame - clip.in_frame)
                                        * f64::from(x / width)
                            }
                            false => f64::from(clip.in_frame),
                        };
                        let (from, to) = (along(vis_x) / self.fps, along(vis_x + vis_w) / self.fps);
                        let tip = tip.clone();
                        // What the pointer carries on the way to another lane:
                        // the file the box is showing, the same ghost a library
                        // row makes. A box too narrow for its own label still
                        // says what is moving.
                        let ghost: SharedString =
                            label.clone().unwrap_or_else(|| lane.label()).into();
                        // Its head in frames, for the press below: the `start`
                        // above is the same moment in seconds, which is what
                        // the box is *drawn* from.
                        let head = clip.start;
                        div()
                            // Named per lane: two rows numbering their clips
                            // from zero would hand gpui the same id twice.
                            .id((row_id.clone(), i))
                            .absolute()
                            .top_0()
                            .h_full()
                            // Negative once the clip's head has been scrolled
                            // off the left edge: the bed clips what hangs out
                            // of it, so a half-visible clip is drawn as the
                            // half of itself that is on screen.
                            .left(px(left))
                            .w(px(width))
                            .overflow_hidden()
                            .rounded(px(3.))
                            .border_1()
                            .border_color(rgb(if on {
                                ACCENT
                            } else if grouped {
                                tint
                            } else {
                                INK_DIM
                            }))
                            .bg(rgb(if on { SELECTED } else { tint }))
                            .cursor_pointer()
                            .hover(|s| s.border_color(rgb(ACCENT)))
                            .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                            // Dragged, it *moves*: to the frame it was let go on
                            // and to the lane it was let go over. The click that
                            // starts the drag still selects, so picking a clip
                            // up and putting it back down where it was is
                            // exactly a click.
                            .on_drag(
                                ClipDrag {
                                    lane,
                                    idx: i,
                                    clip: placed,
                                },
                                move |_, _, _, cx| cx.new(|_| Tip(ghost.clone())),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    // Where in the box the hand took hold of it,
                                    // for the drag this press may become: the
                                    // clip has to move with the pointer rather
                                    // than jump its head under it.
                                    this.grab =
                                        this.frame_under(event.position.x).saturating_sub(head);
                                    this.select((lane, i), cx);
                                }),
                            )
                            // The right button selects exactly as the left one
                            // does -- the menu acts on the clip it names, so
                            // opening one has to pick it -- and then hangs the
                            // menu at the pointer.
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.open_menu(lane, i, event.position, cx);
                                }),
                            )
                            // The two strips a drag *lengthens* the clip by,
                            // one at each end. They occlude the box behind
                            // them, which is what keeps one gesture one thing:
                            // a press here trims, a press anywhere else on the
                            // box still starts the move to another lane.
                            //
                            // Asked of the clip's own width and not of the box's
                            // floor: a take drawn at `HIT_MIN` because it is
                            // shorter than that has no *pixels* to trim by -- one
                            // would move it by seconds -- so it keeps all of its
                            // padded box as a body to select and drag by, and is
                            // trimmed after zooming in, exactly as [`trims`] says.
                            .children(
                                [Edge::Start, Edge::End]
                                    .into_iter()
                                    .filter(|_| trims(span))
                                    .map(|edge| {
                                        let mut zone = div()
                                            .absolute()
                                            .top_0()
                                            .h_full()
                                            .w(px(EDGE_W))
                                            .occlude()
                                            .cursor(CursorStyle::ResizeLeftRight)
                                            .hover(|s| s.bg(rgb(ACCENT)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    move |this, _: &MouseDownEvent, _, cx| {
                                                        this.start_trim(lane, i, edge, cx);
                                                    },
                                                ),
                                            )
                                            // Occluded, so the box's own right-button
                                            // listener never fires here: the menu is
                                            // the same menu, opened by the same call.
                                            .on_mouse_down(
                                                MouseButton::Right,
                                                cx.listener(
                                                    move |this, event: &MouseDownEvent, _, cx| {
                                                        this.open_menu(lane, i, event.position, cx);
                                                    },
                                                ),
                                            );
                                        zone = match edge {
                                            Edge::Start => zone.left_0(),
                                            Edge::End => zone.right_0(),
                                        };
                                        zone
                                    }),
                            )
                            // Under the label row, never through it.
                            .children(wave.filter(|_| audio && vis_w > 0.).and_then(|wave| {
                                let inner: AnyElement = match wave {
                                    Wave::Peaks(peaks) => {
                                        waveform(peaks, from, to).into_any_element()
                                    }
                                    // A bed while the decode runs, and dimmer
                                    // than any waveform is drawn: a flat
                                    // `INK_DIM` line here would be the shape a
                                    // silent file makes, which this file is not
                                    // known to be yet.
                                    Wave::Loading => div()
                                        .h_full()
                                        .flex()
                                        .items_center()
                                        .child(div().w_full().h(px(1.)).bg(rgb(HOVER)))
                                        .into_any_element(),
                                    // No audio track: nothing, never a fake.
                                    Wave::Silent => return None,
                                    // Could not be read: a band that says so in
                                    // words, because the empty band a silent
                                    // file draws would claim this file has no
                                    // sound. The reason itself went to the log.
                                    Wave::Failed => div()
                                        .h_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .truncate()
                                        .text_size(px(9.))
                                        .text_color(rgb(INK_DIM))
                                        .child("audio unreadable")
                                        .into_any_element(),
                                };
                                Some(
                                    div()
                                        .absolute()
                                        .left(px(vis_x))
                                        .w(px(vis_w))
                                        .top(px(LABEL_H))
                                        .bottom_0()
                                        .child(inner),
                                )
                            }))
                            // A speeded clip says so on the box, in the corner
                            // the label does not reach: the box's width alone
                            // cannot say whether a short clip is a trim or a
                            // clip at 4x, and that is the difference between a
                            // cut and a re-time.
                            // Against the right edge of what is *visible* of the
                            // box, not of the box: zoomed in, the box's own
                            // right edge is off the screen and the badge with
                            // it, which is a clip that stops saying it is
                            // speeded exactly when it is being looked at
                            // closely.
                            .when(!clip.speed.is_normal() && vis_w > 0., |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left(px(vis_x))
                                        .w(px(vis_w))
                                        .flex()
                                        .justify_end()
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .flex_none()
                                                .px(px(3.))
                                                .rounded(px(3.))
                                                .bg(rgb(ACCENT))
                                                .text_size(px(9.))
                                                .text_color(rgb(SURFACE))
                                                .child(format!("{}", clip.speed)),
                                        ),
                                )
                            })
                            // ...and the name sits at the left edge of the same
                            // slice, for the same reason: a box scrolled half
                            // off names itself on the half that is on screen.
                            .when_some(label.filter(|_| show_label(vis_w)), |d, label| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left(px(vis_x))
                                        .w(px(vis_w))
                                        .h(px(LABEL_H))
                                        .px(px(4.))
                                        .truncate()
                                        .text_size(px(10.))
                                        .child(label),
                                )
                            })
                    }))
                    // What the silence card found, over the clips it found them
                    // in and over the waveform band that shows why: on the lane
                    // the scan ran on and no other, because that is the only
                    // lane whose sound was read. Drawn before anything is cut,
                    // and replaced -- never stacked -- by every re-run.
                    .children(
                        self.silence_marks
                            .iter()
                            .filter(|_| self.silence_open.is_some_and(|(on, _)| on == lane))
                            .map(|&(at, len)| {
                                div()
                                    .absolute()
                                    .top_0()
                                    .h_full()
                                    .left(px(scale.px_at(f64::from(at) / self.fps)))
                                    // Floored like a cue's mark and for the same
                                    // reason: a half-second silence on a zoomed-
                                    // out bed rounds to nothing, and a preview
                                    // that draws nothing reads as a scan that
                                    // found nothing.
                                    .w(px(scale
                                        .width_px(f64::from(len) / self.fps)
                                        .max(SUB_CUE_MIN_W)))
                                    .bg(rgba(0x4a9effaa))
                            }),
                    )
                    // Where the thing in the hand would come to rest, at the size
                    // it would come to rest at: the shadow a proper editor draws
                    // under a drag. Over the clips (it is translucent, so what
                    // it would cover shows through) and under the line, which
                    // marks the frame this box merely fills.
                    .children(ghost.map(|g| {
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(px(scale.px_at(f64::from(g.start) / self.fps)))
                            // A row whose length the engine has not measured
                            // draws a head marker rather than nothing: where it
                            // lands is known, how long it is is not.
                            .w(px(scale
                                .width_px(f64::from(g.frames) / self.fps)
                                .max(GHOST_MIN)))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(rgb(if g.refused { REFUSE } else { INK }))
                            // The file's own swatch at a third of its weight, so
                            // the box beneath is still legible through it -- and
                            // the refusal red instead, for a lane that will not
                            // take this drop at all.
                            .bg(rgba(
                                ((if g.refused { REFUSE } else { g.tint }) << 8) | GHOST_ALPHA,
                            ))
                    }))
                    // What the gesture in flight is about to land on, drawn on
                    // every lane so a clip lining up with a take one track over
                    // can be seen to line up with it. Under the playhead's line
                    // and in another colour, since the two mean different
                    // things and often stand on the same pixel.
                    .children(cue.map(|x| {
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(px(x))
                            .w(px(1.))
                            .bg(rgb(INK))
                    }))
                    // Last, so it is over the clips: the same fraction in both
                    // lanes, which is the playhead being one line.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(px(filled))
                            .w(px(1.))
                            .bg(rgb(ACCENT)),
                    ),
            )
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
    /// An import: pages warmed, and the subtitle tracks the walk found *kept* --
    /// the walk is the expensive half and the worker is where it belongs, so
    /// [`Player::take_import`] is left with a push ([`subtitle_tail`]).
    Read(Subs),
    /// The media argv named, with the tail its subtitle tracks earn: a whole
    /// timeline, ready to be hung off the window, or the engine's refusal.
    Media(Result<(PlaybackSession, String), String>),
    /// The `.edith` argv named, restored.
    Project(Result<PlaybackSession, String>),
}

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
) -> Landed {
    use std::sync::atomic::Ordering::Relaxed;
    match what {
        Landing::Import => Landed::Read(read_ahead(path, stage)),
        Landing::Project => {
            let opened = PlaybackSession::open_project(path).map_err(|e| e.to_string());
            Landed::Project(opened)
        }
        Landing::Open => {
            let opened = PlaybackSession::open(path).map_err(|e| e.to_string());
            // The same two stages a read reports, because they are the same two
            // reads: the container, and then the tracks inside it.
            stage.store(ImportStage::Subtitles as u8, Relaxed);
            Landed::Media(opened.map(|mut session| {
                let subs = subtitle_notice(&mut session, path).unwrap_or_default();
                (session, subs)
            }))
        }
    }
}

/// Reads, off the UI thread, exactly what the import that follows is about to
/// read. The container's header is read for the page cache and thrown away: a
/// cold header walk of a 29 GB remux is 11 s and a warm one is 150 ms
/// (measured on a real 2160p h265 remux), so this call is the eleven
/// seconds and [`Player::take_import`] is the hundred and fifty milliseconds.
/// The window keeps painting through the eleven.
///
/// Header errors are dropped on purpose: a file that cannot be read is refused
/// by the engine a moment later, in the engine's own words, and a refusal read
/// twice is a refusal worded twice. The *subtitle* refusal is carried back
/// instead of dropped, for exactly that reason -- nothing walks those cues a
/// second time to re-word it.
///
/// `stage` is what the line above the panel is naming while this runs.
///
/// The subtitle half is not thrown away: it is *the* answer, handed back for
/// [`Player::take_import`] to push ([`subtitle_tail`]) rather than walked a
/// second time on the render thread -- 234 ms of a warmed 25 GB remux and 1.3 s
/// of a cold 3 GB one, which is what a frozen window is made of.
///
/// ponytail: only the subtitle half is handed over. The container's own header
/// is still merely *warmed* here, so [`PlaybackSession::import`] and
/// [`open_session`] walk it again on the UI thread -- 2.8 s of that same 25 GB
/// file. Ceiling: the media walk, not the subtitle one. The upgrade is the
/// engine door this comment has always named: one that takes the header this
/// already parsed, the way [`PlaybackSession::add_subtitle_tracks`] takes the
/// cues this already read.
fn read_ahead(path: &std::path::Path, stage: &std::sync::atomic::AtomicU8) -> Subs {
    use std::sync::atomic::Ordering::Relaxed;
    stage.store(ImportStage::Header as u8, Relaxed);
    // The three doors an import goes through, each warmed by the call the
    // engine will make: a song is measured by its duration, a still by its
    // header, and everything else by the container's.
    if engine::is_audio(path) {
        engine::AudioSession::duration_secs(path).ok();
    } else if !engine::is_image(path) && !is_subtitle(path) {
        engine::demux::Demuxer::open(path).ok();
    }
    stage.store(ImportStage::Subtitles as u8, Relaxed);
    // ...and the tracks inside it, kept.
    walk_subtitles(path)
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
/// ponytail: that split reads the same public fields the list rows read
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
    if action == ActionId::ShowActions {
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
            Some((clip, _))
                if !(clip.start < ctx.playhead
                    && ctx.playhead < clip.end()
                    && clip
                        .speed
                        .split_at(clip.len(), ctx.playhead - clip.start)
                        .is_some()) =>
            {
                Enable::No("only from inside a clip")
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
        ActionId::CancelExport => Enable::No("nothing is exporting"),
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
/// ponytail: one unit for the whole line, so a file mixing a multi-megabit
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
    SOURCE_TINTS[source % SOURCE_TINTS.len()]
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
                format!("{custom_mbps} Mbps — n types a number, {MBPS_MIN}–{MBPS_MAX}")
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
/// engine clamps every explicit bitrate to 1..20 Mbps (export.rs:290), so this
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
/// this it clamps (`export.rs` `MIN_BITRATE`/`MAX_BITRATE`), so a number typed
/// past either end would be written as a different one. The field refuses it
/// instead of clamping quietly -- a card that changes the user's number without
/// saying so is the one thing a field like this must never do.
const MBPS_MIN: u32 = 1;
const MBPS_MAX: u32 = 20;

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

/// One press of a zoom key, or one notch of ctrl+wheel.
const ZOOM_STEP: f32 = 1.25;

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
        let span = f64::from(self.bed) / scale.pps;
        if at < scale.start || at > scale.start + span {
            View {
                scale: Scale {
                    start: at - span / 2.,
                    ..scale
                },
                ..self
            }
            .settled()
        } else {
            scale
        }
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

/// One clip's audio, drawn as a filled min/max envelope inside whatever box it
/// is given. Peaks are the source's whole envelope; `from`/`to` are the source
/// seconds this clip plays, so a cut clip shows its own stretch of the file.
fn waveform(peaks: Arc<Vec<(f32, f32)>>, from: f64, to: f64) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let cols = envelope(&peaks, from, to, f32::from(s.width), f32::from(s.height));
            if cols.len() < 2 {
                return;
            }
            // Down the tops and back along the bottoms: one closed outline of
            // the whole envelope, which is one path rather than a path a column.
            let mut points: Vec<Point<Pixels>> = cols
                .iter()
                .map(|&(x, top, _)| point(o.x + px(x), o.y + px(top)))
                .collect();
            points.extend(
                cols.iter()
                    .rev()
                    .map(|&(x, _, bottom)| point(o.x + px(x), o.y + px(bottom))),
            );
            let mut path = PathBuilder::fill();
            path.add_polygon(&points, true);
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(INK_DIM));
            }
        },
    )
    .size_full()
}

/// A toolbar button: its glyph, its name, and its key on hover. `id` only buys
/// `on_click` and the tooltip -- it is still not focusable, so the root's own
/// key listener keeps working after a press, and the click lands on mouse-up
/// inside the button (a press that slides off does nothing).
///
/// A button that would do nothing says so: dimmed, no pointer, no listener.
fn control(
    id: &'static str,
    glyph: Option<AnyElement>,
    // Not `&'static str`: the volume button's label is its state.
    label: impl Into<SharedString>,
    shortcut: String,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let tip: SharedString = format!("{label} — {shortcut}").into();
    div()
        .id(id)
        .flex_none()
        .h(px(CONTROL_H))
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(8.))
        .rounded(px(3.))
        .bg(rgb(SURFACE))
        .children(glyph)
        .child(label)
        .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
        .when(enabled, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
                .on_click(on_click)
        })
}

/// The monitoring level as something to drag: 4 px of bar to look at and the
/// whole control's height to hit (WCAG 2.5.8), the split the speed bar and the
/// colour sliders both make. Only the level -- mute is the button beside it, so
/// a muted slider still shows what unmuting comes back to, drawn dim.
///
/// Dimmed and inert without a timeline, like every other control that would
/// have nothing to act on.
fn volume_slider(
    volume: Volume,
    bar: Rc<Cell<Bounds<Pixels>>>,
    enabled: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    div()
        .id("volume-bar")
        .relative()
        .flex_none()
        .w(px(VOLUME_W))
        .h(px(CONTROL_H))
        .flex()
        .items_center()
        .tooltip(|_, cx| {
            cx.new(|_| Tip("Volume — drag to set the level; the button mutes".into()))
                .into()
        })
        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
        .when(enabled, |d| {
            d.cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        this.volume_dragging = true;
                        this.drag_volume(event.position.x, cx);
                    }),
                )
                .child(bounds_probe(bar))
        })
        .child(
            div()
                .w_full()
                .h(px(4.))
                .rounded(px(2.))
                .bg(rgb(SURFACE))
                .child(
                    div()
                        .h_full()
                        .w(relative(volume.along()))
                        .rounded(px(2.))
                        .bg(rgb(if volume.muted { INK_DIM } else { ACCENT })),
                ),
        )
}

/// The line between two groups of buttons.
fn separator() -> impl IntoElement {
    div()
        .flex_none()
        .mx(px(4.))
        .w(px(1.))
        .h(px(18.))
        .bg(rgb(HOVER))
}

/// Whether a card or a menu is drawn over the window, as the hover labels see
/// it: written once a frame by [`Player::render`], read by every [`Tip`] before
/// it paints.
///
/// A tooltip already on screen when an overlay opens *stays* on screen in gpui:
/// occluding the surface under it does not take it back, because the check that
/// keeps it visible works off the element's absolute bounds and knows nothing
/// about what was painted over it (`div.rs::handle_tooltip_mouse_move`, its own
/// TODO). So the tip is what has to stand aside -- here, once, for every hover
/// label in this window, rather than at fifteen call sites of which the
/// sixteenth would be forgotten.
static OVERLAID: AtomicBool = AtomicBool::new(false);

/// A tooltip is a view in gpui and nothing smaller, so this is the smallest one
/// that carries a line of text. It paints outside the window's element tree and
/// therefore owns its colours.
struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // A card or a menu is up: nothing. A line of text over the items of the
        // menu that just opened under the pointer is the card being painted
        // over by the window it covers.
        if OVERLAID.load(Ordering::Relaxed) {
            return div();
        }
        div()
            .px(px(6.))
            .py(px(3.))
            .rounded(px(3.))
            .border_1()
            .border_color(rgb(SURFACE))
            .bg(rgb(CHROME))
            .text_color(rgb(INK))
            .text_size(px(12.))
            .child(self.0.clone())
    }
}

/// Scissors: two blades crossed. Drawn this way and not as a split clip because
/// two bars is what the transport wears when it is playing -- the one glyph a
/// cut must never be mistaken for.
fn cut_glyph() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let mut path = PathBuilder::stroke(px(1.5));
            path.move_to(point(o.x + s.width * 0.15, o.y + s.height));
            path.line_to(point(o.x + s.width * 0.9, o.y + s.height * 0.1));
            path.move_to(point(o.x + s.width * 0.85, o.y + s.height));
            path.line_to(point(o.x + s.width * 0.1, o.y + s.height * 0.1));
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(INK));
            }
        },
    )
    .w(px(13.))
    .h(px(13.))
}

/// A lid over a bin.
fn delete_glyph() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .child(div().w(px(13.)).h(px(2.)).bg(rgb(INK)))
        .child(div().w(px(9.)).h(px(9.)).bg(rgb(INK)))
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
                    window.paint_path(path, rgb(HIST_INK[channel]));
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
/// ponytail: one transform length for the whole axis, so the bass end is a bin
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
                window.paint_path(path, rgba(EQ_SPECTRUM_INK));
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
/// ponytail: bands that overlap can sum past the ±`EQ_GAIN_LIMIT` axis and the
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
                    window.paint_path(bell, rgba(EQ_BELL_INK));
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
                window.paint_path(area, rgba(EQ_FILL_INK));
            }
            let mut path = PathBuilder::stroke(px(2.));
            for (step, at) in points.into_iter().enumerate() {
                match step {
                    0 => path.move_to(at),
                    _ => path.line_to(at),
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(ACCENT));
            }
        },
    )
    .absolute()
    .size_full()
}

/// What a window with no file open is waiting for. Both ways in are already
/// built -- the whole window is the drop target and the Import chooser takes a
/// project as readily as media -- so this only has to say so.
fn empty_hint() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .text_color(rgb(INK_DIM))
        .child("Drop a video or an .edith project here")
        .child(
            div()
                .text_size(px(11.))
                .child("or click Import in the media list"),
        )
}

/// Two bars while playing, a triangle in every other state -- paused, nothing
/// open, and played out, where the button's next act is to start over rather
/// than to stop something. Drawn, so there is no icon font and no glyph
/// coverage to depend on.
fn transport_glyph(state: Transport) -> impl IntoElement {
    let playing = state.is_playing();
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .when(playing, |d| {
            d.child(div().w(px(3.)).h(px(12.)).bg(rgb(INK)))
                .child(div().w(px(3.)).h(px(12.)).bg(rgb(INK)))
        })
        .when(!playing, |d| {
            d.child(
                canvas(
                    |_, _, _| (),
                    |bounds, _, window, _| {
                        let (o, s) = (bounds.origin, bounds.size);
                        let mut path = PathBuilder::fill();
                        path.move_to(o);
                        path.line_to(point(o.x + s.width, o.y + s.height / 2.));
                        path.line_to(point(o.x, o.y + s.height));
                        path.close();
                        if let Ok(path) = path.build() {
                            window.paint_path(path, rgb(INK));
                        }
                    },
                )
                .w(px(11.))
                .h(px(13.)),
            )
        })
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
/// arrival of the same path -- a drop of the film that is already open -- is an
/// import, which is what a drop has always been.
fn arrival(opening: Option<&std::path::Path>, path: &std::path::Path) -> Landing {
    match opening == Some(path) {
        false => Landing::Import,
        true if is_project(path) => Landing::Project,
        true => Landing::Open,
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
fn scan_plan(cached: bool, running: Option<&(PathBuf, usize)>, key: &(PathBuf, usize)) -> ScanPlan {
    match (cached, running) {
        (true, _) => ScanPlan::Marks,
        (false, Some(at)) if at == key => ScanPlan::Wait,
        _ => ScanPlan::Start,
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
mod tests {
    use super::{
        ACCENT, AUDIO_KBPS, COLOR_BANDS, COLOR_BAR_W, COLOR_STEP, COLOR_W, CONTROL_H, Clip, Ctx, DEFAULT_AUDIO_KBPS, EQ_BANDS_MAX,
        EQ_CURVE_STEPS, EQ_FFT, EQ_FREQ_HIGH, EQ_FREQ_LOW, EQ_FREQ_STEP, EQ_GAIN_LIMIT, EQ_GRAPH_H,
        EQ_HANDLE, EQ_Q_HIGH, EQ_Q_LOW, EQ_Q_STEP, EQ_SPECTRUM_DB, EQ_TICKS, EQ_W_MAX, ESCAPE,
        EXPORT_FIXED_H, EXPORT_ROWS_H, EXPORT_W, Enable, FORMATS,
        Format, HEADER_GAP, HEADER_H, HEADER_W, HIST_BINS, HIST_H, HIST_SAMPLES, HIT_MIN, INK,
        INK_DIM, KEYS_ROW_H, KEYS_ROWS_H, KEYS_W, KeyRow, LABEL_H, LABEL_MIN_W, LANE_H, LANES_MAX,
        LETTERBOX, LIBRARY_MAX_W, LIBRARY_MIN_W, Lane, MENU_ITEMS, MENU_PAD, MENU_ROW_H,
        MBPS_DIGITS, MBPS_MAX, MBPS_MIN, MB_FLOOR, MENU_W, NO_FILE, NumberEdit, PANEL_H, ROW_ITEMS, RowCtx, RowItem,
        Quality, ROW_H, RULER_HIT_H, SELECTED, SILENCE_ROWS,
        SOURCE_TINTS, SPEED_PRESETS, SPEED_STEP, SURFACE, SWATCH_W, Source, Speed, StreamInfo,
        Transport, VOLUME_W, Volume, WAVE_BPS, WAVE_COL, WAVE_COLS_MAX, Wave, band_label,
        bitrate_detail, bitrate_refusal, can_add, cancels_export, clipboard_after_remove, color_snap, commit_mbps,
        containers,
        enable, envelope, eq_card_w, eq_freq, eq_freq_label, eq_spectrum, eq_x, eq_y,
        estimated_bytes, export_path, export_settings, format_key,
        format_line, format_refusal, fps_choices, fps_label, frac_along, frac_down, frame_at,
        frame_rate_ladder, histogram,
        inserted_band, is_bare_modifier, is_project, keymap, keys_filter, keys_rows, lanes_h,
        marked, menu_at, menu_items, menu_rows_h, next_audio_kbps, next_container, normalise, nothing_to_play, panel_h, project_path,
        retarget, row_enable, row_items, scrub_due, secs_label, show_label, silence_rate, size_label, snap_cue, snap_marks, snapped,
        SUB_PLAN_CHARS, source_tint, span_partner, speed_at, sub_pick_after_removal, subtitle_plan, summary_head, summary_tail, timecode, transport,
        typed, unseen_paths, unseen_sources,
        whole_take, window_title,
    };
    use super::{
        Choice, EDGE_W, ETA_SPAN, Edge, FITS, FRAME_RATES, LaneKind, PPS_DEFAULT, PPS_MIN, Preset, RESOLUTIONS, Repeat,
        Scale, View,
        ZOOM_MIN_FRAMES, ZOOM_OUT_MARGIN, ZOOM_STEP, audio_rate_choices, clock, eta_secs, file_name, file_uri,
        fit_choices, landing, lane_refuses, library_rows, live_idx, next_fit, next_resolution,
        note_progress, px_along,
        IMPORT_STALL, Import, ImportStage, SEEK_STALL, ScanPlan, SilenceScan, import_line,
        Landing, arrival, launch_queue,
        read_ahead, scan_plan, seek_line, silence_line, stash_or_write,
        repeats, resolution_choices, resolution_ladder, span_label, tone_choices, tone_label,
        clip_width, trimmed_clip, trims, unscannable, unusable,
        visible_slice,
    };
    use super::{
        SUB_BOTTOM, SUB_CUE_MIN_W, SUB_HEAD_H, SUB_INK, SUB_LANE_H, SUB_LINE_H, SUB_ROWS_H,
        SUB_STEM_SHARE, SUB_TEXT, clip_middle, cue_box,
        carries_subtitles, cues_at, file_tint, is_subtitle, lang_human, row_text_w, sub_headers_fit,
        Subs, subtitle_detail, subtitle_notice, subtitle_tail,
        sub_pick_name, subtitle_rows, subtitle_strip_h, walk_subtitles,
    };

    /// What the file manager is handed: the parts a path keeps as they are, and
    /// the ones the bus would otherwise read as something else.
    #[test]
    fn file_uri_encodes_what_a_path_carries() {
        assert_eq!(
            file_uri(std::path::Path::new("/a/b.mp4")),
            "file:///a/b.mp4"
        );
        assert_eq!(
            file_uri(std::path::Path::new("/home/x/out dir/my export.mp4")),
            "file:///home/x/out%20dir/my%20export.mp4"
        );
        assert_eq!(
            file_uri(std::path::Path::new("/tmp/ünlü#1?.mp4")),
            "file:///tmp/%C3%BCnl%C3%BC%231%3F.mp4"
        );
    }
    use engine::PlaybackSession;
    use engine::scale::FitPolicy;
    use gpui::{Bounds, Pixels, point, px, size};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    /// A source entry for `path` on `stream`, as the project keeps them.
    fn source(path: &str, stream: usize) -> Source {
        Source {
            path: PathBuf::from(path),
            audio_stream: stream,
        }
    }

    /// An import fills the *library* and nothing else -- the whole point of
    /// this door: the row is there at the file's own length before any clip
    /// plays it, wearing the tint the lanes will tint that clip with, and the
    /// timeline is exactly as long as it was. Placing it is the drag, and the
    /// drag is a separate act.
    #[test]
    fn an_import_adds_a_row_at_its_own_length_and_leaves_the_lanes_alone() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        let before = session.timeline_duration();
        let clips: Vec<usize> = session
            .lanes()
            .into_iter()
            .map(|lane| session.lane_clips(lane).len())
            .collect();
        session.import(&asset("test_av2.mp4")).expect("av2 matches");
        assert_eq!(session.sources().len(), 2, "the library grew");
        assert_eq!(
            session.timeline_duration(),
            before,
            "an import must not place a clip"
        );
        assert_eq!(
            session
                .lanes()
                .into_iter()
                .map(|lane| session.lane_clips(lane).len())
                .collect::<Vec<_>>(),
            clips,
            "no lane moved"
        );
        // The rows the panel draws: one per source, each its file's own length
        // -- source 1 has no clip anywhere and is still 4 s at 30 fps.
        let rows = library_rows(
            session.sources(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            |path| session.file_frames(path),
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].frames, 150, "5 s at 30 fps");
        assert_eq!(rows[1].frames, 120, "4 s at 30 fps, never placed");
        // The swatch is the clip colour, by the same index and the same
        // function -- what makes the panel and the lanes one association.
        for row in 0..rows.len() {
            assert_eq!(rows[row].tint, row);
            assert_eq!(source_tint(row), SOURCE_TINTS[row % SOURCE_TINTS.len()]);
        }
    }

    /// The window opened on a song and nothing else -- the launch argument, the
    /// drop on an empty window and the Import button all end in the same
    /// `PlaybackSession::open`. The library lists it placeable, the lane door
    /// the Add button and a drag share puts it on `A1`, and the one format that
    /// needs a picture says so on its own row instead of failing at the end of
    /// an export.
    #[test]
    fn a_song_opens_the_window_by_itself() {
        let mut session =
            PlaybackSession::open(asset("test_tone.mp3")).expect("a song is a timeline");
        session.set_gain(0.0);
        // The source's own path, which is the canonical one a row carries.
        let path = session.sources()[0].path.clone();
        assert!(session.lane_clips(Lane::V1).is_empty(), "no picture");
        assert_eq!(session.lane_clips(Lane::A1).len(), 1);

        // The library row: probed like any other source, and not greyed --
        // `unusable` is what the panel dims a row with.
        let streams = HashMap::from([(
            path.clone(),
            engine::AudioSession::probe_streams(&path).expect("probe the song"),
        )]);
        let rate = streams[&path]
            .iter()
            .find(|s| s.index == session.sources()[0].audio_stream)
            .map(|s| (s.sample_rate, s.channels));
        let frames = session.file_frames(&path);
        let rows = library_rows(session.sources(), &streams, &HashMap::new(), rate, |_| {
            frames
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "test_tone.mp3");
        assert_eq!(rows[0].unusable, None, "the row is placeable");
        assert_eq!(rows[0].frames, 90, "3 s at the audio-only 30 fps");

        // ...and it places, on the audio lane, through the door `insert_source`
        // uses: a second copy of the song at the playhead.
        session.seek(1.0);
        assert!(
            session
                .place_stream_at(1.0, &path, 0, Some(Lane::A1))
                .expect("its own file is on this timeline")
        );
        assert_eq!(session.lane_clips(Lane::A1).len(), 2);
        assert!(session.lane_clips(Lane::V1).is_empty(), "still no picture");

        // Both picture rows carry the reason rather than the format's detail
        // line, each naming itself; the audio formats are what this timeline is
        // and are never refused.
        assert_eq!(
            format_refusal(&session, Format::Mp4).as_deref(),
            Some("no picture — an mp4 would be black; export WAV, FLAC, MP3 or OGG")
        );
        assert_eq!(
            format_refusal(&session, Format::Av1).as_deref(),
            Some("no picture — an AV1 Matroska would be black; export WAV, FLAC, MP3 or OGG")
        );
        assert_eq!(
            format_refusal(&session, Format::Av1Mp4).as_deref(),
            Some("no picture — an AV1 mp4 would be black; export WAV, FLAC, MP3 or OGG")
        );
        assert_eq!(format_refusal(&session, Format::Wav), None);
        assert_eq!(format_refusal(&session, Format::Flac), None);
        assert_eq!(format_refusal(&session, Format::Mp3), None);
    }

    fn info(
        index: usize,
        rate: u32,
        channels: u16,
        lang: Option<&str>,
        decodable: bool,
    ) -> StreamInfo {
        StreamInfo {
            index,
            codec: if decodable { "aac" } else { "unknown" }.into(),
            channels,
            sample_rate: rate,
            lang: lang.map(str::to_string),
            decodable,
        }
    }

    /// A file's every audio track gets a row: the ones the timeline can take
    /// are placeable, the ones it cannot are listed greyed with the reason.
    /// Which rows exist at all is the branchy part of the panel, so it is
    /// planned as data and checked here rather than through the pointer.
    #[test]
    fn every_audio_stream_of_a_file_is_a_row_usable_or_not() {
        let multi = PathBuf::from("/m/movie.mp4");
        let sources = [source("/m/movie.mp4", 0)];
        let mut streams = HashMap::new();
        streams.insert(
            multi.clone(),
            vec![
                info(0, 44_100, 2, None, true),
                info(1, 44_100, 2, Some("fra"), true),
                info(2, 22_050, 1, Some("deu"), true),
                info(3, 0, 0, None, false),
            ],
        );
        let rows = library_rows(
            &sources,
            &streams,
            &HashMap::new(),
            Some((44_100, 2)),
            |_| 90,
        );
        assert_eq!(
            rows.iter().map(|r| r.stream).collect::<Vec<_>>(),
            [0, 1, 2, 3],
            "one row per audio stream, in file order"
        );
        assert!(
            rows.iter().all(|r| r.path == multi && r.tint == 0),
            "every stream of one file wears the file's own tint"
        );
        assert_eq!(rows[0].name, "movie.mp4 [audio 1]");
        assert_eq!(rows[1].name, "movie.mp4 [audio 2]");
        assert_eq!(rows[1].detail, "fra 44.1 kHz stereo");
        // Placeable: the one already on the timeline, and the one that matches
        // it. The mono track cannot join a stereo timeline -- one device and one
        // copied AAC track for the whole of it -- and the codec we cannot read
        // cannot join anything. Both say which. Its 22 kHz is *not* part of that
        // any more: a rate of its own is resampled at the decoder's door, and a
        // row greyed for one the engine accepts is this picker telling a lie.
        assert_eq!((&rows[0].unusable, &rows[1].unusable), (&None, &None));
        assert_eq!(rows[2].unusable.as_deref(), Some("the timeline is stereo"));
        assert_eq!(
            unusable(&info(9, 48_000, 2, None, true), Some((44_100, 2))),
            None,
            "48 kHz stereo joins a 44.1 kHz stereo timeline"
        );
        assert_eq!(rows[3].unusable.as_deref(), Some("unsupported codec"));
        assert_eq!(
            rows[3].detail, "",
            "a stream we cannot parse claims nothing"
        );
        // Every row is the same file, so every row is that file's length.
        assert!(rows.iter().all(|r| r.frames == 90));

        // The single-stream case is exactly one row and no stream tag: no
        // regression for the media everything else in the world is.
        let plain = [source("/m/plain.mp4", 0)];
        let mut one = HashMap::new();
        one.insert(
            PathBuf::from("/m/plain.mp4"),
            vec![info(0, 44_100, 2, None, true)],
        );
        let rows = library_rows(&plain, &one, &HashMap::new(), Some((44_100, 2)), |_| 90);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "plain.mp4");
        assert_eq!(
            rows[0].detail, "",
            "one audio track is the row it has always been: name and length"
        );
        // ...as is a silent file, and a file not probed yet.
        let mut silent = HashMap::new();
        silent.insert(PathBuf::from("/m/plain.mp4"), Vec::new());
        for probe in [silent, HashMap::new()] {
            let rows = library_rows(&plain, &probe, &HashMap::new(), None, |_| 90);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].name, "plain.mp4");
            assert_eq!(rows[0].detail, "");
        }

        // A second stream *placed* on the timeline is a source entry of its
        // own: it keeps its row, no duplicate appears for it, and the rows of
        // one file stay together whatever order the entries came in.
        let placed = [
            source("/m/movie.mp4", 0),
            source("/m/other.mp4", 0),
            source("/m/movie.mp4", 2),
        ];
        streams.insert(
            PathBuf::from("/m/other.mp4"),
            vec![info(0, 44_100, 2, None, true)],
        );
        let rows = library_rows(
            &placed,
            &streams,
            &HashMap::new(),
            Some((44_100, 2)),
            |_| 90,
        );
        assert_eq!(
            rows.iter()
                .map(|r| (file_name(&r.path), r.stream))
                .collect::<Vec<_>>(),
            [
                ("movie.mp4".to_string(), 0),
                ("other.mp4".to_string(), 0),
                ("movie.mp4".to_string(), 2),
                ("movie.mp4".to_string(), 1),
                ("movie.mp4".to_string(), 3),
            ]
        );
        assert!(
            rows[2].unusable.is_none(),
            "a stream already on the timeline is playing, whatever a probe says"
        );
        assert_eq!(rows[1].tint, 1, "the other file is the other tint");
    }

    /// What the clip menu offers, and where it hangs. The two items that act on
    /// the playhead rather than on the clicked clip are the ones that can be
    /// inapplicable, and a menu at the edge of the window has to come back
    /// inside it or its last item cannot be clicked at all.
    #[test]
    fn the_clip_menu_dims_what_the_playhead_is_not_on_and_stays_in_the_window() {
        use keymap::{ActionId, Keymap};
        // Frames 30..90 of the timeline, taken from the head of its source.
        let clip = Clip {
            start: 30,
            in_frame: 0,
            out_frame: 60,
            source: 0,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };
        assert_eq!(clip.end(), 90);
        let a1 = Lane::A1;
        let v1 = Lane::V1;
        // The question the clip menu asks: a timeline is open and the menu was
        // opened on this clip, at this playhead.
        let on = |clip: &Clip, lane, action, playhead| {
            enable(
                action,
                Ctx {
                    clip: Some((*clip, lane)),
                    playhead,
                    timeline: true,
                    ..Ctx::default()
                },
            )
        };
        let offered = |clip: &Clip, lane, action, playhead| on(clip, lane, action, playhead).yes();
        // Cut splits from inside only: neither edge has anything to split off.
        assert!(offered(&clip, v1, ActionId::Cut, 31));
        assert!(offered(&clip, v1, ActionId::Cut, 89));
        assert!(!offered(&clip, v1, ActionId::Cut, 30));
        assert!(!offered(&clip, v1, ActionId::Cut, 90));
        assert!(!offered(&clip, v1, ActionId::Cut, 200));
        // Regroup is the other way round: only where this clip meets another.
        assert!(offered(&clip, v1, ActionId::Regroup, 30));
        assert!(offered(&clip, v1, ActionId::Regroup, 90));
        assert!(!offered(&clip, v1, ActionId::Regroup, 60));
        // Detach is the clip's own business: nothing to take apart in one that
        // names no group, and whether the group still has another half is the
        // engine's question. Group is offered on every clip, for that reason.
        assert!(!offered(&clip, v1, ActionId::Detach, 0));
        let grouped = Clip {
            link: Some(3),
            ..clip
        };
        assert!(offered(&grouped, v1, ActionId::Detach, 60));
        assert!(offered(&clip, a1, ActionId::Group, 0));
        // The equalizer is the one item the *lane* decides: it filters samples,
        // and a video clip has none of its own. Never the playhead's business.
        assert!(offered(&clip, a1, ActionId::Equalizer, 0));
        assert!(offered(&clip, a1, ActionId::Equalizer, 60));
        assert!(!offered(&clip, v1, ActionId::Equalizer, 60));
        // The rest act on the clip that was clicked, so they always mean
        // something -- the engine words its own refusals.
        for action in [ActionId::Delete, ActionId::Lift, ActionId::ToggleMute] {
            assert!(offered(&clip, v1, action, 0));
            assert!(offered(&clip, a1, action, 60));
        }
        // Except the grade, which is a picture setting: offered on a video
        // clip wherever the playhead is, dimmed on a waveform.
        assert!(offered(&clip, v1, ActionId::Color, 0));
        assert!(!offered(&clip, a1, ActionId::Color, 60));
        // The two kinds of no, which is what the row is told apart by: a grade
        // on a waveform is a *class* answer -- an audio clip has no picture and
        // never will, so the menu leaves the item out entirely -- where a cut at
        // the clip's edge is this moment's answer and the next click of the
        // playhead changes it, so that one is drawn, dimmed, and says why.
        //
        // What the menu draws for a clip of each kind, in order: the render
        // loop's `continue`, as a list.
        let menu = |lane, image, playhead| {
            MENU_ITEMS
                .into_iter()
                .filter(|&action| {
                    enable(
                        action,
                        Ctx {
                            clip: Some((clip, lane)),
                            image,
                            playhead,
                            timeline: true,
                            ..Ctx::default()
                        },
                    )
                    .listed()
                })
                .collect::<Vec<_>>()
        };
        // Sound: no grade and no fit policy, and the equalizer that is the
        // whole reason an audio clip has a menu of its own.
        let sound = menu(a1, false, 60);
        assert!(!sound.contains(&ActionId::Color), "{sound:?}");
        assert!(!sound.contains(&ActionId::Fit), "{sound:?}");
        assert!(sound.contains(&ActionId::Equalizer));
        assert!(sound.contains(&ActionId::Silence));
        // Picture: the mirror of it. The sound of a take is the audio lane's,
        // clip for clip, so the equalizer is not this clip's business -- but the
        // silence scan is, because it opens on the half it is grouped with.
        let picture = menu(v1, false, 60);
        assert!(!picture.contains(&ActionId::Equalizer), "{picture:?}");
        assert!(picture.contains(&ActionId::Color));
        assert!(picture.contains(&ActionId::Fit));
        assert!(picture.contains(&ActionId::Silence));
        // A still: picture with no sound anywhere, ever. Graded, fitted and
        // re-timed like any other clip (a speed reaches a still through the same
        // rewrite), scanned like none.
        let still = menu(v1, true, 60);
        assert!(!still.contains(&ActionId::Silence), "{still:?}");
        assert!(!still.contains(&ActionId::Equalizer), "{still:?}");
        assert!(still.contains(&ActionId::Color));
        assert!(still.contains(&ActionId::Fit));
        assert!(still.contains(&ActionId::Speed));
        // ...and the state refusals are on all three, dimmed rather than gone:
        // at 30 the playhead is on this clip's head, where a cut has nothing to
        // split off -- a row the next click of the playhead lights.
        for rows in [menu(a1, false, 30), menu(v1, false, 30), menu(v1, true, 30)] {
            assert!(rows.contains(&ActionId::Cut), "{rows:?}");
            assert!(rows.contains(&ActionId::Detach), "{rows:?}");
        }
        // The actions card is the other half of the rule: it lists the whole
        // registry, so a class refusal is dimmed there with its reason and never
        // dropped -- an action missing from the one surface that lists
        // everything would read as an action that does not exist.
        let listed: Vec<ActionId> = keys_rows()
            .into_iter()
            .filter_map(|r| match r {
                KeyRow::Act(action) => Some(action),
                _ => None,
            })
            .collect();
        for (lane, action) in [
            (a1, ActionId::Color),
            (a1, ActionId::Fit),
            (v1, ActionId::Equalizer),
        ] {
            assert!(matches!(on(&clip, lane, action, 60), Enable::Hidden(_)));
            assert!(listed.contains(&action), "{action:?} left the actions card");
            assert!(on(&clip, lane, action, 60).why().is_some());
        }
        assert!(matches!(on(&clip, v1, ActionId::Cut, 30), Enable::No(_)));
        assert!(matches!(on(&clip, v1, ActionId::Regroup, 60), Enable::No(_)));
        assert!(matches!(on(&clip, v1, ActionId::Detach, 0), Enable::No(_)));
        // Every refusal says something, and says it short enough to sit in the
        // menu's right-hand column beside a label -- the still's included, which
        // the card dims with while the menu leaves the row out.
        for action in MENU_ITEMS {
            for (lane, image) in [(v1, false), (a1, false), (v1, true)] {
                for playhead in [0, 30, 60, 90] {
                    let refusal = enable(
                        action,
                        Ctx {
                            clip: Some((clip, lane)),
                            image,
                            playhead,
                            timeline: true,
                            ..Ctx::default()
                        },
                    );
                    if let Some(why) = refusal.why() {
                        assert!(!why.is_empty() && why.len() <= 30, "{action:?}: {why:?}");
                    }
                }
            }
        }
        assert!(matches!(
            enable(
                ActionId::Silence,
                Ctx {
                    clip: Some((clip, v1)),
                    image: true,
                    playhead: 60,
                    timeline: true,
                    ..Ctx::default()
                }
            ),
            Enable::Hidden(_)
        ));
        // The editor as a whole, which is how the actions card asks: with no
        // timeline nothing is offered, an export leaves only its own cancel,
        // and the three that act on the marked clip say so when none is.
        let whole = |action, ctx| enable(action, ctx);
        assert_eq!(
            whole(ActionId::Play, Ctx::default()),
            Enable::No("no timeline open")
        );
        let live = Ctx {
            timeline: true,
            ..Ctx::default()
        };
        assert!(whole(ActionId::Play, live).yes());
        assert!(!whole(ActionId::Delete, live).yes());
        assert!(!whole(ActionId::Paste, live).yes());
        assert!(
            whole(
                ActionId::Paste,
                Ctx {
                    clipboard: true,
                    ..live
                }
            )
            .yes()
        );
        assert!(!whole(ActionId::CancelExport, live).yes());
        let busy = Ctx {
            exporting: true,
            ..live
        };
        assert!(whole(ActionId::CancelExport, busy).yes());
        for action in ActionId::ALL {
            assert_eq!(
                whole(action, busy).yes(),
                action == ActionId::CancelExport,
                "{action:?} while an export reads the edit list"
            );
        }
        // The playhead frame is the engine's own rule, boundary included.
        assert_eq!(frame_at(1.0, 30.), 30);
        assert_eq!(frame_at(0.0, 30.), 0);
        assert_eq!(frame_at(-1.0, 30.), 0);
        // Where it hangs: at the pointer when it fits, back inside when not.
        let viewport = size(px(800.), px(400.));
        assert_eq!(menu_at(point(px(10.), px(10.)), viewport, 150.), (10., 10.));
        assert_eq!(
            menu_at(point(px(700.), px(380.)), viewport, 150.),
            (800. - MENU_W, 250.)
        );
        // A window smaller than the menu loses its bottom, never its top.
        assert_eq!(
            menu_at(point(px(90.), px(40.)), size(px(100.), px(50.)), 150.),
            (0., 0.)
        );
        // Every item is an action the registry knows, so the menu and the keys
        // menu say the same thing about it -- and none of them is unreachable
        // by keyboard, which is what makes the hint column worth drawing.
        let keymap = Keymap::defaults();
        for action in MENU_ITEMS {
            assert!(ActionId::ALL.contains(&action), "{action:?} is not listed");
            assert_ne!(keymap.display(action), "unbound", "{action:?}");
        }
        // ...and the whole card still fits the 640x360 floor, however many items
        // it grows to: the list is what scrolls where the window is too short
        // for it, never the card that grows.
        let items = MENU_ITEMS.len() + 1; // Properties
        let floor = size(px(640.), px(360.));
        assert!(MENU_PAD * 2. + menu_rows_h(items, floor) <= 360., "too tall");
        assert!(
            menu_rows_h(items, floor) / MENU_ROW_H >= 12.,
            "too few items visible to scan on the smallest window"
        );
        assert_eq!(
            menu_at(point(px(0.), px(0.)), floor, {
                MENU_PAD * 2. + menu_rows_h(items, floor)
            }),
            (0., 0.)
        );
    }

    /// Both menus, at the two things they can be opened on, and the box each
    /// one is drawn in. Two rules, and the render obeys them by *calling* what
    /// this calls -- [`menu_items`] and [`row_items`] are the only lists either
    /// menu is built from, and [`menu_rows_h`] is the height each is both placed
    /// by and drawn to:
    ///
    /// 1. a row exists only where the oracle lists the action for the very thing
    ///    that was right-clicked, so an item can never offer a video action on a
    ///    waveform (the complaint this comes from) and a new action added to
    ///    `MENU_ITEMS` cannot appear where it does not apply;
    /// 2. the whole card is inside the window, wherever it was opened and
    ///    however long the list -- a menu drawn past the bottom edge is a menu
    ///    whose last items nobody can click.
    #[test]
    fn a_menu_offers_only_what_applies_and_is_drawn_inside_the_window() {
        use keymap::ActionId;
        let clip = Clip {
            start: 30,
            in_frame: 0,
            out_frame: 60,
            source: 0,
            link: Some(1),
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };
        let ctx = |lane, image| Ctx {
            clip: Some((clip, lane)),
            image,
            playhead: 60,
            timeline: true,
            ..Ctx::default()
        };
        // The oracle is the whole of what the menu draws: every item it lists is
        // one the oracle would list, and every item it leaves out is one the
        // oracle hides -- there is no third answer, and no hand-written list.
        for (lane, image) in [(Lane::V1, false), (Lane::A1, false), (Lane::V1, true)] {
            let ctx = ctx(lane, image);
            let rows = menu_items(ctx);
            for action in MENU_ITEMS {
                assert_eq!(
                    rows.contains(&action),
                    enable(action, ctx).listed(),
                    "{action:?} on {lane:?}, image={image}"
                );
            }
        }
        // Sound has no picture settings; picture has no equalizer of its own; a
        // still has no sound to scan. The user's own words: an audio clip must
        // not be offered what only a picture can be given.
        let sound = menu_items(ctx(Lane::A1, false));
        assert!(!sound.contains(&ActionId::Color), "{sound:?}");
        assert!(!sound.contains(&ActionId::Fit), "{sound:?}");
        assert!(sound.contains(&ActionId::Equalizer));
        let picture = menu_items(ctx(Lane::V1, false));
        assert!(!picture.contains(&ActionId::Equalizer), "{picture:?}");
        assert!(picture.contains(&ActionId::Color));
        assert!(!menu_items(ctx(Lane::V1, true)).contains(&ActionId::Silence));
        // The library menu is the same rule on the other panel: its items come
        // off `row_items` and nothing else, and the two that change the timeline
        // say why rather than being clicked and refused afterwards.
        let live = RowCtx {
            timeline: true,
            usable: true,
            ..RowCtx::default()
        };
        for ctx in [
            live,
            RowCtx {
                usable: false,
                ..live
            },
            RowCtx { placed: 2, ..live },
            RowCtx {
                exporting: true,
                ..live
            },
            RowCtx::default(),
        ] {
            let rows = row_items(ctx);
            for item in ROW_ITEMS {
                assert_eq!(rows.contains(&item), row_enable(item, ctx).listed());
            }
            // Whatever the state, the two that need neither timeline nor edit
            // list are offered: a file is always describable and findable.
            assert!(row_enable(RowItem::Reveal, ctx).yes());
            assert!(row_enable(RowItem::Properties, ctx).yes());
        }
        assert!(row_enable(RowItem::Add, live).yes());
        assert!(
            !row_enable(
                RowItem::Add,
                RowCtx {
                    usable: false,
                    ..live
                }
            )
            .yes(),
            "a file that cannot join this timeline is not an Add anybody can ask for"
        );
        assert!(!row_enable(RowItem::Remove, RowCtx { placed: 1, ..live }).yes());
        assert!(!row_enable(RowItem::Add, RowCtx::default()).yes());
        // Every refusal is short enough to sit in the hint column beside its
        // label, the clip menu's rule.
        for item in ROW_ITEMS {
            for ctx in [live, RowCtx::default()] {
                if let Some(why) = row_enable(item, ctx).why() {
                    assert!(!why.is_empty() && why.len() <= 30, "{why:?}");
                }
            }
        }
        // ...and the box. Every window from the floor up, every corner of it,
        // and every list length either menu can have: the card is placed by
        // `MENU_PAD * 2 + menu_rows_h` and drawn to it, so this is the card.
        for viewport in [
            size(px(640.), px(360.)),
            size(px(800.), px(600.)),
            size(px(1280.), px(690.)),
            // Smaller than the floor the layout is sized for: it still may not
            // draw outside the window it has.
            size(px(320.), px(200.)),
        ] {
            for rows in 1..=MENU_ITEMS.len() + 1 {
                let h = MENU_PAD * 2. + menu_rows_h(rows, viewport);
                for at in [
                    point(px(0.), px(0.)),
                    point(px(10.), px(10.)),
                    // The click that started all this: low in the window, where
                    // the menu used to hang off the bottom edge.
                    point(px(0.), viewport.height - px(4.)),
                    point(viewport.width - px(4.), viewport.height - px(4.)),
                    point(viewport.width * 2., viewport.height * 2.),
                ] {
                    let (x, y) = menu_at(at, viewport, h);
                    assert!(x >= 0. && y >= 0., "{x},{y} outside {viewport:?}");
                    assert!(
                        x + MENU_W <= f32::from(viewport.width) + 0.01,
                        "{rows} rows at {at:?} hang off the right of {viewport:?}"
                    );
                    assert!(
                        y + h <= f32::from(viewport.height) + 0.01,
                        "{rows} rows at {at:?} hang off the bottom of {viewport:?}"
                    );
                }
            }
        }
        // On any window with the room, the whole list is drawn rather than
        // twelve rows of it and a scroll nobody is told about.
        let items = MENU_ITEMS.len() + 1;
        let real = size(px(1280.), px(690.));
        assert_eq!(menu_rows_h(items, real), items as f32 * MENU_ROW_H);
        assert!(menu_rows_h(items, size(px(640.), px(360.))) < items as f32 * MENU_ROW_H);
    }

    /// The other half of the keys menu's guarantee, and the audit this batch was
    /// asked for kept as a test: no action may be a stroke and nothing else.
    /// The actions card answers it for all of them at once -- its rows come off
    /// [`ActionId::ALL`], so a fortieth action is on it the moment it exists and
    /// there is no hand-written list here to fall behind.
    #[test]
    fn every_action_is_reachable_without_the_keyboard() {
        use keymap::ActionId;
        let source = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        let element = |id: &str| source.contains(&format!("\"{id}\""));
        let listed: Vec<ActionId> = keys_rows()
            .iter()
            .filter_map(|r| match r {
                KeyRow::Act(a) => Some(*a),
                _ => None,
            })
            .collect();
        for action in ActionId::ALL {
            assert_eq!(
                listed.iter().filter(|a| **a == action).count(),
                1,
                "{action:?} is reachable by keyboard only"
            );
        }
        // The snap has a door of its own as well as its row on the card: the
        // button beside the zoom, which says which way it is set as well as
        // setting it.
        assert!(element("snap"), "no snap button beside the zoom");
        // And the card is a door the pointer can open: the panel's own button.
        assert!(element("keys"), "no way to open the actions card");
        // The card-local strokes have the same rule, and each of them is a thing
        // on its card: the graph and its two buttons, the colour bars and their
        // reset, the speed bar and its presets, and the silence card's rows --
        // whose steppers are the pointer's only way to a threshold.
        for id in [
            "eq-graph",
            "eq-reset",
            "eq-spectrum",
            "color-bar",
            "color-reset",
            "speed-bar",
            "speed-preset",
            "silence-row",
            "silence-step",
            "mix-row",
            "mix-step",
            "silence-apply",
            "export-confirm",
        ] {
            assert!(element(id), "{id} is not on any card");
        }
        // ...and each one is named by the row that carries its stroke, so a
        // card-local key added to `FIXED` with nothing to click fails here
        // instead of being noticed by whoever tries to use the card without a
        // keyboard. `KeyRow::Fixed` is "shown and never offered", which used to
        // mean the twenty-eight rows below were the one part of this editor no
        // reachability test read.
        for fixed in keymap::FIXED.iter() {
            match fixed.reach {
                keymap::Reach::Click(id) => assert!(
                    element(id),
                    "{:?} ({}) points at {id}, which is on no card",
                    fixed.chord,
                    fixed.label
                ),
                // Nothing to click by decision, not by omission: getting out of
                // a card, and the hold that repeats what a drag already does.
                keymap::Reach::Gesture => {}
            }
        }
    }

    /// The other half of "getting out of a card is a `Reach::Gesture`": the
    /// gesture has to exist. Every card's scrim closes it on a press, so a hand
    /// that never touches the keyboard can shut every one of them -- for seven
    /// cards `esc` was the only exit, which is the same complaint as an action
    /// reachable by stroke alone, said about the way out instead of the way in.
    ///
    /// Read off [`Player::modal`] rather than off a list written here: a card
    /// counted there and not closed by [`Player::close_card`] fails this test
    /// the day it is added, which is the only way the two stay in step.
    #[test]
    fn every_card_closes_without_the_keyboard() {
        let source = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        // A fn's source, up to the next one: enough of a body to scan.
        let body = |name: &str| -> &str {
            let at = source
                .find(&format!("fn {name}("))
                .unwrap_or_else(|| panic!("no fn {name} in main.rs"));
            let rest = &source[at..];
            &rest[..rest[1..].find("\n    fn ").map_or(rest.len(), |e| e + 1)]
        };
        for card in [
            "keys_overlay",
            "export_card",
            "eq_card",
            "color_card",
            "speed_card",
            "mix_card",
            "silence_card",
        ] {
            let src = body(card);
            assert!(
                src.contains("this.close_card()"),
                "{card}'s scrim swallows the press without closing the card: \
                 escape is the only way out of it"
            );
            assert!(
                src.contains("MouseButton::Left, swallow"),
                "{card}'s body does not swallow its own presses, so a press on \
                 one of its controls would close the card under itself"
            );
        }
        // ...and every state that makes the window modal is a state that press
        // clears. `exporting()` is not one of them: a running export is a job
        // with a cancel button, not a card with a scrim.
        let close = body("close_card");
        for field in body("modal").split("self.").skip(1).filter_map(|rest| {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!rest[name.len()..].starts_with('(')).then_some(name)
        }) {
            assert!(
                close.contains(&format!("self.{field}")),
                "{field} makes the window modal and `close_card` leaves it set: \
                 that card cannot be closed by pointer"
            );
        }
    }

    /// A clip too short to hit is still a clip: at a far zoom-out its box is
    /// drawn [`HIT_MIN`] wide rather than the fraction of a pixel it is worth,
    /// because a box nobody can put a pointer on cannot be selected, dragged,
    /// trimmed or given a menu -- and an invisible unselectable take is strictly
    /// worse than one drawn a few pixels too wide.
    #[test]
    fn a_clip_box_is_never_narrower_than_its_target() {
        // Two seconds on a bed showing an hour: a fifth of a pixel.
        let far = Scale {
            pps: 0.1,
            start: 0.,
        };
        assert!(far.width_px(2.) < 1., "the fixture is not zoomed out enough");
        assert_eq!(clip_width(far.width_px(2.)), HIT_MIN);
        // Zero is the floor's own case: a clip trimmed to nothing, and the
        // width a lane draws before its first frame is measured.
        assert_eq!(clip_width(0.), HIT_MIN);
        // What is already wide enough keeps its own width, to the pixel: the
        // floor is a floor and never a resize.
        let near = Scale::default();
        assert_eq!(clip_width(near.width_px(5.)), near.width_px(5.));
        // ...and the padding is not trimmable: the strips are asked of the
        // clip's own width, so a box drawn wider than its length keeps all of
        // that box as a body to select and drag by ([`trims`]).
        assert!(!trims(far.width_px(2.)));
        assert!(clip_width(far.width_px(2.)) >= HIT_MIN);
    }

    /// What the Detach and Group items do to a real timeline: a music video's
    /// sound comes off its picture, Delete on the loose half takes that half
    /// only, undo puts both back, and Group makes the two one take again --
    /// whole-take delete and all.
    #[test]
    fn a_detached_half_is_removed_alone_and_groups_again() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        assert!(whole_take(&session, Lane::V1, 0), "one take to start with");
        assert!(whole_take(&session, Lane::A1, 0));
        let frames = session.lane_clips(Lane::V1)[0].len();

        // Detach audio: neither half is a whole take any more, so Delete on
        // either leaves the other exactly where it is.
        assert!(session.ungroup(Lane::V1, 0));
        assert!(
            !whole_take(&session, Lane::A1, 0),
            "the sound is a half now"
        );
        assert!(!whole_take(&session, Lane::V1, 0), "and so is the picture");
        assert!(session.lift_clip(Lane::A1, 0));
        assert!(session.lane_clips(Lane::A1).is_empty(), "the sound went");
        assert_eq!(session.lane_clips(Lane::V1).len(), 1, "the picture stayed");
        assert_eq!(session.lane_clips(Lane::V1)[0].len(), frames, "untrimmed");

        // One undo per edit, the removal then the detach.
        assert!(session.undo());
        assert_eq!(session.lane_clips(Lane::A1).len(), 1, "the sound is back");
        assert!(session.undo());
        assert!(whole_take(&session, Lane::A1, 0), "one take again");

        // Group: the partner is the clip covering these very frames on the
        // other track, which is what the item hands the engine.
        assert!(session.ungroup(Lane::V1, 0));
        assert_eq!(span_partner(&session, Lane::V1, 0), Some((Lane::A1, 0)));
        session
            .group(Lane::V1, 0, Lane::A1, 0)
            .expect("both halves still cover the same frames");
        assert!(
            whole_take(&session, Lane::A1, 0),
            "a take that ripples again"
        );
        assert_eq!(
            span_partner(&session, Lane::V1, 0),
            None,
            "and nothing left on another track to group with"
        );
    }

    /// Which clip Group reaches when more than one covers the span: the sound,
    /// whatever order the lanes are stored in. A project file may hold them in
    /// any order -- a video layer *before* the audio lane among them -- and
    /// "group this" means the other half of the take, never the layer above it.
    #[test]
    fn group_reaches_the_sound_before_a_video_layer_over_it() {
        use engine::project::LaneKind;

        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        let v2 = session.add_lane(LaneKind::Video);
        let path = session.sources()[0].path.clone();
        assert!(
            session
                .place_stream_at(0.0, &path, 0, Some(v2))
                .expect("its own file is on this timeline"),
            "a layer covering the same frames as the take"
        );

        // Saved and loaded back with the lanes in the order a hand-written
        // project may hold them: the sound last, behind the layer.
        let dir = engine::scratch::Scratch::dir("ve_group");
        let file = dir.join("lanes.edith");
        session.save_project(&file).expect("save the project");
        let text = std::fs::read_to_string(&file).expect("read it back");
        let (sound, rest): (Vec<&str>, Vec<&str>) =
            text.lines().partition(|l| l.starts_with("audio "));
        std::fs::write(
            &file,
            format!("{}\n{}\n", rest.join("\n"), sound.join("\n")),
        )
        .expect("write the reordered project");
        let mut session = PlaybackSession::open_project(&file).expect("it loads as it stands");
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            session.lanes(),
            vec![Lane::V1, v2, Lane::A1],
            "the sound is the last lane there"
        );

        // Detached, so Group has a choice to get wrong: the layer covers these
        // frames too, and it is the lane the walk meets first.
        assert!(session.ungroup(Lane::V1, 0));
        // Group on the picture reaches the sound, not that layer.
        assert_eq!(span_partner(&session, Lane::V1, 0), Some((Lane::A1, 0)));
        session
            .group(Lane::V1, 0, Lane::A1, 0)
            .expect("the two halves cover the same frames");
        // ...and a lane of its own kind is still groupable, once the sound is
        // spoken for: two video lanes may be one take.
        assert_eq!(
            span_partner(&session, Lane::V1, 0),
            Some((v2, 0)),
            "the layer is what is left to group with"
        );
    }

    /// The refusal path, end to end against the real files: an incompatible
    /// import changes nothing, and the library mirrors `sources()` 1:1, so a
    /// refused file cannot leave a row behind.
    #[test]
    fn a_refused_import_leaves_no_row_and_an_accepted_one_is_whole() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        // Silent like the engine suite: this opens the real device.
        session.set_gain(0.0);
        assert_eq!(session.sources().len(), 1);
        // 640x360 joins now (the project canvas places it), and so does a file
        // with no sound (it plays silence over its span). What is left is one
        // output device: a mono track cannot join a stereo timeline.
        let refusal = session
            .import(&asset("test_ac3.mp4"))
            .expect_err("a mono track must not join a stereo timeline")
            .to_string();
        assert!(refusal.contains("audio"), "refusal must name it: {refusal}");
        assert_eq!(session.sources().len(), 1, "a refusal added a row");
        // An accepted one does add a row, and it reads as the whole file: 4 s
        // at 30 fps, its own length and not one taken off the lanes.
        session.import(&asset("test_av2.mp4")).expect("av2 matches");
        assert_eq!(session.sources().len(), 2);
        let second = session.sources()[1].path.clone();
        assert_eq!(session.file_frames(&second), 120);
        assert_eq!(timecode(120. / 30., 30.), "00:00:04:00");
    }

    /// What the Add button and a dropped row both do, minus the pointer: the
    /// clip [`Player::insert_source`] builds, put in where the playhead is. One
    /// call, so what a drop does cannot drift from what the button does.
    #[test]
    fn adding_a_row_drops_the_whole_source_in_at_the_playhead() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        session.import(&asset("test_av2.mp4")).expect("av2 matches");
        // In the library only: the 5 s the fixture is, still.
        assert_eq!(session.timeline_duration(), 5.0);
        // Two seconds in, which is inside the first take: the insert splits it.
        session.seek(2.0);
        let second = session.sources()[1].path.clone();
        let frames = session.file_frames(&second);
        // Through the engine door `insert_source` uses, with the row's own
        // stream: the button, the drop and this are one call.
        assert!(
            session
                .place_stream_at(2.0, &second, 0, None)
                .expect("av2 is already on the timeline")
        );
        // The whole of source 1 went in and nothing was painted over: the
        // timeline is longer by exactly that file.
        assert_eq!(session.timeline_duration(), 9.0);
        let (video, audio) = (session.lane_clips(Lane::V1), session.lane_clips(Lane::A1));
        // One take, not a video clip with no sound under it: both lanes hold
        // the same clip at the same place, in the same group.
        let at = |lane: &[Clip]| {
            *lane
                .iter()
                .find(|c| c.start == 60)
                .expect("inserted at 2 s")
        };
        assert_eq!(at(video), at(audio));
        assert_eq!(at(video).source, 1);
        assert_eq!(at(video).len(), frames);
        assert!(at(video).link.is_some());
        assert_eq!(video.len(), audio.len());

        // The same door with a *second audio stream* of a file already there:
        // a new source entry, the same picture, and the row that was dragged
        // is what plays. This is the whole user-facing point of the slice.
        let mut session =
            PlaybackSession::open(asset("test_multilang.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        let path = session.sources()[0].path.clone();
        let frames = session.file_frames(&path);
        let end = session.timeline_duration();
        assert!(
            session
                .place_stream_at(end, &path, 1, None)
                .expect("the French track shares the timeline's parameters")
        );
        assert_eq!(session.sources()[1].audio_stream, 1);
        assert_eq!(session.timeline_duration(), end * 2.0);
        // Both rows are the same file, so both rows are that file's length --
        // the second one before any clip of its own existed.
        assert_eq!(session.file_frames(&path), frames);
    }

    /// The drop a hand actually makes: a library row let go on the *empty* bed
    /// past the last clip, which is most of the bed on any timeline shorter
    /// than the window. The whole chain the release runs, minus gpui's own
    /// pointer read -- [`Player::place_frame`]'s [`landing`], the frame back
    /// through the rate as [`Player::insert_source`] hands it over, and the one
    /// engine door a row goes through -- and the head lands on the frame the
    /// ghost was drawn on, black in front of it. It used to be swallowed by the
    /// clipboard's clamp inside `Project::paste` and appended after the last
    /// clip wherever it was let go, which is the bug this pins.
    #[test]
    fn a_row_dropped_on_the_open_bed_lands_under_the_pointer() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        session.import(&asset("test_av2.mp4")).expect("av2 matches");
        let second = session.sources()[1].path.clone();
        let fps = session.meta().frame_rate;
        assert_eq!(session.timeline_duration(), 5.0);
        // 5 s of timeline on a bed 30 s wide: the open bed past the last clip
        // is five sixths of it.
        let bed: Bounds<Pixels> = Bounds {
            origin: point(px(12.), px(400.)),
            size: size(px(600.), px(LANE_H)),
        };
        let scale = Scale {
            pps: 20.,
            start: 0.,
        };

        // What `Player::place_frame` answers for a pointer 200 px along the
        // bed: ten seconds in, five seconds past the end of the timeline, and
        // no mark anywhere near enough to pull it back.
        let clips = [session.lane_clips(Lane::V1), session.lane_clips(Lane::A1)];
        let marks = snap_marks(&clips, None, frame_at(session.now(), fps));
        let under = frame_at(scale.time_at(px_along(px(212.), bed)), fps);
        let (at, cue) = landing(under, 0, 0, true, scale.snap_frames(fps), &marks);
        assert_eq!((at, cue), (300, None), "the pointer is on frame 300");

        // ...and what the release does with it: the frame back through the same
        // rate every box is drawn at, into the door the Add button uses too.
        assert!(
            session
                .place_stream_at(f64::from(at) / fps, &second, 0, None)
                .expect("av2 is already on this timeline")
        );
        let head = |lane| {
            session
                .lane_clips(lane)
                .last()
                .copied()
                .expect("the dropped clip")
        };
        assert_eq!(
            head(Lane::V1).start,
            at,
            "the drop landed somewhere other than under the pointer"
        );
        assert_eq!(head(Lane::A1).start, at, "...and its sound with it");
        assert!(head(Lane::V1).link.is_some(), "one grouped take");
        assert_eq!(head(Lane::V1).link, head(Lane::A1).link);
        // The bed in front of it stays black: nothing was stretched to reach
        // it, and the 4 s file is the whole of what was added.
        assert_eq!(session.timeline_duration(), 14.0);
    }

    /// Remove from library, through the door the menu item uses
    /// ([`Player::remove_source`] calls exactly this): refused by name while
    /// clips play from the file, and once they do not the row leaves the list.
    #[test]
    fn removing_a_row_is_refused_while_it_plays_and_takes_the_row_away() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        // Imported, then dragged onto the end: an import alone fills the
        // library, and a row with no clip is removable without any refusal to
        // test. This one has to have a take on the timeline.
        session.import(&asset("test_av2.mp4")).expect("av2 matches");
        let second = session.sources()[1].path.clone();
        let end = session.timeline_duration();
        assert!(
            session
                .place_stream_at(end, &second, 0, None)
                .expect("a file just imported is on this timeline")
        );
        let streams = HashMap::new();
        let rows = |session: &PlaybackSession| {
            library_rows(session.sources(), &streams, &HashMap::new(), None, |_| 0).len()
        };
        assert_eq!(rows(&session), 2, "a row per source");

        let refusal = session
            .remove_source(&second, 0)
            .expect_err("its take is still on the timeline")
            .to_string();
        assert!(refusal.contains("still plays"), "{refusal}");
        assert!(
            refusal.contains("V1 (1 clip)") && refusal.contains("A1 (1 clip)"),
            "the refusal names the lanes to clear: {refusal}"
        );
        assert_eq!(rows(&session), 2, "and the row is still there");

        // Delete that take -- the whole group, from either half -- and the file
        // can go. The list is what says so.
        let last = session.lane_clips(Lane::V1).len() - 1;
        assert!(session.delete_clip(Lane::V1, last));
        session
            .remove_source(&second, 0)
            .expect("nothing plays av2 any more");
        assert_eq!(rows(&session), 1, "the row went with it");
        assert_eq!(session.sources().len(), 1);
        // The one file left is held to the same rule and no other: its own take
        // is still on the lanes, so it stays...
        let first = session.sources()[0].path.clone();
        let refusal = session
            .remove_source(&first, 0)
            .expect_err("its take is still on the timeline")
            .to_string();
        assert!(refusal.contains("still plays"), "{refusal}");
        // ...and once nothing plays it, the *last* row goes too. What is left
        // is the empty library `Player::close_session` turns back into the
        // window the editor launches as -- the user-reported bug was that this
        // very removal was refused, leaving a row that could never be taken
        // out.
        assert!(session.delete_clip(Lane::V1, 0));
        session
            .remove_source(&first, 0)
            .expect("the only row goes like any other");
        assert_eq!(rows(&session), 0, "an empty library");
        assert!(session.sources().is_empty());
        // And it is still a session: silent, empty, and asked for its length
        // rather than panicking on a source list that is not there.
        assert_eq!(session.timeline_duration(), 0.0);
        assert!(
            session.save_project(&asset("nothing.edith")).is_err(),
            "a project naming no file could never be opened again, so it is not written"
        );
        // A row this timeline never had is refused, not panicked on.
        assert!(session.remove_source(&second, 0).is_err());
    }

    /// The clipboard after a library removal. A copied clip names its file by
    /// index and a removal renumbers the list, so this is the difference
    /// between pasting the take that was copied and pasting **another file**
    /// over the same range ([`clipboard_after_remove`], called by
    /// [`Player::remove_source`]).
    #[test]
    fn a_copied_clip_is_renumbered_or_dropped_when_a_row_leaves_the_library() {
        let clip = |source: usize| Clip {
            start: 0,
            in_frame: 0,
            out_frame: 30,
            source,
            link: None,
            eq: None,
            color: None,
            fit: Default::default(),
            speed: Default::default(),
        };
        // Copied from source 2, source 0 removed: the same file is source 1 now.
        assert_eq!(
            clipboard_after_remove(Some(clip(2)), 0).map(|c| c.source),
            Some(1),
            "the clipboard follows its file down the list"
        );
        // Copied from a source *before* the one that went: untouched.
        assert_eq!(
            clipboard_after_remove(Some(clip(0)), 2).map(|c| c.source),
            Some(0)
        );
        // Copied from the row that was just removed: there is nothing left to
        // paste, and pasting the next file along would be a lie.
        assert!(clipboard_after_remove(Some(clip(1)), 1).is_none());
        assert!(clipboard_after_remove(None, 0).is_none());
    }

    /// The trim-a-clip path through the doors the edge drag uses:
    /// [`Player::trim_to`] clamps the pointer with `trim_room` and
    /// [`Player::commit_trim`] writes it with `trim_clip`. The clip plays less
    /// of its file, the sound linked to it follows, the head trim moves the
    /// in-point, and one undo takes a whole gesture back.
    ///
    /// The routing *to* these doors -- the 6 px edge strip claiming the press
    /// the clip's own body-drag would otherwise take -- is gpui hitbox
    /// behaviour (`occlude`) and is not reachable without a window.
    #[test]
    fn a_clip_trimmed_by_its_edge_plays_less_of_its_file() {
        use engine::project::Edge;

        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        let whole = session.lane_clips(Lane::V1)[0];
        let full = session.timeline_duration();
        assert_eq!(
            session.trim_room(Lane::V1, 0, Edge::End),
            Some((whole.start + 1, whole.end())),
            "the file's own last frame is how far the tail goes"
        );
        assert!(
            !session.trim_clip(Lane::V1, 0, Edge::End, 9_999),
            "it already plays all of it"
        );

        // Pulled in by a third: the timeline ends earlier and the sound with it.
        let shorter = whole.end() - whole.len() / 3;
        assert!(session.trim_clip(Lane::V1, 0, Edge::End, shorter));
        assert_eq!(session.lane_clips(Lane::V1)[0].end(), shorter);
        assert_eq!(
            session.lane_clips(Lane::A1)[0].end(),
            shorter,
            "the linked sound was trimmed with the picture"
        );
        assert!(session.timeline_duration() < full, "and plays out earlier");

        // ...and dragged back out, as far as the file goes and no further.
        assert!(session.trim_clip(Lane::V1, 0, Edge::End, 9_999));
        assert_eq!(
            session.lane_clips(Lane::V1)[0],
            whole,
            "the whole take back"
        );

        // The head takes the in-point with it, so what plays at the new start
        // is source frame 10 rather than source frame 0.
        assert!(session.trim_clip(Lane::V1, 0, Edge::Start, 10));
        let head = session.lane_clips(Lane::V1)[0];
        assert_eq!((head.start, head.in_frame), (10, 10));
        assert_eq!(session.lane_clips(Lane::A1)[0].in_frame, 10, "sound too");
        assert_eq!(
            session.trim_room(Lane::V1, 0, Edge::Start).map(|r| r.0),
            Some(0),
            "and it may be pulled back out to the file's first frame"
        );

        assert!(session.undo(), "one step for the whole drag");
        assert_eq!(session.lane_clips(Lane::V1)[0], whole);
    }

    /// The move-a-clip-between-tracks path through the door the drop uses
    /// ([`Player::move_clip`] calls exactly this): the clip changes row, the
    /// *picture* comes from the new row afterwards -- which is what "it plays
    /// from there" means -- and one undo puts it back.
    #[test]
    fn a_clip_dragged_onto_another_track_plays_from_it() {
        use engine::project::LaneKind;

        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        let v2 = session.add_lane(LaneKind::Video);
        assert_eq!(session.video_clip_at(0.0), Some((Lane::V1, 0)));

        assert!(session.move_clip_to(Lane::V1, 0, v2, 0), "V1 -> V2");
        assert!(session.lane_clips(Lane::V1).is_empty(), "it left V1");
        assert_eq!(session.lane_clips(v2).len(), 1, "and landed on V2");
        assert_eq!(
            session.video_clip_at(0.0),
            Some((v2, 0)),
            "the picture now comes from V2"
        );
        assert_eq!(session.lane_clips(Lane::A1).len(), 1, "its sound stayed");

        // Dropped on a lane of the other kind it is refused and nothing moves --
        // the notice the front-end shows for it says which kind of lane to use.
        // (The other refusal, landing on another clip, is the engine's own test
        // `move_clip_keeps_the_frames_and_refuses_the_rest`.)
        assert!(!session.move_clip_to(v2, 0, Lane::A1, 0), "picture on A1");
        assert_eq!(session.lane_clips(v2).len(), 1, "and it stayed on V2");
        assert!(
            session.move_clip_to(v2, 0, Lane::V1, 0),
            "dragged back down"
        );

        // One undo per move, and each is a single step.
        assert!(session.undo(), "the drag back");
        assert_eq!(session.video_clip_at(0.0), Some((v2, 0)));
        assert!(session.undo(), "the drag up");
        assert_eq!(session.video_clip_at(0.0), Some((Lane::V1, 0)));
        assert!(session.lane_clips(v2).is_empty());
    }

    /// The add-a-track path end to end through the doors the buttons and the
    /// drop use: `+ V` adds a row, a library row let go over it lands there and
    /// nowhere else, Delete on it leaves the lanes under it where they are, and
    /// undo takes the whole thing back one step at a time.
    #[test]
    fn a_track_can_be_added_dropped_on_edited_and_taken_back() {
        use engine::project::LaneKind;

        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        assert_eq!(session.lanes(), vec![Lane::V1, Lane::A1]);
        // What the `+ V` button asks for, and what the row it draws is called.
        let v2 = session.add_lane(LaneKind::Video);
        assert_eq!(v2.label(), "V2");
        assert_eq!(session.lanes(), vec![Lane::V1, Lane::A1, v2]);
        assert!(session.lane_clips(v2).is_empty());
        // A library row let go over that row: the same door the Add button
        // uses, told which lane it was let go over.
        let path = session.sources()[0].path.clone();
        assert!(
            session
                .place_stream_at(1.0, &path, 0, Some(v2))
                .expect("its own file is on this timeline")
        );
        assert_eq!(session.lane_clips(v2).len(), 1, "the drop landed on V2");
        assert_eq!(session.lane_clips(v2)[0].start, 30, "at the playhead");
        // And nowhere else: the first pair is exactly as it was, one take each.
        assert_eq!(session.lane_clips(Lane::V1).len(), 1);
        assert_eq!(session.lane_clips(Lane::A1).len(), 1);
        // Delete on the layer is a lift: it is laid over the timeline, so
        // closing a hole under it would drag the take beneath out of step with
        // it. The take on the first pair is still a take, and still ripples.
        assert!(!whole_take(&session, v2, 0));
        assert!(whole_take(&session, Lane::V1, 0));
        assert!(whole_take(&session, Lane::A1, 0));
        assert!(session.lift_clip(v2, 0));
        assert!(session.lane_clips(v2).is_empty());
        assert_eq!(session.lane_clips(Lane::V1).len(), 1, "V1 stayed put");

        // A second audio lane used to be what an mp4 export could not write --
        // it copied one AAC track and a mix is not a copy. It mixes now
        // (`export::copy_audio`), so no row is greyed by a count of lanes.
        assert_eq!(format_refusal(&session, Format::Mp4), None);
        let a2 = session.add_lane(LaneKind::Audio);
        assert_eq!(a2.label(), "A2");
        assert!(
            session
                .place_stream_at(0.0, &path, 0, Some(a2))
                .expect("its own file is on this timeline")
        );
        for format in [Format::Mp4, Format::Av1, Format::Av1Mp4] {
            assert_eq!(
                format_refusal(&session, format),
                None,
                "two audio lanes are a mix, not a refusal"
            );
        }

        // Undo, one edit at a time and backwards: the drop on A2, the lane A2
        // itself, the lift, the drop on V2, and last the lane V2 -- an added
        // track is one step like every other edit.
        for lanes in [4, 3, 3, 3, 2] {
            assert!(session.undo());
            assert_eq!(session.lanes().len(), lanes);
        }
        assert_eq!(session.lanes(), vec![Lane::V1, Lane::A1]);
        assert_eq!(session.lane_clips(Lane::V1).len(), 1);
        assert_eq!(format_refusal(&session, Format::Mp4), None);
    }

    #[test]
    fn a_source_is_only_ever_asked_for_its_peaks_once() {
        let (a, b) = (asset("test_av.mp4"), asset("test_av2.mp4"));
        // Two files, and two streams of the first: three envelopes, because
        // one file's two audio tracks are two different waveforms.
        let sources = [
            Source {
                path: a.clone(),
                audio_stream: 0,
            },
            Source {
                path: a.clone(),
                audio_stream: 1,
            },
            Source {
                path: b.clone(),
                audio_stream: 0,
            },
        ];
        let keys = |s: &[Source]| {
            s.iter()
                .map(|s| (s.path.clone(), s.audio_stream))
                .collect::<Vec<_>>()
        };
        let mut waves: HashMap<(PathBuf, usize), Wave> = HashMap::new();
        assert_eq!(unseen_sources(&sources, &waves), keys(&sources));
        // The entry goes in when the decode *starts*, so the sixty repaints a
        // second that happen while it runs must not start it again -- which is
        // what this asserts about a key whose value is not an answer yet.
        waves.insert((a.clone(), 0), Wave::Loading);
        assert_eq!(
            unseen_sources(&sources, &waves),
            keys(&sources[1..]),
            "the file's other stream is a key of its own"
        );
        // A file with no audio is an answer like any other: never re-asked.
        waves.insert((a, 1), Wave::Silent);
        waves.insert((b.clone(), 0), Wave::Silent);
        assert!(unseen_sources(&sources, &waves).is_empty());
        // The stream probe is per *file*: the two entries of `a` ask once.
        let mut streams: HashMap<PathBuf, Vec<StreamInfo>> = HashMap::new();
        assert_eq!(
            unseen_paths(&sources, &streams),
            vec![asset("test_av.mp4"), b]
        );
        streams.insert(asset("test_av.mp4"), Vec::new());
        assert_eq!(unseen_paths(&sources, &streams).len(), 1);
    }

    #[test]
    fn the_add_button_is_dead_unless_it_would_do_something() {
        let picked = (PathBuf::from("/m/0.mp4"), 0);
        assert!(can_add(Some(&picked), true, false));
        // Nothing picked, nothing to put it on, or an export reading the very
        // edit list this would change.
        assert!(!can_add(None, true, false));
        assert!(!can_add(Some(&picked), false, false));
        assert!(!can_add(Some(&picked), true, true));
    }

    #[test]
    fn the_media_column_never_takes_the_picture_over() {
        for window in [360., 640., 1280., 1920., 3840.] {
            let w = super::library_w(window);
            // The whole point of the budget: the picture keeps the majority of
            // the row at every size a window can be.
            assert!(w <= window / 3., "{window}px window gave the list {w}px");
            assert!(w >= LIBRARY_MIN_W.min(window / 3.), "{window}px: {w}px");
            assert!(w <= LIBRARY_MAX_W);
        }
        // It yields: a narrower window gives the list less, never the same.
        assert!(super::library_w(640.) < super::library_w(1280.));
        // Rows are clickable, so WCAG 2.5.8 binds them like every other target,
        // and a name over a timecode has to fit inside one.
        assert!(ROW_H >= HIT_MIN);
        assert!(SWATCH_W < LIBRARY_MIN_W);
    }

    #[test]
    fn the_window_is_named_after_the_program_and_what_is_open() {
        assert_eq!(window_title("test_av.mp4"), "test_av.mp4 — edith");
        // An empty window is the program, not "no file open — edith".
        assert_eq!(window_title(NO_FILE), "edith");
    }

    #[test]
    fn frac_along_measures_from_the_elements_own_left_edge() {
        // A ruler inset by the panel's 12 px padding: window x is not bar x.
        let bar: Bounds<Pixels> = Bounds {
            origin: point(px(12.), px(400.)),
            size: size(px(200.), px(6.)),
        };
        assert_eq!(frac_along(px(12.), bar), 0.);
        assert_eq!(frac_along(px(112.), bar), 0.5);
        assert_eq!(frac_along(px(212.), bar), 1.);
        // Outside the bar (a click that slid off) clamps, never extrapolates.
        assert_eq!(frac_along(px(0.), bar), 0.);
        assert_eq!(frac_along(px(9999.), bar), 1.);
        // Never painted: no division by zero, no NaN reaching seek().
        assert_eq!(frac_along(px(50.), Bounds::default()), 0.);

        // The equalizer's axis is the other one, and reads the same way.
        let graph: Bounds<Pixels> = Bounds {
            origin: point(px(12.), px(60.)),
            size: size(px(296.), px(EQ_GRAPH_H)),
        };
        assert_eq!(frac_down(px(60.), graph), 0.);
        assert_eq!(frac_down(px(60. + EQ_GRAPH_H / 2.), graph), 0.5);
        assert_eq!(frac_down(px(9999.), graph), 1.);
        // Never painted reads as flat: an unpainted graph must not slam a band
        // to +12 dB on the first press.
        assert_eq!(frac_down(px(50.), Bounds::default()), 0.5);
    }

    /// What a drop reads: the frame under the pointer, through the same scale
    /// the boxes are drawn through. Zoomed in, the same pixel is a different
    /// frame -- which is the whole reason `Player::frame_under` goes through
    /// [`Scale`] rather than through the duration alone.
    #[test]
    fn a_drop_reads_the_frame_under_the_pointer_at_every_zoom() {
        // A 200 px bed inset by the panel's padding, 10 seconds at 30 fps.
        let bed: Bounds<Pixels> = Bounds {
            origin: point(px(12.), px(400.)),
            size: size(px(200.), px(6.)),
        };
        let fps = 30.;
        // The frame a pointer at window `x` names, exactly as `frame_under`
        // composes it.
        let under = |scale: Scale, x: f32| frame_at(scale.time_at(px_along(px(x), bed)), fps);

        // The whole 10 s timeline across the 200 px bed: 20 px to the second,
        // so halfway along is frame 150.
        let fit = Scale {
            pps: 20.,
            start: 0.,
        };
        assert_eq!(under(fit, 12.), 0);
        assert_eq!(under(fit, 112.), 150);
        assert_eq!(under(fit, 212.), 300);

        // Four times in, starting at second 5: the same middle pixel is now the
        // frame in the middle of seconds 5..7.5, and the left edge is not 0.
        let zoomed = Scale {
            pps: 80.,
            start: 5.,
        };
        assert_eq!(under(zoomed, 12.), 150);
        assert_eq!(under(zoomed, 112.), 187);
        assert_eq!(under(zoomed, 212.), 225);
        // ...and a pointer that slid off either end of the bed names an end of
        // the bed, never a pixel outside it.
        assert_eq!(under(zoomed, 0.), 150);
        assert_eq!(under(fit, 9999.), 300);
    }

    /// The snap: a clip let go a few frames off a neighbour's edge lands *on*
    /// it, by whichever of its own ends is nearer, and a clip let go in open bed
    /// stays exactly where the hand left it.
    #[test]
    fn a_dropped_clip_snaps_to_the_edges_worth_landing_on() {
        // A neighbour covering [100, 160) and the playhead at 300.
        let marks = [100, 160, 300, 0];
        // Head a frame short of the neighbour's tail: laid end to end with it.
        assert_eq!(snapped(158, 40, 4, &marks), 160);
        // Tail a frame into its head: pulled back so the two meet exactly.
        assert_eq!(snapped(62, 40, 4, &marks), 60);
        // The playhead is an edge like any other.
        assert_eq!(snapped(298, 40, 4, &marks), 300);
        // Outside the tolerance nothing moves -- a gap the hand meant to leave
        // is a gap.
        assert_eq!(snapped(150, 40, 4, &marks), 150);
        // No tolerance at all (zoomed right in, where a few pixels are worth
        // less than a frame) is no snap: single frames are placed by hand.
        assert_eq!(snapped(158, 40, 0, &marks), 158);
        // The nearer edge wins when two are in reach.
        assert_eq!(snapped(101, 40, 8, &marks), 100);
        // ...and a mark closer to the head than `len` cannot pull the clip to a
        // negative start.
        assert_eq!(snapped(2, 40, 4, &marks), 0);
    }

    /// The shadow a drag draws and the drop that commits are one answer: both
    /// ask [`landing`], so the box seen in flight is the box the release leaves
    /// behind. What this pins down is the composition around the snap -- the
    /// grab offset comes off *before* the magnet, or a clip taken by its tail
    /// would land a boxful late.
    #[test]
    fn a_ghost_and_a_drop_are_one_landing() {
        // A neighbour covering [100, 160), the playhead at 300, and a 40 frame
        // clip taken hold of 12 frames in.
        let marks = [100, 160, 300, 0];
        let (len, grab, tol) = (40, 12, 4);
        // Pointer at frame 170: the head is 12 frames behind it, at 158, which
        // is a frame short of the neighbour's tail -- so both the ghost and the
        // drop say 160, and the line stands on the mark that pulled it.
        assert_eq!(landing(170, grab, len, true, tol, &marks), (160, Some(160)));
        // Without the grab taken off first, the same pointer would land the
        // box at 170 and no mark would be in reach: the offset is not cosmetic.
        assert_eq!(landing(170, 0, len, true, tol, &marks), (170, None));
        // A library row carries no grab and no length the engine has measured,
        // which is how `Player::place_frame` asks: only its head lands, on the
        // playhead here.
        assert_eq!(landing(298, 0, 0, true, tol, &marks), (300, Some(300)));
        // The magnet off, ghost and drop agree on the raw frame and no line is
        // drawn -- the frame-by-frame placement the switch is for.
        assert_eq!(landing(170, grab, len, false, tol, &marks), (158, None));
        // A pointer nearer the bed's start than the hand is into the box cannot
        // pull a head below zero.
        assert_eq!(landing(3, grab, len, true, tol, &marks), (0, Some(0)));
    }

    /// Which lanes tint that shadow as refused: the two kinds of file a lane
    /// cannot hold, in the words the release would say them in -- one rule, so
    /// what is shown as impossible is exactly what is refused.
    #[test]
    fn a_lane_refuses_the_files_it_cannot_hold_before_the_release_says_so() {
        let (video, audio) = (Lane::V1, Lane::A1);
        let sound = Path::new("/media/take.mp3");
        let still = Path::new("/media/card.png");
        let movie = Path::new("/media/take.mp4");
        assert_eq!(
            lane_refuses(sound, video).as_deref(),
            Some("NOT ON V1 — take.mp3 has no picture; drop it on an audio lane")
        );
        assert_eq!(
            lane_refuses(still, audio).as_deref(),
            Some("NOT ON A1 — card.png is a still image; drop it on a video lane")
        );
        // ...and every lane a file *can* go on says nothing at all, which is a
        // ghost drawn in the file's own colour.
        assert_eq!(lane_refuses(sound, audio), None);
        assert_eq!(lane_refuses(still, video), None);
        assert_eq!(lane_refuses(movie, video), None);
        // A file with a picture is not refused by an audio lane here: the
        // engine takes its sound onto one, and the words for a video-only file
        // are its own.
        assert_eq!(lane_refuses(movie, audio), None);
    }

    /// Where those edges come from: every lane, not the one being dropped on --
    /// and never the clip in the hand or the half of it one track down.
    #[test]
    fn the_marks_are_every_lane_the_playhead_and_the_start() {
        let clip = |start: u32, frames: u32, link| Clip {
            start,
            in_frame: 0,
            out_frame: frames,
            source: 0,
            link,
            eq: None,
            color: None,
            fit: Default::default(),
            speed: Default::default(),
        };
        // A grouped take across two lanes at 100..160, and a lone one on the
        // audio lane at 400..430.
        let video = [clip(100, 60, Some(7))];
        let audio = [clip(100, 60, Some(7)), clip(400, 30, None)];
        let lanes: [&[Clip]; 2] = [&video, &audio];

        // Nothing in the hand: both lanes' edges, the playhead, and 0.
        let mut all = snap_marks(&lanes, None, 300);
        all.sort_unstable();
        assert_eq!(all, [0, 100, 100, 160, 160, 300, 400, 430]);

        // The video half in the hand: its own edges are gone, and so are its
        // group's on the other lane -- both boxes travel with the drag. The
        // lone audio clip is still a target, which is the whole point: a take
        // being carried on V1 lands flush with a sound on A1.
        let mut carried = snap_marks(&lanes, Some((0, 0)), 300);
        carried.sort_unstable();
        assert_eq!(carried, [0, 300, 400, 430]);

        // An index that names no clip skips nothing and still answers.
        assert_eq!(snap_marks(&[], Some((3, 9)), 0), [0, 0]);
    }

    /// The line the bed draws, and the switch that turns the whole thing off.
    #[test]
    fn the_snap_names_the_mark_it_landed_on_unless_it_is_switched_off() {
        let marks = [100, 160, 300, 0];
        // Pulled by the tail: the clip lands at 60 and the line stands on the
        // edge its *tail* met, 100 -- not on the head it happens to have.
        assert_eq!(snap_cue(true, 62, 40, 4, &marks), (60, Some(100)));
        // Pulled by the head: line and landing are the same frame.
        assert_eq!(snap_cue(true, 158, 40, 4, &marks), (160, Some(160)));
        // A trim carries no length, so only its own edge lands.
        assert_eq!(snap_cue(true, 298, 0, 4, &marks), (300, Some(300)));
        // Open bed: nothing moves and nothing is drawn.
        assert_eq!(snap_cue(true, 200, 40, 4, &marks), (200, None));
        // Switched off, a gesture that would have snapped lands raw and draws
        // no line -- the frame-by-frame placement the toggle is for.
        assert_eq!(snap_cue(false, 158, 40, 4, &marks), (158, None));
    }

    /// The card's rate row: every component the container states and no other,
    /// so a track the header is silent about is absent from the line rather
    /// than sitting in it as a zero.
    #[test]
    fn a_rate_row_says_only_what_the_container_stated() {
        use super::MediaBitrate;
        let all = MediaBitrate {
            total: Some(8_432_000),
            video: Some(7_918_000),
            audio: Some(128_000),
        };
        assert_eq!(
            bitrate_detail(Some(all), 1),
            "8.4 Mb/s · 7.9 video · 0.13 sound"
        );
        // A Matroska states no audio rate of its own: the sound is dropped from
        // the line, never drawn as "0".
        assert_eq!(
            bitrate_detail(
                Some(MediaBitrate {
                    audio: None,
                    ..all
                }),
                1
            ),
            "8.4 Mb/s · 7.9 video"
        );
        // A still, or anything that would not open: the probe answered, and the
        // answer is that nobody said.
        assert_eq!(
            bitrate_detail(Some(MediaBitrate::default()), 0),
            "not stated"
        );
        // Asked, not answered yet -- a 12 GB film's walk takes seconds.
        assert_eq!(bitrate_detail(None, 2), "…");
        // A dual-audio file: the number is the track that plays, and the line
        // says so rather than letting it stand for the AC-3 beside it.
        assert_eq!(
            bitrate_detail(Some(all), 2),
            "8.4 Mb/s · 7.9 video · 0.13 1 of 2"
        );
        // Every component of a tiny file is stated, and small: the line changes
        // unit, so not one of them reads as the zero it is not. In megabits
        // this file was "0.00 Mb/s · 0.00 video · 0.00 sound".
        assert_eq!(
            bitrate_detail(
                Some(MediaBitrate {
                    total: Some(4_998),
                    video: Some(113),
                    audio: Some(2_400),
                }),
                1
            ),
            "5.0 kb/s · 0.11 video · 2.4 sound"
        );
    }

    /// The invariant under the row above, over every rate a container can
    /// state: a component the file *does* state never renders as a zero. The
    /// probe leaves out what is unstated, so a "0.00" on this card would be it
    /// saying a track that plays is silent.
    #[test]
    fn a_stated_rate_never_renders_as_a_zero() {
        use super::MediaBitrate;
        // Every decade from 1 bit a second to a 100 Mb/s master, the rounding
        // edges of each unit switch, and every pair of them in one line: the
        // line's unit is picked off its smallest component, so the widest
        // spread is the one that could round the biggest away.
        let edges: Vec<u64> = (0..12)
            .flat_map(|e| {
                let decade = 10_u64.pow(e);
                [decade, decade * 4, decade * 5, decade * 9]
            })
            .chain([MB_FLOOR - 1, MB_FLOOR, 999_999, 4_998, 113, 2_400])
            .collect();
        for &small in &edges {
            for &big in &edges {
                let line = bitrate_detail(
                    Some(MediaBitrate {
                        total: Some(small.max(big)),
                        video: Some(big),
                        audio: Some(small),
                    }),
                    2,
                );
                for number in line
                    .split(" · ")
                    .filter_map(|part| part.split(' ').next())
                    .filter_map(|n| n.parse::<f64>().ok())
                {
                    assert!(number > 0., "{small}/{big} bits a second rendered as {line:?}");
                }
            }
        }
    }

    #[test]
    fn timecode_counts_frames_inside_the_second() {
        assert_eq!(timecode(0., 30.), "00:00:00:00");
        assert_eq!(timecode(-1., 30.), "00:00:00:00"); // clamped, never negative
        assert_eq!(timecode(4.9667, 30.), "00:00:04:29"); // last frame of a 5 s clip
        assert_eq!(timecode(5., 30.), "00:00:05:00");
        assert_eq!(timecode(3661.5, 30.), "01:01:01:15");
        // Rounding must not spill into a frame the second does not have.
        assert_eq!(timecode(1. - f64::EPSILON, 30.), "00:00:00:29");
        assert_eq!(timecode(0.999, 29.97), "00:00:00:29");
    }

    #[test]
    fn scrub_seeks_only_on_a_new_frame_and_only_every_100ms() {
        let (slow, fast) = (Duration::from_millis(100), Duration::from_millis(99));
        // Both halves must hold: a moved pointer that has not crossed a frame
        // boundary would reopen the decoder for the same picture.
        assert!(scrub_due(31, 30, slow));
        assert!(!scrub_due(30, 30, slow));
        assert!(!scrub_due(31, 30, fast));
        assert!(!scrub_due(30, 30, fast));
        // Exactly at the gap counts, and a long stall never blocks.
        assert!(scrub_due(1, 0, Duration::from_secs(9)));
    }

    #[test]
    fn export_lands_beside_the_source_and_never_on_it() {
        // The whole point: the export of an .mp4 is never that .mp4.
        assert_eq!(
            export_path("assets/test_baseline.mp4"),
            std::path::Path::new("assets/test_baseline.export.mp4")
        );
        // A second export of an export is still not its own source.
        assert_eq!(
            export_path("a.export.mp4"),
            std::path::Path::new("a.export.export.mp4")
        );
        // Extensionless and dotted-directory names keep the directory intact.
        assert_eq!(export_path("clip"), std::path::Path::new("clip.export.mp4"));
        assert_eq!(
            export_path("/v.1/clip.MP4"),
            std::path::Path::new("/v.1/clip.export.mp4")
        );
    }

    #[test]
    fn the_first_save_lands_beside_the_media() {
        assert_eq!(
            project_path("assets/test_av.mp4"),
            std::path::Path::new("assets/test_av.edith")
        );
        // The same rule an export follows: only the last extension moves.
        assert_eq!(
            project_path("a.export.mp4"),
            std::path::Path::new("a.export.edith")
        );
        assert_eq!(project_path("clip"), std::path::Path::new("clip.edith"));
        // Saving a loaded project writes the file it came from, not a second.
        assert_eq!(project_path("a.edith"), std::path::Path::new("a.edith"));
    }

    #[test]
    fn only_an_exact_edith_extension_is_a_project() {
        let p = std::path::Path::new;
        assert!(is_project(p("a.edith")));
        // A dotted directory must not decide it -- the file name does.
        assert!(is_project(p("/v.mp4/a.edith")));
        assert!(!is_project(p("a.mp4")));
        assert!(!is_project(p("/v.edith/a.mp4")));
        // Exactly what `save_project` writes: a dropped `.EDITH` goes to the
        // demuxer and is refused there, not parsed as a project.
        assert!(!is_project(p("a.EDITH")));
        // An extension, never a bare name.
        assert!(!is_project(p("edith")));
        assert!(!is_project(p("a.edith.mp4")));
    }

    /// The keys menu is the registry drawn, so a *bindable* stroke cannot go
    /// missing from it by construction. The strokes the modal cards read for
    /// themselves are the ones that could: this reads the key handler's own
    /// source and fails on any key it answers to that the menu never mentions.
    #[test]
    fn no_stroke_is_missing_from_the_keys_menu() {
        use keymap::{ActionId, Keymap};
        // Everything above the test module -- the handler, and the helpers it
        // asks. The tests below compare keys too, and are not shortcuts.
        let handler = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        let keymap = Keymap::defaults();
        let listed = |key: &str| {
            let pretty = keymap::Chord {
                key: key.to_string(),
                ctrl: false,
            }
            .pretty();
            keymap.lookup(key, false).is_some() || keymap::FIXED.iter().any(|f| f.chord == pretty)
        };
        let mut compared = 0;
        for (at, needle) in handler.match_indices("key == ") {
            let rest = &handler[at + needle.len()..];
            let key = match rest.strip_prefix('"') {
                Some(literal) => {
                    literal[..literal.find('"').expect("unterminated key")].to_string()
                }
                // A named constant: the escape the cards get out by.
                None if rest.starts_with("ESCAPE") => super::ESCAPE.to_string(),
                None => panic!("a key compared against something this cannot read: {rest:.20}"),
            };
            assert!(
                listed(&key),
                "the handler answers to {key:?} and the keys menu never says so"
            );
            compared += 1;
        }
        // The scan is only a guard while it still finds the comparisons: a
        // rewrite that spells them differently must come back here.
        assert!(
            compared >= 4,
            "the key comparisons moved; this scan is blind"
        );
        // The one branch that is not a comparison -- any digit is a bitrate.
        assert!(handler.contains("key.parse::<u32>()"));
        assert!(keymap::FIXED.iter().any(|f| f.chord == "0–9"));
        // And every entry of both halves lands under a heading the menu draws.
        for action in ActionId::ALL {
            assert!(keymap::Category::ALL.contains(&action.category()));
        }
        for fixed in keymap::FIXED.iter() {
            assert!(keymap::Category::ALL.contains(&fixed.category));
        }
    }

    /// The bed the view tests are drawn on: 200 px at 30 fps, as the drop test
    /// above uses.
    const TEST_BED: f32 = 200.;

    /// A scale against that bed and a timeline `duration` seconds long.
    fn test_view(scale: Scale, duration: f64) -> View {
        View {
            scale,
            bed: TEST_BED,
            duration,
            fps: 30.,
        }
    }

    /// The mapping the whole panel is drawn and clicked through: a moment goes
    /// to a pixel on the bed and comes back the same moment, at every zoom.
    #[test]
    fn a_moment_and_the_place_it_is_drawn_are_the_same_at_every_zoom() {
        let duration = 20.;
        // The fit, which is the only scale the content's own length picks: 20 s
        // across a 200 px bed is 10 px to the second, and a 5 s clip is a
        // quarter of the bed -- the answer the fractional view gave for it.
        let fit = test_view(Scale::default(), duration).fit();
        assert_eq!(fit.pps, 10.);
        assert_eq!(fit.width_px(5.), TEST_BED * 0.25);
        for t in [0., 5., 12.5, 20.] {
            assert_eq!(fit.px_at(t), (t / duration) as f32 * TEST_BED, "fit at {t}");
        }
        for scale in [
            fit,
            Scale {
                pps: 20.,
                start: 4.,
            },
            Scale {
                pps: 80.,
                start: 12.5,
            },
            Scale {
                pps: 375.,
                start: 19.,
            },
        ] {
            let scale = test_view(scale, duration).settled();
            for x in [0., 50., 100., TEST_BED] {
                let at = scale.time_at(x);
                assert!(
                    (f64::from(scale.px_at(at)) - f64::from(x)).abs() < 1e-3,
                    "{scale:?} round trip at {x}"
                );
            }
            // A stretch as long as what is on the bed is as wide as the bed.
            assert!(
                (f64::from(scale.width_px(test_view(scale, duration).span())) - f64::from(TEST_BED))
                    .abs()
                    < 1e-3,
                "{scale:?} spans the bed"
            );
        }
    }

    /// The rule that makes a zoom usable: whatever was under the anchor is
    /// still under it afterwards -- the playhead for a key, the pointer for a
    /// ctrl+wheel.
    #[test]
    fn a_zoom_leaves_the_anchor_where_it_was_on_screen() {
        let duration = 20.;
        let mut scale = test_view(Scale::default(), duration).settled();
        // The same three points along the bed the fractional view held: a half,
        // a quarter and nine tenths of 200 px.
        for anchor in [100f32, 50., 180.] {
            // Well past the zoom-in stop: the anchor holds at the clamp too,
            // which is where it used to slide (the clamp came after the offset).
            for _ in 0..30 {
                let at = scale.time_at(anchor);
                let view = test_view(scale, duration);
                let zoomed = view.zoomed(ZOOM_STEP, anchor);
                // Only where the anchor is not pinned by an edge of the
                // timeline: a view already against an end cannot slide further.
                let span = test_view(zoomed, duration).span();
                let pinned = zoomed.start <= 0. || zoomed.start + span >= duration;
                if !pinned {
                    assert!(
                        (f64::from(zoomed.px_at(at)) - f64::from(anchor)).abs() < 1e-3,
                        "{at} moved: {scale:?} -> {zoomed:?}"
                    );
                }
                assert!(zoomed.pps >= scale.pps, "and it did zoom in");
                scale = zoomed;
            }
        }
    }

    /// Both stops. In is [`ZOOM_MIN_FRAMES`] across the bed; out, on a timeline
    /// far too short to be worth widening to, is a pixel to the second -- so a
    /// short import can be zoomed out of, a long one zoomed into, and neither
    /// can scroll off an end.
    #[test]
    fn zoom_stops_at_a_bedful_of_time_and_at_a_handful_of_frames() {
        let (duration, fps) = (20., 30.);
        let mut scale = Scale::default();
        for _ in 0..200 {
            scale = test_view(scale, duration).zoomed(1. / ZOOM_STEP, 100.);
        }
        assert!(
            (test_view(scale, duration).span() - f64::from(TEST_BED) / PPS_MIN).abs() < 1e-6,
            "widest is a pixel to the second, not the 20 s that happen to be on it"
        );
        for _ in 0..200 {
            scale = test_view(scale, duration).zoomed(ZOOM_STEP, 100.);
        }
        assert_eq!(
            (test_view(scale, duration).span() * fps).round(),
            ZOOM_MIN_FRAMES,
            "tightest is a handful of frames"
        );
        // Against the far end, the slice still ends at the last frame.
        let end = test_view(
            Scale {
                start: 1e6,
                ..scale
            },
            duration,
        );
        let end = (end.settled(), end.span());
        assert!((end.0.start + end.1 - duration).abs() < 1e-9);
        assert_eq!(
            test_view(
                Scale {
                    start: -5.,
                    ..scale
                },
                duration
            )
            .settled()
            .start,
            0.
        );
        // The dead zone the fractional view had: a timeline of a handful of
        // frames could not be zoomed at all, because its own length was the
        // floor *and* the ceiling. Both keys work on it now.
        let tiny = test_view(Scale::default(), 0.1);
        assert!(tiny.zoomed(ZOOM_STEP, 100.).pps > PPS_DEFAULT);
        assert!(tiny.zoomed(1. / ZOOM_STEP, 100.).pps < PPS_DEFAULT);
        // A timeline with no length at all divides by nothing and scrolls
        // nowhere; the fit of one keeps the scale it had.
        let empty = test_view(Scale::default(), 0.);
        assert_eq!(empty.settled(), Scale::default());
        assert_eq!(empty.fit(), Scale::default());
        assert_eq!(Scale::default().px_at(0.), 0.);
        // A bed that was never painted clamps nothing -- there is nothing to
        // clamp against, and a zoom must survive the frame before the probe.
        let unpainted = View {
            bed: 0.,
            ..test_view(Scale { pps: 4e6, start: 3. }, duration)
        };
        assert_eq!(unpainted.settled().pps, 4e6);
        assert_eq!(unpainted.following(19.), unpainted.scale);
    }

    /// The bug a fixed far stop was: two two-and-a-half hour clips are five
    /// hours of timeline, longer than any stop measured in hours, so the end of
    /// the second one could not be brought on screen by any zoom. The stop is
    /// the timeline's own length now, so it can.
    #[test]
    fn zooming_out_reaches_the_end_of_a_timeline_however_long_it_is() {
        let bed = 900.;
        let view = |scale: Scale, duration: f64| View {
            scale,
            bed,
            duration,
            fps: 30.,
        };
        // As far out as the keys go, from the scale a fresh project is drawn at.
        let out = |duration: f64| {
            let mut scale = Scale::default();
            for _ in 0..400 {
                scale = view(scale, duration).zoomed(1. / ZOOM_STEP, 0.);
            }
            scale
        };
        let five_hours = 2. * 2.5 * 3600.;
        let wide = out(five_hours);
        // The whole five hours is on the bed, the last frame drawn inside the
        // window rather than against its edge.
        assert_eq!(wide.start, 0.);
        let end = wide.px_at(five_hours);
        assert!(end < bed, "the end of the timeline is off the bed at {end} px");
        assert!(end > bed * 0.9, "and not shrunk into a corner: {end} px");
        assert!(
            (view(wide, five_hours).span() - five_hours * ZOOM_OUT_MARGIN).abs() < 1e-6,
            "the far stop is the timeline plus its margin"
        );
        // Zooming back in still reaches the frame stop on a timeline that long:
        // the far stop moving does not drag the near one with it.
        let mut scale = wide;
        for _ in 0..400 {
            scale = view(scale, five_hours).zoomed(ZOOM_STEP, 0.);
        }
        assert_eq!(
            (view(scale, five_hours).span() * 30.).round(),
            ZOOM_MIN_FRAMES
        );
        // And a ten second project is not zoomed out to four hours of empty
        // bed: short of a pixel to the second its own length is not worth
        // widening to, and that is 900 s of bed, not 14400.
        assert!((view(out(10.), 10.).span() - f64::from(bed) / PPS_MIN).abs() < 1e-6);
        // Whatever the length, the resting scale is nobody's content: the width
        // invariant, which a far stop measured off the content would break.
        assert_eq!(view(Scale::default(), 10.).settled(), Scale::default());
        assert_eq!(view(Scale::default(), five_hours).settled(), Scale::default());
        // Shrinking the timeline pulls a fully zoomed out view in with it --
        // what was showing all of the timeline still shows all of it.
        let shrunk = view(wide, 1800.).settled();
        assert!((view(shrunk, 1800.).span() - 1800. * ZOOM_OUT_MARGIN).abs() < 1e-6);
        assert!(shrunk.px_at(1800.) < bed);
        // Growing it does not: a scale the user zoomed to is still legal, and
        // the stop it stopped at is one press further out.
        assert_eq!(view(wide, 2. * five_hours).settled().pps, wide.pps);
        assert!(view(wide, 2. * five_hours).zoomed(1. / ZOOM_STEP, 0.).pps < wide.pps);
    }

    /// What makes a zoomed timeline follow the playing head: off the bed at
    /// either end pulls the view onto it, and on the bed it never moves -- a
    /// view that jumped every frame would be unreadable.
    #[test]
    fn the_view_follows_a_playhead_that_runs_off_the_bed() {
        let duration = 20.;
        // 5 s on a 200 px bed: 40 px to the second, starting at second 5.
        let view = test_view(
            Scale {
                pps: 40.,
                start: 5.,
            },
            duration,
        );
        let scale = view.settled();
        assert_eq!(view.span(), 5.);
        // Inside: untouched, whichever part of the slice it is in.
        for at in [5., 7.5, 10.] {
            assert_eq!(view.following(at), scale, "{at} is on the bed");
        }
        // Past the right edge, as playback does it: the head comes back on the
        // bed, and the scale is not changed by the scroll.
        let moved = view.following(12.);
        assert_eq!(moved.pps, scale.pps);
        assert!(moved.start > scale.start, "scrolled forward");
        assert!(
            moved.px_at(12.) > 0. && moved.px_at(12.) < TEST_BED,
            "and the playhead is on screen"
        );
        // A seek back behind the slice does the same the other way.
        let back = view.following(1.);
        assert!(back.start < scale.start);
        assert!(back.px_at(1.) >= 0.);
        // With the whole timeline on the bed there is nothing to follow.
        let whole = test_view(Scale::default(), duration);
        let fit = whole.fit();
        for at in [0., 10., 20.] {
            assert_eq!(
                test_view(fit, duration).following(at),
                fit,
                "the fit never scrolls"
            );
        }
    }

    /// The bug this mapping exists for: the first import used to fill the whole
    /// track whatever it was, because the bed *was* the timeline -- so a 5 s
    /// clip was 100% of the lane, zooming out did nothing, and adding a second
    /// clip silently halved the first one's box.
    #[test]
    fn a_clip_is_drawn_the_same_width_whatever_else_is_on_the_timeline() {
        let bed = 900.;
        let of = |duration: f64| View {
            scale: Scale::default(),
            bed,
            duration,
            fps: 30.,
        };
        // One 5 s import, then a second clip after it: 20 s of timeline where
        // there were 5, and the first box has not moved or narrowed.
        let (alone, joined) = (of(5.).settled(), of(20.).settled());
        assert_eq!(alone, joined);
        assert_eq!(alone.width_px(5.), joined.width_px(5.));
        assert_eq!(joined.px_at(5.), alone.width_px(5.));
        // And it does not fill the track: a short clip reads as short.
        assert!(
            alone.width_px(5.) < bed / 2.,
            "5 s at {} px/s is {} px of a {bed} px bed",
            alone.pps,
            alone.width_px(5.)
        );
        // Zooming out visibly shrinks it, however short the timeline is -- the
        // press that used to be a no-op.
        let out = of(5.).zoomed(1. / ZOOM_STEP, 0.);
        assert!(
            out.width_px(5.) < alone.width_px(5.),
            "{} px is not smaller than {} px",
            out.width_px(5.),
            alone.width_px(5.)
        );
        // The way back to "the whole timeline across the bed" is still one key.
        assert_eq!(of(5.).fit().width_px(5.), bed);
    }

    /// What the zoom button says: how much timeline is on the bed, in a unit
    /// that tells two zooms apart.
    #[test]
    fn the_zoom_button_says_how_much_is_on_the_bed() {
        assert_eq!(span_label(4.5), "4.5s");
        assert_eq!(span_label(22.5), "22s");
        assert_eq!(span_label(90.), "1.5m");
        assert_eq!(span_label(3600.), "1.0h");
        // The span a five hour timeline is zoomed all the way out to.
        assert_eq!(span_label(5. * 3600. * 1.05), "5.2h");
        // Before the first paint there is no bed and so no answer to give.
        assert_eq!(span_label(0.), "—");
        assert_eq!(span_label(f64::NAN), "—");
        // A span under a second is a span: the tightest zoom is
        // `ZOOM_MIN_FRAMES` across the bed, which on 240 fps slow-motion is
        // 0.03s, and "0.0s" would be the pill saying nothing is on the bed.
        for fps in [60., 120., 240., 1000.] {
            let label = span_label(ZOOM_MIN_FRAMES / fps);
            assert_ne!(label, "0.0s", "{fps} fps");
            assert_ne!(label, "0.00s", "{fps} fps");
        }
        assert_eq!(span_label(ZOOM_MIN_FRAMES / 240.), "0.03s");
        // A frame of quiet at 60 fps, which the silence card says out loud.
        assert_eq!(secs_label(1. / 60.), "0.02s");
        assert_eq!(secs_label(4.5), "4.5s");
    }

    #[test]
    fn escape_gets_out_of_an_export_whatever_is_held_with_it() {
        use keymap::ActionId;
        // The regression this guards: looking the stroke up in the keymap made
        // ctrl+escape mean nothing, and an export that a modifier could trap
        // the user inside of is worse than an unbound chord.
        assert!(cancels_export("escape", None));
        assert!(cancels_export("escape", Some(ActionId::CancelExport)));
        // A rebound cancel works as well -- it adds a way out, never replaces
        // the one that always worked.
        assert!(cancels_export("q", Some(ActionId::CancelExport)));
        // Nothing else does, whatever it means outside an export.
        assert!(!cancels_export("e", Some(ActionId::Export)));
        assert!(!cancels_export("space", Some(ActionId::Play)));
        assert!(!cancels_export("q", None));
    }

    #[test]
    fn a_capture_waits_through_a_lone_modifier() {
        // gpui delivers these on their own; taking one as a binding would make
        // the action fire on the way to every chord that uses it.
        for key in [
            "control", "shift", "alt", "super", "platform", "function", "fn", "meta", "command",
        ] {
            assert!(is_bare_modifier(key), "{key}");
        }
        // Everything a binding is actually made of, escape included -- the
        // capture branch turns that one away itself.
        for key in ["c", "x", "space", "escape", "delete", "f1", "z"] {
            assert!(!is_bare_modifier(key), "{key}");
        }
    }

    /// The whole point of the card: there is no action a pointer cannot reach.
    /// It renders [`keys_rows`] and nothing else, so this reads the same list
    /// the card does -- add an `ActionId` and forget to surface it and this
    /// fails, which is the only way that stays true as the editor grows.
    #[test]
    fn every_action_is_on_the_actions_card() {
        use keymap::{ActionId, Category, Keymap};
        let rows = keys_rows();
        let listed: Vec<ActionId> = rows
            .iter()
            .filter_map(|r| match r {
                KeyRow::Act(a) => Some(*a),
                _ => None,
            })
            .collect();
        for action in ActionId::ALL {
            assert_eq!(
                listed.iter().filter(|a| **a == action).count(),
                1,
                "{action:?} is not on the card exactly once"
            );
        }
        assert_eq!(listed.len(), ActionId::ALL.len());
        // Under its own heading, in the registry's order: every row after a
        // heading belongs to that heading until the next one.
        let mut heading = None;
        let mut heads = 0;
        for row in &rows {
            match row {
                KeyRow::Head(category) => {
                    heading = Some(*category);
                    heads += 1;
                }
                KeyRow::Act(action) => assert_eq!(Some(action.category()), heading, "{action:?}"),
                KeyRow::Fixed(i) => assert_eq!(Some(keymap::FIXED[*i].category), heading),
            }
        }
        assert_eq!(heads, Category::ALL.len(), "a heading per category");
        // The card-local strokes are still all there beside them.
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, KeyRow::Fixed(_)))
                .count(),
            keymap::FIXED.len()
        );
        // Both columns say something: the label does the action, the stroke
        // beside it changes that stroke, and neither may read blank.
        let keymap = Keymap::defaults();
        for action in ActionId::ALL {
            assert!(!action.label().is_empty(), "{action:?}");
            assert_ne!(keymap.display(action), "unbound", "{action:?}");
        }
        // The list scrolls inside a card the smallest window holds, so a
        // thirty-fourth action costs no height at all.
        assert!(rows.len() as f32 * KEYS_ROW_H > KEYS_ROWS_H, "no cap needed?");
        // Both halves of a row are click targets, so WCAG 2.5.8 binds them.
        assert!(KEYS_ROW_H >= HIT_MIN);
    }

    /// The search box: what a typed word leaves standing. Forty actions is more
    /// than a 360 px window shows at once, so this is the card's answer to
    /// "where is the one I want" -- and a heading with nothing under it would
    /// be worse than no filter at all.
    #[test]
    fn a_search_leaves_the_rows_it_names_under_their_own_headings() {
        use keymap::{ActionId, Category, Keymap};
        let keymap = Keymap::defaults();
        let acts = |found: &[(usize, KeyRow)]| -> Vec<ActionId> {
            found
                .iter()
                .filter_map(|(_, r)| match r {
                    KeyRow::Act(a) => Some(*a),
                    _ => None,
                })
                .collect()
        };
        let heads = |found: &[(usize, KeyRow)]| -> Vec<Category> {
            found
                .iter()
                .filter_map(|(_, r)| match r {
                    KeyRow::Head(c) => Some(*c),
                    _ => None,
                })
                .collect()
        };
        // Nothing typed hides nothing, and every row keeps its place in the
        // unfiltered list: an element id must not move under a keystroke.
        let all = keys_filter("", &keymap);
        assert_eq!(all.len(), keys_rows().len());
        assert!(all.iter().enumerate().all(|(n, (i, _))| n == *i));
        // The word the user types is the word on the row. The mix card names
        // the track volumes, so it is an honest hit; the fixed row about the
        // volume keys is another, and each comes with its own heading only.
        let vol = keys_filter("vol", &keymap);
        assert_eq!(
            acts(&vol),
            vec![ActionId::VolumeUp, ActionId::VolumeDown, ActionId::Mix]
        );
        assert_eq!(heads(&vol), vec![Category::Audio, Category::View]);
        // Case is not part of the question, in either direction.
        assert_eq!(keys_filter("VoL", &keymap).len(), vol.len());
        // The stroke column is searched too -- "what did ctrl do again" -- and
        // an unbound-looking word finds nothing rather than everything.
        let ctrl = keys_filter("ctrl+", &keymap);
        assert!(acts(&ctrl).contains(&ActionId::Save));
        assert!(!acts(&ctrl).contains(&ActionId::Play));
        assert!(
            keys_filter("qzx", &keymap).is_empty(),
            "headings left behind"
        );
        // The card's own door is on the card, by name and by stroke.
        assert_eq!(
            acts(&keys_filter("f1", &keymap)),
            vec![ActionId::ShowActions]
        );
        assert_eq!(
            acts(&keys_filter("all actions", &keymap)),
            vec![ActionId::ShowActions]
        );
    }

    /// What a keystroke means to that box: a letter is a letter, and a word
    /// gpui reports for a key that prints nothing is not typed letter by
    /// letter into the search.
    #[test]
    fn only_a_printable_stroke_types_into_the_search() {
        assert_eq!(typed("a"), Some('a'));
        assert_eq!(typed("-"), Some('-'));
        assert_eq!(typed("1"), Some('1'));
        // The one printable key gpui reports by name.
        assert_eq!(typed("space"), Some(' '));
        for word in ["left", "escape", "f1", "backspace", "tab", "delete", "home"] {
            assert_eq!(typed(word), None, "{word}");
        }
    }

    #[test]
    fn the_keybindings_card_fits_the_smallest_window() {
        // The row list is capped and scrolls, so the card's height no longer
        // depends on how many actions there are: a title, a status line, the
        // search box and the viewport, inside the 640x360 the rest of the
        // layout is sized for.
        let title = 17.; // 13 px text on its own line
        let status = 28.; // 11 px text, two lines: a refusal wraps
        let search = 15.; // 11 px text, one line: it never wraps
        let gaps = 4. * 2.;
        let padding = 24.;
        // The line the list scrolls under: margin, rule, padding.
        let separator = 4. + 1. + 4.;
        assert!(
            title + status + search + separator + KEYS_ROWS_H + gaps + padding <= 360.,
            "card too tall"
        );
        // ...and the list is the only part that grows with the editor, so the
        // rows past the fold are reached by scrolling that viewport (and, with
        // forty of them, by the search box above it) rather than by a card
        // taller than the window.
        assert!(
            keys_rows().len() as f32 * KEYS_ROW_H > KEYS_ROWS_H,
            "the list outgrew the viewport long ago; the cap must still scroll"
        );
        // The cap is only honest if it is the taller list that scrolls, not the
        // card that grows: every action must be reachable by scrolling, and
        // enough of them visible that the list reads as a list.
        assert!(
            KEYS_ROWS_H / KEYS_ROW_H >= 8.,
            "too few rows visible to scan"
        );
        assert!(KEYS_W <= 640., "card too wide");
        // The rows are clickable, so WCAG 2.5.8 binds them like every other
        // target in this window.
        assert!(KEYS_ROW_H >= HIT_MIN);
    }

    /// The equalizer card's graph: where a band lands on it, that a drag reads
    /// back as the gain it painted -- the two are one mapping and its inverse,
    /// which is what makes a handle follow the pointer -- and that the card
    /// still fits the window the other two are sized for.
    #[test]
    fn the_equalizer_graph_puts_a_band_where_a_drag_reads_it_and_fits_the_smallest_window() {
        use engine::eq::{Band, BandKind, EqParams};
        // Flat is the middle of the box: a band nobody has touched must not
        // look like one that has been turned down.
        assert_eq!(eq_y(0.), 0.5);
        // Full boost is the top edge, full cut the bottom one -- y grows down.
        assert_eq!(eq_y(EQ_GAIN_LIMIT), 0.);
        assert_eq!(eq_y(-EQ_GAIN_LIMIT), 1.);
        assert_eq!(eq_y(EQ_GAIN_LIMIT / 2.), 0.25);
        // A file may carry a gain past what this card offers (the format writes
        // any finite value): it paints on the edge, never off the box.
        assert_eq!(eq_y(400.), 0.);
        assert_eq!(eq_y(-400.), 1.);

        // A pointer reads back as the gain that paints where it landed, which
        // is what makes a drag land under the hand ([`Player::drag_band`]).
        for gain in [-12., -6., 0., 6., 12.] {
            let read = (0.5 - eq_y(gain)) * 2. * EQ_GAIN_LIMIT;
            assert!((read - gain).abs() < 1e-4, "{gain} read back as {read}");
        }

        // The frequency axis is logarithmic and spans the audible range: the
        // ends are the ends, and the decade at 200 Hz is as wide as the one at
        // 2 kHz -- the whole reason a bass band is reachable at all.
        assert_eq!(eq_x(EQ_FREQ_LOW), 0.);
        assert_eq!(eq_x(EQ_FREQ_HIGH), 1.);
        assert!((eq_x(200.) - 1. / 3.).abs() < 1e-4);
        assert!(((eq_x(2000.) - eq_x(200.)) - (eq_x(20000.) - eq_x(2000.))).abs() < 1e-4);
        // Off either end clamps rather than painting outside the box.
        assert_eq!(eq_x(1.), 0.);
        assert_eq!(eq_x(96_000.), 1.);
        // Every tick the card names is on the axis it is drawn against.
        for (freq, label) in EQ_TICKS {
            assert!(
                (EQ_FREQ_LOW..=EQ_FREQ_HIGH).contains(&freq),
                "tick {label} is off the axis"
            );
        }
        // The default bands all land inside it too, spread out enough that the
        // nearest-band pick (`Player::nearest_band`) has something to pick.
        let xs: Vec<f32> = EqParams::default_layout()
            .bands
            .iter()
            .map(|b| eq_x(b.freq_hz))
            .collect();
        for pair in xs.windows(2) {
            assert!(pair[1] - pair[0] > 0.1, "bands too close to aim at: {xs:?}");
        }

        // Every default band says what it is, and a shelf says so: "12 kHz"
        // alone would not tell anyone it tilts the whole top octave.
        let labels: Vec<String> = EqParams::default_layout()
            .bands
            .iter()
            .map(band_label)
            .collect();
        assert_eq!(
            labels,
            [
                "80 Hz low shelf",
                "250 Hz",
                "1 kHz",
                "4 kHz",
                "12 kHz high shelf"
            ]
        );
        // A band moved off a round number reads as where it *is*: a keystroke
        // that changes the filter and not the number on the card is a keystroke
        // nobody can aim.
        assert_eq!(
            band_label(&Band {
                freq_hz: 2600.,
                gain_db: 0.,
                q: 1.,
                kind: BandKind::Peak
            }),
            "2.6 kHz"
        );
        assert_eq!(eq_freq_label(1122.), "1.12 kHz");
        assert_eq!(eq_freq_label(12000.), "12 kHz", "no zeroes to read past");
        assert_eq!(eq_freq_label(80.), "80 Hz");

        // The card fits the smallest window and takes the room a bigger one has
        // -- it is a graph, and the width *is* the frequency resolution.
        assert!(eq_card_w(640.) <= 640. - 24., "card too wide for 640");
        assert!(eq_card_w(1280.) > eq_card_w(640.), "card ignores the window");
        assert_eq!(eq_card_w(1920.), EQ_W_MAX, "card grows without end");
        assert!(eq_card_w(320.) >= KEYS_W, "card narrower than a row of text");
        // At the smallest window the graph is still a graph: three across for
        // one down, so an octave is wide enough to put a handle in.
        assert!(
            eq_card_w(640.) - 24. >= 3. * EQ_GRAPH_H,
            "graph too square to aim at"
        );

        // The same shape as the other two cards, so it fits where they do: the
        // graph stands where the export card's rows do, and is shorter than
        // they are. The numbers row is a row of buttons now, so it is one of
        // those tall rather than one line of text.
        let (title, status, gaps, padding) = (17., 28., 4. * 2., 24.);
        assert!(
            title + status + EQ_GRAPH_H + HIT_MIN + gaps + padding + CONTROL_H <= 360.,
            "card too tall"
        );
        assert!(
            EQ_GRAPH_H <= EXPORT_ROWS_H,
            "graph taller than a card of rows"
        );
        // What is dragged is the whole graph -- the handle is a 10 px dot, but
        // a press anywhere in the box takes the band nearest it -- so WCAG
        // 2.5.8 is satisfied by the box, which is far past the minimum.
        assert!(EQ_GRAPH_H >= HIT_MIN);
        assert!(
            EQ_HANDLE < HIT_MIN,
            "a dot that size would want its own hitbox"
        );
        assert!(KEYS_ROW_H >= HIT_MIN);
    }

    /// Editing a band, which is what the card is *for*: the pointer reads a
    /// frequency off the axis the same way the axis draws one, a new band lands
    /// in the gap beside the picked one rather than on top of it, and every band
    /// the card will hold has a digit that picks it.
    #[test]
    fn a_band_can_be_moved_added_and_reached() {
        use engine::eq::{Band, BandKind, EqParams};
        // Across the graph and back: a drag sets the frequency the handle is
        // then drawn at, so the two mappings have to be one another's inverse or
        // the handle walks away from the pointer.
        for freq in [20., 80., 250., 1000., 4000., 12000., 20000.] {
            let read = eq_freq(eq_x(freq));
            assert!(
                (read / freq - 1.).abs() < 1e-3,
                "{freq} Hz read back as {read}"
            );
        }
        // Off the box either end stops at the axis, never past it.
        assert_eq!(eq_freq(-1.), EQ_FREQ_LOW);
        assert_eq!(eq_freq(2.), EQ_FREQ_HIGH);

        // A step of the frequency keys is a real move on screen -- a keystroke
        // that changes nothing visible is a keystroke that reads as broken --
        // and small enough to aim with.
        let step = eq_x(1000. * EQ_FREQ_STEP) - eq_x(1000.);
        assert!(step > 0.01 && step < 0.06, "frequency key steps {step}");

        // A new band lands between the picked one and the next one up, in
        // octaves: 250 Hz and 1 kHz put it at 500, which is a gap on screen.
        let bands = EqParams::default_layout().bands;
        let added = inserted_band(&bands, 1);
        assert!((added.freq_hz - 500.).abs() < 1., "landed at {added:?}");
        assert_eq!(added.gain_db, 0., "a new band changes nothing until moved");
        assert_eq!(added.kind, BandKind::Peak, "a new band is not a shelf");
        assert!(eq_x(added.freq_hz) - eq_x(bands[1].freq_hz) > 0.1);
        // Above the topmost band the gap is the rest of the axis, and the band
        // still lands on it rather than off the end.
        let top = inserted_band(&bands, bands.len() - 1);
        assert!(
            top.freq_hz > bands[4].freq_hz && top.freq_hz <= EQ_FREQ_HIGH,
            "landed at {top:?}"
        );
        // Bands out of frequency order (a drag may cross two over) still get a
        // sane neighbour: the next one *up*, not the next one along the list.
        let crossed = vec![
            Band {
                freq_hz: 4000.,
                gain_db: 0.,
                q: 1.,
                kind: BandKind::Peak,
            },
            Band {
                freq_hz: 100.,
                gain_db: 0.,
                q: 1.,
                kind: BandKind::Peak,
            },
        ];
        let between = inserted_band(&crossed, 1);
        assert!(
            (between.freq_hz - 632.).abs() < 2.,
            "landed at {between:?}, not between 100 and 4000"
        );

        // Every band the card will hold is one digit away: the keyboard has ten
        // digits, which is exactly why the cap is what it is.
        assert!(
            EQ_BANDS_MAX <= 10,
            "a band past the tenth has no key that picks it"
        );
        assert!(EQ_BANDS_MAX > EqParams::default_layout().bands.len());
        // The Q range holds the default, so a file's band never opens out of
        // range and needs dragging back in before it can be edited.
        assert!((EQ_Q_LOW..=EQ_Q_HIGH).contains(&0.707));
        assert!(EQ_Q_STEP > 1.);
    }

    /// The analyser drawn behind the curve: a tone has to land on its own
    /// frequency, or the backdrop is a decoration rather than a reading of what
    /// is playing -- and someone shaping a band against it would be aiming at
    /// the wrong octave.
    #[test]
    fn the_spectrum_puts_a_tone_under_its_own_frequency() {
        use std::f32::consts::TAU;
        let rate = 48_000u32;
        let sine = |hz: f32, amp: f32| -> Vec<f32> {
            (0..EQ_FFT)
                .map(|i| amp * (TAU * hz * i as f32 / rate as f32).sin())
                .collect()
        };

        // Silence is the floor of the box everywhere, not a band of noise.
        let quiet = eq_spectrum(&vec![0.; EQ_FFT], rate);
        assert_eq!(quiet.len(), EQ_CURVE_STEPS + 1);
        assert!(quiet.iter().all(|&l| l == 0.), "silence drew something");

        // A tone peaks over its own frequency, and the columns two octaves
        // either side of it are near the floor. "Over" to within a bin down low
        // and a column up high, which is all a 1024-point transform on a log
        // axis can promise: at 200 Hz a whole column is a fraction of a bin
        // wide, so the peak sits on the bin the tone fell in.
        let column = |freq: f32| (eq_x(freq) * EQ_CURVE_STEPS as f32).round() as usize;
        let freq_at = |col: usize| {
            let along = col as f32 / EQ_CURVE_STEPS as f32;
            EQ_FREQ_LOW * (EQ_FREQ_HIGH / EQ_FREQ_LOW).powf(along)
        };
        for hz in [200., 1000., 5000.] {
            let levels = eq_spectrum(&sine(hz, 0.05), rate);
            let (at, top) =
                levels
                    .iter()
                    .enumerate()
                    .fold((0, 0f32), |best, (i, &l)| match l > best.1 {
                        true => (i, l),
                        false => best,
                    });
            let slack = 1.5 * rate as f32 / EQ_FFT as f32 + 0.04 * hz;
            assert!(
                (freq_at(at) - hz).abs() <= slack,
                "{hz} Hz peaked at {:.0} Hz (column {at})",
                freq_at(at)
            );
            // -26 dBFS, which is where a 0.05 tone sits on the axis.
            let want =
                (20. * 0.05f32.log10() - EQ_SPECTRUM_DB.0) / (EQ_SPECTRUM_DB.1 - EQ_SPECTRUM_DB.0);
            assert!(
                (top - want).abs() < 0.05,
                "a -26 dBFS tone drew {top} of the box, not {want}"
            );
            // Two octaves off is far enough down the box that the hump reads as
            // one hump: 0.4 of the axis is a little over 30 dB.
            for off in [hz / 4., hz * 4.] {
                let away = levels[column(off).min(EQ_CURVE_STEPS)];
                assert!(
                    top - away > 0.4,
                    "{hz} Hz drew {top} but still {away} at {off} Hz"
                );
            }
        }

        // Level reads as level: 40 dB quieter sits lower by the fraction of the
        // axis those 40 dB are.
        let loud = eq_spectrum(&sine(1000., 0.05), rate);
        let soft = eq_spectrum(&sine(1000., 0.0005), rate);
        let at = column(1000.);
        let drop = loud[at] - soft[at];
        let (floor, ceiling) = EQ_SPECTRUM_DB;
        assert!(
            (drop - 40. / (ceiling - floor)).abs() < 0.05,
            "40 dB quieter moved the analyser {drop} of the box"
        );

        // A tap the engine has not filled yet (right after a seek) draws
        // nothing at all rather than a transform of half a window.
        assert!(eq_spectrum(&[0.; 16], rate).is_empty());
    }

    /// The speed bar is the same round trip -- pixels -> rate -> fill -- with
    /// one thing the colour sliders do not have to promise: **exactly 1.00x has
    /// to be reachable**, by a hand as well as by the reset. A grid that missed
    /// it would leave a clip nobody could put back.
    #[test]
    fn the_speed_bar_lands_where_it_paints_and_real_time_is_reachable() {
        let bar = Bounds {
            origin: point(px(180.), px(240.)),
            size: size(px(COLOR_BAR_W), px(KEYS_ROW_H)),
        };
        let (lo, hi) = (
            f32::from(Speed::MIN.permille()),
            f32::from(Speed::MAX.permille()),
        );
        // The same arithmetic `Player::drag_speed` runs, which is the one thing
        // a test of it can share without re-deriving it.
        let at = |x: f32| {
            let raw = lo + frac_along(px(x), bar) * (hi - lo);
            speed_at((raw / SPEED_STEP as f32).round() as i32 * SPEED_STEP)
        };
        assert_eq!(at(180.), Speed::MIN, "the left end is a quarter speed");
        assert_eq!(at(180. + COLOR_BAR_W), Speed::MAX, "the right end is 4x");
        assert_eq!(at(-4000.), Speed::MIN, "off the left clamps");
        assert_eq!(at(9999.), Speed::MAX, "off the right clamps");
        let mut hits_real_time = false;
        for step in 0..=240 {
            let along = step as f32 / 240.;
            let speed = at(180. + along * COLOR_BAR_W);
            hits_real_time |= speed == Speed::NORMAL;
            assert_eq!(
                i32::from(speed.permille()) % SPEED_STEP,
                0,
                "{speed} is off the {SPEED_STEP} grid the keys move on"
            );
            // What the bar paints from that rate is where the pointer was, to
            // within the half step the snap costs.
            let painted = (f32::from(speed.permille()) - lo) / (hi - lo);
            let slack = SPEED_STEP as f32 / (hi - lo) / 2. + 1e-4;
            assert!(
                (painted - along).abs() <= slack,
                "pressed at {along}, paints at {painted}"
            );
        }
        assert!(hits_real_time, "a drag can land on exactly 1.00x");
        // ...and every preset the card offers is a rate the bar can also reach.
        for permille in SPEED_PRESETS {
            assert_eq!(Speed::from_permille(permille).permille(), permille);
            assert_eq!(i32::from(permille) % SPEED_STEP, 0);
        }
        assert!(SPEED_PRESETS.contains(&Speed::NORMAL.permille()), "reset");
    }

    /// A colour slider is dragged straight to a value, so where the pointer
    /// lands and where the bar then paints have to be the same place: this is
    /// the round trip [`Player::drag_color`] makes, pixels -> value -> fill.
    #[test]
    fn a_colour_drag_lands_where_it_paints_and_the_card_fits_the_smallest_window() {
        // A bar as laid out, somewhere that is not the window's origin -- a
        // mapping that forgot the offset would pass at zero.
        let bar = Bounds {
            origin: point(px(180.), px(240.)),
            size: size(px(COLOR_BAR_W), px(KEYS_ROW_H)),
        };
        for &(label, low, high) in &COLOR_BANDS {
            // The ends are the ends: the left of the bar is the bottom of the
            // range and the right is the top, so a slider can be pulled to
            // either without hunting for the last pixel.
            let at = |x: f32| color_snap(low + frac_along(px(x), bar) * (high - low));
            assert_eq!(at(180.), low, "{label} left end");
            assert_eq!(at(180. + COLOR_BAR_W), high, "{label} right end");
            // Off either end clamps rather than running past the range.
            assert_eq!(at(-4000.), low, "{label} off the left");
            assert_eq!(at(9999.), high, "{label} off the right");

            for step in 0..=48 {
                let along = step as f32 / 48.;
                let value = at(180. + along * COLOR_BAR_W);
                // Every stop is one the keyboard can also reach, which is what
                // keeps "0.35" the number the file writes.
                let steps = value / COLOR_STEP;
                assert!(
                    (steps - steps.round()).abs() < 1e-3,
                    "{label}: {value} is off the {COLOR_STEP} grid"
                );
                assert!(
                    (low..=high).contains(&value),
                    "{label}: {value} outside {low}..{high}"
                );
                // What the row paints from that value is where the pointer was,
                // to within the half step the snap costs.
                let painted = (value - low) / (high - low);
                let slack = COLOR_STEP / (high - low) / 2. + 1e-4;
                assert!(
                    (painted - along).abs() <= slack,
                    "{label}: pressed at {along}, paints at {painted}"
                );
            }
        }

        // The same shape as the other two cards, so it fits where they do: the
        // graph, four rows and the reset button inside a 360 px window.
        let (title, status, gaps, padding) = (17., 17., 6. * 2., 24.);
        let rows = COLOR_BANDS.len() as f32 * KEYS_ROW_H;
        assert!(
            title + status + HIST_H + rows + gaps + padding + CONTROL_H + 4. <= 360.,
            "card too tall"
        );
        assert!(COLOR_W <= 640., "card too wide");
        // The label still has room beside the bar and the readout, which is
        // what the buttons coming off the row bought.
        let row = COLOR_W - padding - 12. - 2. * 8. - COLOR_BAR_W - 44.;
        assert!(row >= LABEL_MIN_W, "no room left for a label: {row}px");
        // What is dragged is the whole row's height, not the 4 px the bar is
        // drawn as (WCAG 2.5.8) -- the same split the ruler makes.
        assert!(KEYS_ROW_H >= HIT_MIN);
    }

    /// The graph over the sliders is the frame the grade already went through,
    /// so it has to count what is actually in those bytes -- BGRA on the wire,
    /// red-green-blue in the bins.
    #[test]
    fn the_histogram_counts_the_frame_it_is_handed() {
        // Half pure red, half mid grey: two known values, in two known bins.
        let (w, h) = (64usize, 64usize);
        let mut frame = Vec::with_capacity(w * h * 4);
        for _ in 0..h {
            for col in 0..w {
                match col < w / 2 {
                    true => frame.extend_from_slice(&[0, 0, 255, 255]),
                    false => frame.extend_from_slice(&[128, 128, 128, 255]),
                }
            }
        }
        let bins = histogram(&frame);
        let half = (w * h / 2) as u32;
        // 64 bins over 256 codes: 255 is the last bin, 128 the middle one, 0 the
        // first.
        assert_eq!(bins[0][63], half, "the red half tops the red channel");
        assert_eq!(bins[0][32], half, "and the grey half sits mid red");
        for channel in [1, 2] {
            assert_eq!(bins[channel][0], half, "no green or blue in the red half");
            assert_eq!(bins[channel][32], half);
            assert_eq!(bins[channel][63], 0);
        }
        // Nothing is counted twice and nothing is dropped: this frame is small
        // enough to be read whole.
        for channel in bins {
            assert_eq!(channel.iter().sum::<u32>(), (w * h) as u32);
        }

        // A grade shifts it, which is the whole point of drawing it: the same
        // frame darkened lands in lower bins.
        let darker: Vec<u8> = frame.iter().map(|b| b / 2).collect();
        let bins = histogram(&darker);
        assert_eq!(bins[0][31], half, "255 -> 127");
        assert_eq!(bins[0][16], half, "128 -> 64");

        // A real frame is subsampled: a 1080p one is read every 253rd pixel, so
        // the shape costs a thousandth of the reads and still counts thousands.
        let big = vec![200u8; 1920 * 1080 * 4];
        let bins = histogram(&big);
        let counted = bins[0].iter().sum::<u32>();
        let pixels = 1920 * 1080usize;
        let expected = pixels.div_ceil(pixels / HIST_SAMPLES) as u32;
        assert_eq!(counted, expected, "every strided pixel counted, once");
        assert!(
            (HIST_SAMPLES as u32..=HIST_SAMPLES as u32 + 64).contains(&counted),
            "{counted} samples is not the budget"
        );
        assert_eq!(bins[0][200 * HIST_BINS / 256], counted, "all in one bin");

        // An empty buffer is a flat graph rather than a panic: the card is open
        // before the first frame is pumped.
        assert_eq!(histogram(&[]), [[0; HIST_BINS]; 3]);
    }

    /// Mute and level are one control with two states, and the whole point is
    /// that mute keeps the level: the user gets back what they had, not 100%.
    #[test]
    fn muting_keeps_the_level_it_comes_back_to() {
        let mut volume = Volume::default();
        assert_eq!(volume.gain(), 1.0);
        assert_eq!(volume.label(), "Vol 100%");

        // Four presses down, then muted: the gain is silence but the level is
        // still what it was, and the button keeps saying so.
        for _ in 0..4 {
            volume.step(false);
        }
        assert_eq!(volume.gain(), 0.8);
        volume.muted = true;
        assert_eq!(volume.gain(), 0.0);
        assert_eq!(volume.label(), "Muted 80%");

        // Turning it down while muted stays muted -- the one thing a mute
        // button must never do is get louder because you asked for quieter.
        volume.step(false);
        assert_eq!(volume.gain(), 0.0);
        assert!(volume.muted);

        // Unmute returns to the level, including the step taken while silent.
        volume.muted = false;
        assert_eq!(volume.gain(), 0.75);
    }

    /// The whole transport in one place: the clock keeps running past the last
    /// frame (wall time takes over at audio EOF), so "the clock is going" is
    /// not "this is playing" -- and a button that read the clock showed Pause
    /// on a timeline that had stopped moving. Ended is its own state, it draws
    /// Play, and the next press starts over from the top.
    #[test]
    fn a_played_out_timeline_is_not_playing_and_the_next_press_starts_it_over() {
        assert_eq!(transport(true, false), Transport::Playing);
        assert_eq!(transport(false, false), Transport::Paused);
        // The transition the bug was about: the clock is still running and the
        // decoder is finished. Played out wins, however the clock reads.
        assert_eq!(transport(true, true), Transport::Ended);
        assert_eq!(transport(false, true), Transport::Ended);

        // What the button draws, in each state. Two bars only while it moves.
        assert!(Transport::Playing.is_playing());
        for state in [Transport::Paused, Transport::Ended, Transport::Stopped] {
            assert!(!state.is_playing(), "{state:?} must draw the Play triangle");
        }

        // And what a press does: start over at the end, plain toggle before it,
        // nothing with no timeline. Same answer for the key and the button --
        // both come through `Player::toggle_or_restart`.
        assert!(Transport::Ended.restarts());
        for state in [Transport::Playing, Transport::Paused, Transport::Stopped] {
            assert!(!state.restarts(), "{state:?} must toggle, not reseek");
        }
    }

    /// The half of `Ended` the eye cannot see: the clock. Wall time takes over
    /// at the last frame and nothing used to stop it, so the playhead walked off
    /// the end of the timeline in real time -- and the playhead is what a cut, a
    /// paste, an insert and the analyser all act at. `pump` pauses on the
    /// crossing; this is the engine contract that rests on, driven exactly as
    /// the pump drives it.
    #[test]
    fn the_clock_stops_where_the_timeline_does_and_the_end_still_restarts() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        // Start a breath short of the end: the tail is what this is about, and
        // playing the whole five seconds would say nothing more.
        session.seek(4.8);
        session.play();

        // The pump's own loop -- tick, drain, ask where the transport is --
        // with a deadline so a fixture that will not decode fails as a failure
        // rather than as a hang.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut state = transport(session.is_playing(), session.is_eos());
        while state != Transport::Ended {
            assert!(Instant::now() < deadline, "never reached the end of a 5s file");
            session.tick();
            while session.try_frame().is_some() {}
            state = transport(session.is_playing(), session.is_eos());
        }

        // What `pump` does on the crossing, and the whole point of it: the
        // position holds still afterwards instead of counting on past the end,
        // and it holds still *on the out point* -- where the timecode and the
        // playhead have been showing it. The clock at the moment the end is
        // recognised is not that: a slow renderer reaches EOF with the clock
        // seconds past the timeline, which is why this repositions rather than
        // only freezing.
        session.halt_at_end();
        let stopped_at = session.now();
        assert_eq!(stopped_at, session.timeline_duration());
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(session.now(), stopped_at, "the clock kept running past EOF");
        // And it is still the end: pausing must not spend the state the glyph
        // and the restart both read.
        assert!(session.is_eos());
        assert_eq!(
            transport(session.is_playing(), session.is_eos()),
            Transport::Ended
        );

        // The restart path off that frozen end, which is what the button and
        // the play key do from `Ended`: back to the top, and running.
        session.seek(0.);
        session.play();
        assert!(!session.is_eos(), "a seek revives the session");
        assert!(session.now() < 1.0);
        assert_eq!(
            transport(session.is_playing(), session.is_eos()),
            Transport::Playing
        );
        session.pause();
    }

    /// Both ends hold under a key held down: the ABI only accepts `0.0..=1.0`,
    /// and a wrapped step count would hand it something else.
    #[test]
    fn the_volume_stops_at_both_ends() {
        let mut volume = Volume::default();
        for _ in 0..40 {
            volume.step(true);
        }
        assert_eq!(volume.gain(), 1.0);
        assert_eq!(volume.steps, Volume::MAX_STEPS);

        for _ in 0..40 {
            volume.step(false);
        }
        assert_eq!(volume.gain(), 0.0);
        assert_eq!(volume.label(), "Vol 0%");

        // Silent by the level rather than by the flag is still not muted: the
        // button says which, because only one of them survives a step up.
        assert!(!volume.muted);
        volume.step(true);
        assert_eq!(volume.gain(), 0.05);
    }

    #[test]
    fn a_quality_row_is_the_bitrate_it_promises() {
        // Auto is the one row that says nothing: the exporter derives it, and
        // a number typed against the custom row must not leak into it.
        let mp4 = Format::Mp4;
        assert_eq!(
            export_settings(Quality::Auto, 7, mp4, DEFAULT_AUDIO_KBPS).bitrate,
            None
        );
        assert_eq!(
            export_settings(Quality::Low, 0, mp4, DEFAULT_AUDIO_KBPS).bitrate,
            Some(2_000_000)
        );
        assert_eq!(
            export_settings(Quality::Medium, 0, mp4, DEFAULT_AUDIO_KBPS).bitrate,
            Some(6_000_000)
        );
        assert_eq!(
            export_settings(Quality::High, 0, mp4, DEFAULT_AUDIO_KBPS).bitrate,
            Some(12_000_000)
        );
        // Megabits as typed, and as the row says it back.
        assert_eq!(
            export_settings(Quality::Custom, 7, mp4, DEFAULT_AUDIO_KBPS).bitrate,
            Some(7_000_000)
        );
        assert_eq!(Quality::Low.detail(0), "2 Mbps");
        // The picked format travels, or the card's rows would be a picture of a
        // choice the engine never hears about.
        for format in [Format::Mp4, Format::Wav, Format::Flac] {
            assert_eq!(
                export_settings(Quality::Auto, 0, format, DEFAULT_AUDIO_KBPS).format,
                format
            );
        }
        // Every fixed row sits inside the engine's clamp (export.rs:290), so no
        // row can promise a bitrate the exporter silently changes.
        for quality in Quality::ALL {
            let settings = export_settings(quality, 7, mp4, DEFAULT_AUDIO_KBPS);
            if let Some(bitrate) = settings.bitrate {
                assert!(
                    (1_000_000..=20_000_000).contains(&bitrate),
                    "{quality:?} outside the engine clamp"
                );
            }
            // The software pin is the environment's to set, never a row's.
            assert!(!settings.force_sw);
        }
    }

    /// The Sound row: what it offers, and that the pick travels to the engine
    /// for a *video* export as much as for an MP3 -- both files carry sound, so
    /// a row that only reached the audio formats would be half a control.
    #[test]
    fn the_sound_row_carries_its_rate_into_both_kinds_of_file() {
        // Sorted and unique, so a row that says "b for the next one" is stepping
        // up and not shuffling.
        assert!(AUDIO_KBPS.windows(2).all(|w| w[0] < w[1]));
        // The untouched figure is one of the offered rows, or the first press of
        // `b` would jump somewhere nobody chose.
        assert!(AUDIO_KBPS.contains(&DEFAULT_AUDIO_KBPS));
        // ...and it is what this program wrote before the row existed: the
        // export of a user who never opens it must not change under them.
        assert_eq!(DEFAULT_AUDIO_KBPS, 256);
        for format in [Format::Mp4, Format::Av1, Format::Hevc, Format::Mp3] {
            for kbps in AUDIO_KBPS {
                assert_eq!(
                    export_settings(Quality::Auto, 0, format, kbps).audio_kbps,
                    Some(kbps),
                    "{format:?} at {kbps} kbps"
                );
            }
        }
        // The pointer's door to the same field: one row per rate, the one in
        // force marked exactly once, and every row's small print short enough
        // to survive `MENU_W`'s truncation (the resolution list's own rule).
        let rows = audio_rate_choices(DEFAULT_AUDIO_KBPS);
        assert_eq!(rows.len(), AUDIO_KBPS.len());
        assert_eq!(rows.iter().filter(|(.., picked)| *picked).count(), 1);
        for ((choice, label, detail, picked), kbps) in rows.iter().zip(AUDIO_KBPS) {
            assert_eq!(*choice, Choice::AudioRate(kbps));
            assert_eq!(label.as_ref(), format!("{kbps} kbps"));
            assert!(detail.chars().count() <= 26, "{detail} is too long to read");
            assert_eq!(*picked, kbps == DEFAULT_AUDIO_KBPS);
        }
        // A rate no row holds marks none of them, rather than the wrong one.
        assert!(audio_rate_choices(7).iter().all(|(.., picked)| !picked));

        // The wrap the row's key does, which is the row's own step function.
        assert_eq!(
            next_audio_kbps(AUDIO_KBPS[AUDIO_KBPS.len() - 1]),
            AUDIO_KBPS[0]
        );
        assert_eq!(next_audio_kbps(DEFAULT_AUDIO_KBPS), 320);
        // A rate no list holds (a stale one, say) lands back on the first row
        // rather than nowhere.
        assert_eq!(next_audio_kbps(7), AUDIO_KBPS[1]);
    }

    #[test]
    fn a_typed_bitrate_is_a_field_and_not_a_key_capture() {
        // It opens on the number in force, so backspace edits that number
        // rather than the field starting empty over a bitrate still being used.
        let mut edit = NumberEdit::new(12);
        assert_eq!(edit.text, "12");
        edit.backspace();
        edit.digit(8);
        assert_eq!(edit.text, "18");
        assert_eq!(edit.commit(), Some(18));
        // A card nobody has typed a number into opens empty: zero is not a
        // bitrate anyone chose.
        assert_eq!(NumberEdit::new(0).text, "");

        // Out of range is refused *in words* and the digits stay put: clamping
        // 45 to 20 would write a bitrate the user never typed.
        let mut edit = NumberEdit::new(0);
        for digit in [4, 5] {
            edit.digit(digit);
        }
        assert_eq!(edit.commit(), None);
        assert_eq!(edit.text, "45", "a refusal keeps what was typed");
        let refusal = edit.refusal.clone().expect("a refusal says why");
        assert!(refusal.contains("20"), "{refusal}");
        assert!(edit.detail().starts_with("45▏"), "{}", edit.detail());
        assert!(edit.detail().contains(&refusal));
        // And is fixable in place, which is the whole point of a field.
        edit.backspace();
        assert_eq!(edit.refusal, None, "the reason went with the digit");
        assert_eq!(edit.commit(), Some(4));

        // Empty, zero, and past the digit cap: each its own reason, none of
        // them silent.
        assert!(commit_mbps("").is_err());
        assert!(commit_mbps("0").unwrap_err().contains("not a rate"));
        assert_eq!(commit_mbps("1"), Ok(MBPS_MIN));
        assert_eq!(commit_mbps("20"), Ok(MBPS_MAX));
        assert!(commit_mbps("21").is_err());
        let mut edit = NumberEdit::new(0);
        for digit in [9, 9, 9, 9] {
            edit.digit(digit);
        }
        assert_eq!(edit.text, "999", "the cap holds");
        assert!(edit.refusal.is_some(), "and says it is holding");
        // Never past what a u64 bitrate can be built from -- the committed
        // number is the only one that reaches the engine, and it is bounded.
        assert!(u64::from(MBPS_MAX) * 1_000_000 < u64::from(u32::MAX));
        assert_eq!(MBPS_DIGITS, 3);

        // The arrows step inside the range and stop at both ends: a walk
        // through the legal numbers, never a way out of them.
        let mut edit = NumberEdit::new(0);
        edit.step(1);
        assert_eq!(edit.text, MBPS_MIN.to_string(), "empty starts at the floor");
        edit.step(-1);
        assert_eq!(edit.text, MBPS_MIN.to_string());
        let mut edit = NumberEdit::new(MBPS_MAX);
        edit.step(1);
        assert_eq!(edit.text, MBPS_MAX.to_string());
        edit.step(-1);
        assert_eq!(edit.text, (MBPS_MAX - 1).to_string());
        // A step past a refused number clears the refusal with it.
        let mut edit = NumberEdit::new(0);
        edit.digit(4);
        edit.digit(5);
        assert_eq!(edit.commit(), None);
        edit.step(-1);
        assert_eq!(edit.refusal, None);
        assert_eq!(edit.text, MBPS_MAX.to_string(), "back inside the range");

        // The hint the field shows when there is nothing to refuse names both
        // ways out of it.
        let detail = NumberEdit::new(6).detail();
        assert!(detail.contains("enter") && detail.contains("esc"), "{detail}");
        assert!(detail.starts_with("6▏"), "{detail}");
    }

    #[test]
    fn a_clip_with_no_sound_is_refused_in_the_same_words_whichever_kind_it_is() {
        // A still and a video with no audio track are one answer to one
        // question: the lane and index that were picked, the file, and which of
        // the two soundless things it is. What must never reach the bar is the
        // demuxer's own words -- a png handed to the mp4 reader answers "a box
        // with a larger size than it", which is true of a container and useless
        // to a person.
        assert_eq!(
            unscannable(Lane::V1, 1, std::path::Path::new("/tmp/shot.png")),
            "V1 clip 2 has no audio to scan — shot.png is a picture"
        );
        assert_eq!(
            unscannable(
                Lane::new(LaneKind::Audio, 1),
                0,
                std::path::Path::new("/tmp/test_baseline.mp4")
            ),
            "A2 clip 1 has no audio to scan — test_baseline.mp4 is silent"
        );
    }

    /// The hold gate: a value runs, everything else still means one press one
    /// action -- which is the whole invariant the blanket `is_held` filter used
    /// to carry on its own.
    #[test]
    fn a_held_key_moves_a_value_and_nothing_else() {
        use keymap::ActionId;
        // A card's four arrows, whichever card it is.
        for key in ["up", "down", "left", "right"] {
            assert!(repeats(Repeat::Card, key, None), "{key} on a card");
        }
        // The card's own one-shots: flatten every band, cut forty places, play
        // them fast, close. None of them on a hold.
        for key in ["r", "enter", "f", "1", "escape"] {
            assert!(!repeats(Repeat::Card, key, None), "{key} on a card");
        }
        // Outside a card the keymap answers, and only the volume pair is a
        // value being moved.
        assert!(repeats(Repeat::Keymap, "up", Some(ActionId::VolumeUp)));
        assert!(repeats(Repeat::Keymap, "down", Some(ActionId::VolumeDown)));
        // ...and the zoom pair, which runs the view the way they run the level.
        assert!(repeats(Repeat::Keymap, "=", Some(ActionId::ZoomIn)));
        assert!(!repeats(Repeat::Keymap, "0", Some(ActionId::ZoomFit)));
        for action in ActionId::ALL {
            let held = repeats(Repeat::Keymap, "k", Some(action));
            assert_eq!(
                held,
                matches!(
                    action,
                    ActionId::VolumeUp
                        | ActionId::VolumeDown
                        | ActionId::ZoomIn
                        | ActionId::ZoomOut
                ),
                "{action:?} on a hold"
            );
        }
        // An arrow with nothing bound to it moves nothing on the timeline.
        assert!(!repeats(Repeat::Keymap, "left", None));
        // And a stroke being captured, an export, or the overlays: nothing at
        // all, or the hold would bind a key and then fire what it just bound.
        for key in ["up", "left", "escape", "5"] {
            assert!(!repeats(Repeat::Nothing, key, Some(ActionId::VolumeUp)));
        }
    }

    #[test]
    fn the_silence_card_fits_the_smallest_window_and_never_slows_a_silence_down() {
        // The same 640x360 floor, and this card starts below the header: a
        // title and a hint over its [`SILENCE_ROWS`] rows, the count line and
        // the two buttons.
        let (title, hint, count) = (17., 17., 17.);
        let gaps = 6. * 5.;
        let padding = 24.;
        assert!(
            HEADER_H
                + 8.
                + title
                + hint
                + SILENCE_ROWS as f32 * KEYS_ROW_H
                + count
                + KEYS_ROW_H
                + gaps
                + padding
                <= 360.,
            "card too tall"
        );
        // Its rows, its steppers and its buttons are clicked, so WCAG 2.5.8
        // binds them: a stepper is `HIT_MIN` square inside a row of that height.
        assert!(KEYS_ROW_H >= HIT_MIN);
        // ...and the pair of them fits beside the widest value the card prints.
        assert!(2. * HIT_MIN + 4. < COLOR_W / 2., "steppers crowd the value");
        // A "speed-up" is never a slow-down: the rate stops above real time at
        // one end and at what a clip can hold at the other, whatever the keys
        // ask for. A silence played *slower* would make the timeline longer,
        // which is the one thing neither button may do.
        assert!(silence_rate(0) > Speed::NORMAL);
        assert!(silence_rate(1000) > Speed::NORMAL);
        assert_eq!(silence_rate(i32::MAX), Speed::MAX);
        assert_eq!(silence_rate(4000), Speed::MAX);
    }

    /// The choice lists that replaced two click-to-cycle surfaces: every value
    /// on offer at once, exactly one of them marked as the one in force, the
    /// same order the stroke steps through, and the open list inside the
    /// 640x360 floor with every row a `HIT_MIN` target.
    #[test]
    fn a_choice_list_offers_every_value_and_fits_the_smallest_window() {
        // Odd media: its own size is on the ladder, in its place by area, and
        // nothing else moved.
        let native = (1440, 1080);
        let ladder = resolution_ladder(native);
        assert_eq!(
            ladder,
            [
                (3840, 2160),
                (2560, 1440),
                (1920, 1080),
                (1440, 1080),
                (1280, 720),
                (854, 480)
            ]
        );
        for size in RESOLUTIONS {
            assert!(ladder.contains(&size), "{size:?} is not on offer");
        }
        // Media already at a listed size is on the ladder once, not twice.
        assert_eq!(resolution_ladder((1920, 1080)).len(), RESOLUTIONS.len());

        // The rows say the same thing the ladder does, and mark the one in
        // force -- exactly one row, whichever rung the project is on.
        let rows = resolution_choices((1280, 720), native);
        assert_eq!(rows.len(), ladder.len());
        assert_eq!(rows.iter().filter(|(.., picked)| *picked).count(), 1);
        for ((choice, label, detail, picked), size) in rows.iter().zip(&ladder) {
            assert_eq!(*choice, Choice::Size(size.0, size.1));
            assert_eq!(label.as_ref(), format!("{}p", size.1));
            assert!(detail.contains(&format!("{}x{}", size.0, size.1)));
            assert_eq!(*picked, *size == (1280, 720));
        }
        // The media's own size says so: it is the one rung a person cannot read
        // off a number they chose.
        let (.., native_detail, _) = &rows[3];
        assert!(
            native_detail.contains("the media's own"),
            "{native_detail}"
        );
        // A project at a size nobody listed still gets the whole list, with
        // nothing marked rather than a wrong row marked.
        assert!(
            resolution_choices((1000, 1000), native)
                .iter()
                .all(|(.., picked)| !picked)
        );
        // Picking a row means that row, and stepping means the next one: the
        // list and the stroke read the same ladder.
        assert_eq!(next_resolution(ladder[1], native), ladder[2]);

        // The fit list, on a clip: all four policies, in the order the stroke
        // steps through them, the clip's own marked and every row naming the
        // canvas it would place the picture on.
        let mut fit = FITS[0];
        for next in FITS.into_iter().skip(1).chain([FITS[0]]) {
            assert_eq!(next_fit(fit), next, "the stroke skips a policy");
            fit = next;
        }
        let fits = fit_choices(Lane::V1, 3, FITS[2], (1920, 1080));
        assert_eq!(fits.len(), FITS.len());
        assert_eq!(fits.iter().filter(|(.., picked)| *picked).count(), 1);
        assert_eq!(fits[2].0, Choice::Fit(Lane::V1, 3, FITS[2]));
        assert!(fits[2].3, "the clip's own policy is not marked");
        assert!(fits[0].2.contains("1920x1080"), "{}", fits[0].2);

        // The rate list, the other setting the project has of its own: every
        // rate on offer, the media's own cycled in at its place by speed and
        // said so, and the one the timeline is cut at marked. The value carried
        // is the `f64` the engine conforms to, not the rounded label -- 23.976
        // is not 24000/1001, and a rate the timescales cannot name is refused.
        let ntsc = 24_000. / 1001.;
        let rates = frame_rate_ladder(25.);
        assert_eq!(rates.len(), FRAME_RATES.len(), "25 is already on the list");
        let odd = frame_rate_ladder(48.);
        assert_eq!(odd[5], 48., "the media's own, in its place by speed");
        assert_eq!(odd.len(), FRAME_RATES.len() + 1);
        let fps = fps_choices(ntsc, 48.);
        assert_eq!(fps.len(), odd.len());
        assert_eq!(fps.iter().filter(|(.., picked)| *picked).count(), 1);
        assert_eq!(fps[0].0, Choice::Fps(ntsc), "the ratio, not 23.976");
        assert_eq!(fps[0].1.as_ref(), "23.976 fps");
        assert!(fps[0].3, "the rate in force is not marked");
        assert!(fps[5].2.contains("the media's own"), "{}", fps[5].2);
        for (.., detail, _) in &fps {
            assert!(detail.chars().count() < 26, "{detail} loses its tail");
        }

        // The HDR list, the third project setting: all three renditions in the
        // order they brighten, the one in force marked, and every row saying
        // what it is in words that fit beside the label.
        let tones = tone_choices(Preset::Standard);
        assert_eq!(tones.len(), Preset::ALL.len());
        assert_eq!(tones.iter().filter(|(.., picked)| *picked).count(), 1);
        for (row, preset) in tones.iter().zip(Preset::ALL) {
            assert_eq!(row.0, Choice::Tone(preset));
            assert_eq!(row.1.as_ref(), tone_label(preset));
            assert!(!row.2.is_empty(), "{preset:?} says nothing about itself");
            assert!(row.2.chars().count() < 26, "{} loses its tail", row.2);
        }
        assert!(tones[1].3, "the rendition in force is not marked");

        // The open list fits the floor the menus are measured against: the
        // longest of them is the rate ladder with an odd rate cycled in, and it
        // hangs at the pointer with every row on screen. Rows are click targets
        // (WCAG 2.5.8).
        assert!(MENU_ROW_H >= HIT_MIN);
        assert!(odd.len() > ladder.len(), "the longest list moved");
        assert!(MENU_PAD * 2. + odd.len() as f32 * MENU_ROW_H <= 360.);
        let tall = MENU_PAD * 2. + ladder.len() as f32 * MENU_ROW_H;
        assert!(tall <= 360., "the list is taller than the floor");
        assert_eq!(
            menu_at(point(px(600.), px(340.)), size(px(640.), px(360.)), tall),
            (640. - MENU_W, 360. - tall),
            "the list would hang off the smallest window"
        );
    }

    #[test]
    fn the_export_card_fits_the_smallest_window() {
        // Same 640x360 floor the keybindings card is measured against: the
        // capped row list, the two summary lines and the confirm button, under
        // a title and a status line.
        let title = 17.;
        let status = 28.;
        // The head is one line of 11 px at this width -- every field of it,
        // worst case, is 71 characters against the 76 that fit. The tail is
        // budgeted for two: the destination's name is the user's and a long one
        // wraps.
        let summary = 15. + 30.;
        // Six children in the column, so five gaps.
        let gaps = 5. * 2.;
        let padding = 24.;
        assert_eq!(
            EXPORT_FIXED_H,
            title + status + summary + CONTROL_H + 4. + gaps + padding
        );
        assert!(EXPORT_FIXED_H + EXPORT_ROWS_H <= 360., "card too tall");
        // The list grows with a window that has the room -- and never shrinks
        // below the cap that made the floor fit, whatever arithmetic the window
        // hands it.
        let cap = |h: f32| (h - EXPORT_FIXED_H - 24.).max(EXPORT_ROWS_H);
        assert_eq!(cap(360.), EXPORT_ROWS_H);
        assert_eq!(cap(0.), EXPORT_ROWS_H);
        assert!(cap(720.) > EXPORT_ROWS_H);
        assert!(EXPORT_FIXED_H + cap(720.) <= 720.);
        // ...and inside the 640 px floor with the scrim showing either side.
        assert!(EXPORT_W + 2. * 12. <= 640.);
        // The cap is only honest if enough of the list is on screen to read as
        // one -- and the whole format section is: its header and every codec
        // row, so nothing that is picked *first* is behind a scroll.
        let codecs = FORMATS.iter().filter(|(row, ..)| !row.is_empty()).count();
        assert!(EXPORT_ROWS_H / KEYS_ROW_H >= 1. + codecs as f32);
        // Clickable rows, so WCAG 2.5.8 binds them as it binds the panel's --
        // and the bitrate steppers are `HIT_MIN` squares sitting inside a row,
        // which only fits while the row is at least as tall as one.
        assert!(KEYS_ROW_H >= HIT_MIN);
        assert!(CONTROL_H >= HIT_MIN);
        // The dimmed text on the card -- every refusal, every detail, every key
        // in its column -- is body text on `SURFACE` and WCAG 1.4.3 binds it.
        // A dimmed row is drawn in this ink rather than at an opacity, which is
        // what a refusal used to be readable through.
        assert!(
            contrast(INK_DIM, SURFACE) >= 4.5,
            "refusal ink {:.2}",
            contrast(INK_DIM, SURFACE)
        );
        // On the picked row that ink would only be 3.3:1 against the highlight,
        // so the row it lands on lifts its key and detail to `INK`.
        assert!(contrast(INK, SELECTED) >= 4.5);
        assert!(
            contrast(INK_DIM, SELECTED) < 4.5,
            "the lift is still needed"
        );
    }

    /// The progress line's two clocks, driven the way a repaint drives them:
    /// steady work, a stall where hardware hands over to software, then steady
    /// work again. The estimate may not whipsaw, may not vanish once it has
    /// been given, and must meet the elapsed clock at the end.
    #[test]
    fn the_export_estimate_rides_out_a_stall_and_converges() {
        let (mut marks, mut elapsed, mut progress) = (Vec::new(), 0f32, 0f32);
        // 2%/s, a 12 s stall at 40% -- longer than the window, so the window
        // alone would have nothing left to measure -- and the same rate to the
        // end: 62 s of wall clock for 50 s of work.
        let rate = 0.02;
        let (mut quiet, mut before_stall, mut after_stall, mut last) = (0f32, 0., 0., f32::MAX);
        while progress < 1. {
            let stalled = (20. ..32.).contains(&elapsed);
            note_progress(&mut marks, elapsed, progress);
            // What is really left, to hold every guess against.
            let truth = (1. - progress) / rate + if stalled { 32. - elapsed } else { 0. };
            match eta_secs(&marks, elapsed, progress) {
                // "estimating…" is only allowed before there is a span to
                // measure, and never again after the first number.
                None => {
                    assert!(elapsed < ETA_SPAN + 1., "estimate vanished at {elapsed}");
                    quiet = elapsed;
                }
                Some(left) => {
                    // No guess is ever wilder than four times the truth: that
                    // is the eightfold spike a raw window rate throws on either
                    // edge of the stall.
                    assert!(left <= truth * 4., "at {elapsed}s: {left} vs {truth}");
                    // While the rate holds, the answer is the true one and it
                    // only ever counts down.
                    if elapsed >= 5. && elapsed < 20. {
                        assert!((left - truth).abs() <= truth * 0.15, "{elapsed}s: {left}");
                        assert!(left < last, "estimate grew while the rate held");
                    }
                    // Eight seconds past the stall it has caught up again.
                    if elapsed >= 40. {
                        assert!((left - truth).abs() <= truth * 0.25, "{elapsed}s: {left}");
                        assert!(left < last + 0.001, "estimate grew after the stall");
                    }
                    if (20. ..20.25).contains(&elapsed) {
                        before_stall = left;
                    }
                    if (31.5..31.75).contains(&elapsed) {
                        after_stall = left;
                    }
                    last = left;
                }
            }
            // A window's worth of marks and no more, however long the encode.
            assert!(marks.len() <= 20, "{} marks", marks.len());
            elapsed += 0.25;
            if !stalled {
                progress = (progress + rate * 0.25).min(1.);
            }
        }
        assert!(quiet > 0. && quiet < ETA_SPAN + 1.);
        // The stall stretched the guess instead of erasing it, and by more than
        // the stopped clock adds on its own.
        assert!(
            after_stall > before_stall + 12.,
            "{before_stall} -> {after_stall}"
        );
        // Both clocks meet: a finished pass has nothing left.
        assert_eq!(eta_secs(&marks, elapsed, 1.), Some(0.));
        assert!((elapsed - 62.).abs() < 1., "{elapsed}");
        assert_eq!(clock(83.4), "1:23");
        assert_eq!(clock(114.), "1:54");
        assert_eq!(clock(-1.), "0:00");
    }

    /// The import line's own state machine, driven the way a repaint drives
    /// it: the worker writes a stage, the poll notices it changed and restarts
    /// the stall clock, and the line's words change only when a stage has
    /// actually stood still. The whole point is that a stuck read reads as a
    /// stuck read and never as a frozen window.
    #[test]
    fn an_import_line_says_a_stage_has_stopped_moving_and_only_then() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
        let stage = Arc::new(AtomicU8::new(ImportStage::Header as u8));
        let started = Instant::now() - Duration::from_secs(9);
        let mut import = Import {
            path: PathBuf::from("/films/A Film.mkv"),
            started,
            stage: Arc::clone(&stage),
            seen: ImportStage::Header,
            // As if the header had been running for those nine seconds.
            since: started,
        };
        // Nine seconds inside one stage: past the wait a person tolerates, so
        // the line stops pretending it is a progress line.
        let since = import.poll();
        assert!(since > IMPORT_STALL, "{since}");
        let stalled = import_line("A Film.mkv", import.seen, 9., since, 0, false);
        assert!(stalled.contains("still reading the header"), "{stalled}");
        assert!(stalled.contains("not frozen"), "{stalled}");
        assert!(stalled.contains("0:09 elapsed"), "{stalled}");
        // The worker moves on: the stall clock restarts even though the elapsed
        // one does not, which is the distinction the two clocks exist for.
        stage.store(ImportStage::Subtitles as u8, Relaxed);
        let since = import.poll();
        assert_eq!(import.seen, ImportStage::Subtitles);
        assert!(since < IMPORT_STALL, "{since}");
        let moving = import_line("A Film.mkv", import.seen, 9., since, 2, false);
        assert!(
            moving.starts_with("IMPORTING A Film.mkv · reading the subtitle tracks"),
            "{moving}"
        );
        assert!(!moving.contains("still"), "{moving}");
        // ...and what is behind it in the queue, which is what a drop of three
        // files or an argv of three files leaves waiting.
        assert!(moving.ends_with("· 2 more waiting"), "{moving}");
        assert!(!stalled.ends_with("waiting"), "an empty queue says nothing");
        // A stage that does not move is not the same fact as a stage that came
        // back to the same value: the poll only restarts on a change.
        let held = import.since;
        import.poll();
        assert_eq!(import.since, held, "an unchanged stage must not reset it");
    }

    /// The launch, from argv to the window being up: nothing named on the
    /// command line is read before there is a window, so all a launch does is
    /// sort argv into the file that becomes the timeline and the queue that is
    /// read through -- and a run with no arguments has neither, exactly as it
    /// had none before.
    #[test]
    fn a_launch_queues_the_file_it_names_instead_of_opening_it() {
        let film = PathBuf::from("/films/Dune.mkv");
        let extra = PathBuf::from("/films/Titles.mov");
        let (arg, queue) = launch_queue([film.clone(), extra.clone()].into_iter());
        assert_eq!(arg.as_deref(), Some(film.as_path()), "argv[1] is the open");
        // In the order they were named, the timeline's file first: six header
        // walks racing over one disk finish no sooner than six in a row, and
        // the one the person is waiting to watch goes first.
        assert_eq!(queue, [film, extra], "argv, in arrival order");
        // ...and no argument at all is still the empty window: nothing to open
        // and nothing to read.
        let (arg, queue) = launch_queue(std::iter::empty());
        assert_eq!(arg, None);
        assert!(queue.is_empty(), "no argv, nothing queued");
    }

    /// Which door a queued file goes through. One queue carries the file argv
    /// named and every import behind it, so this fork -- made when the worker
    /// starts and carried to the landing as a [`Landed`] -- is the whole state
    /// machine: the named file becomes the timeline (a `.edith` restoring a
    /// whole one), everything else joins the library, including the very same
    /// film dropped again once the open has landed.
    #[test]
    fn the_file_argv_named_lands_as_the_timeline_and_everything_behind_it_as_imports() {
        let film = PathBuf::from("/films/Dune.mkv");
        let extra = PathBuf::from("/films/Titles.mov");
        let project = PathBuf::from("/films/Dune.edith");
        assert_eq!(arrival(Some(&film), &film), Landing::Open);
        assert_eq!(arrival(Some(&project), &project), Landing::Project);
        assert_eq!(arrival(Some(&film), &extra), Landing::Import);
        // Cleared as it lands: a drop of the film already on the timeline is an
        // import, which is what a drop has always been.
        assert_eq!(arrival(None, &film), Landing::Import);
        // A window opened empty has no named file at all, and everything that
        // arrives at it is an import.
        assert_eq!(arrival(None, &project), Landing::Import);
    }

    /// What the window says while the file argv named is being read: the file's
    /// name, the read that is running, and a clock proving the window is
    /// answering -- in the *opening* wording, because the one person who typed
    /// that name is not importing anything.
    #[test]
    fn the_named_file_is_read_under_an_opening_line_not_an_importing_one() {
        let opening = import_line("Dune.mkv", ImportStage::Header, 0.4, 0.4, 1, true);
        assert!(
            opening.starts_with("OPENING Dune.mkv · reading the header"),
            "{opening}"
        );
        assert!(opening.ends_with("· 1 more waiting"), "{opening}");
        // Twelve seconds into a cold 25 GB header walk, which is the whole
        // reason the window is up: it says the read is still moving through the
        // same stage and that this is not a freeze.
        let stalled = import_line("Dune.mkv", ImportStage::Header, 12., 12., 0, true);
        assert!(stalled.starts_with("OPENING Dune.mkv · still"), "{stalled}");
        assert!(stalled.contains("not frozen"), "{stalled}");
        assert!(stalled.contains("0:12 elapsed"), "{stalled}");
        // The files behind it are imports and still say so.
        let import = import_line("Titles.mov", ImportStage::Header, 0.4, 0.4, 0, false);
        assert!(import.starts_with("IMPORTING Titles.mov"), "{import}");
    }

    /// One gesture over the real gate, with the plumbing around it spelled out:
    /// a write reseeks (the worker owes a frame again), a landed frame clears
    /// that and flushes what is held, and the release flushes whatever the
    /// worker is doing. Forty snapped steps and four frames delivered must cost
    /// five writes -- the press's and one per frame -- and not forty, which is
    /// the 22-30 s freeze this exists to remove.
    #[test]
    fn a_bar_wide_sweep_writes_once_per_frame_delivered() {
        let mut stash: Option<i32> = None;
        let mut written = Vec::new();
        let mut busy = false;
        for step in 0..40 {
            if let Some(value) = stash_or_write(&mut stash, step, step == 0, busy) {
                // What `write_color` does: the write supersedes the stash and
                // reseeks, so the worker owes a frame from here.
                stash = None;
                written.push(value);
                busy = true;
            }
            // A frame lands every tenth sample: `pump` clears the seek and the
            // render flushes what the drag held back.
            if step % 10 == 9 {
                busy = false;
                if let Some(value) = stash.take() {
                    written.push(value);
                    busy = true;
                }
            }
        }
        // The release, whatever the worker is doing.
        written.extend(stash.take());
        assert_eq!(written, vec![0, 9, 19, 29, 39], "one write per frame landed");
    }

    /// The one value a gesture may never lose: where the hand let go. The
    /// release samples into a busy worker -- so the sample is held -- and the
    /// flush behind it is what writes it.
    #[test]
    fn a_release_lands_the_value_the_hand_let_go_on() {
        let mut stash = None;
        assert_eq!(stash_or_write(&mut stash, 7, false, true), None);
        assert_eq!(stash_or_write(&mut stash, 11, false, true), None);
        assert_eq!(stash.take(), Some(11), "the release writes the last sample");
        // The press is never held: it is the undo step the gesture rolls back
        // to, and one taken a frame late is a snapshot of the wrong grade.
        assert_eq!(stash_or_write(&mut stash, 3, true, true), Some(3));
        assert_eq!(stash, None);
        // Nothing to hold when the worker is idle: the write goes straight out.
        assert_eq!(stash_or_write(&mut stash, 5, false, false), Some(5));
        assert_eq!(stash, None);
    }

    /// A seek says nothing until it has stood: an ordinary one is a flicker and
    /// a cold read of a big file is the case worth words.
    #[test]
    fn a_seek_says_so_only_once_it_has_stood() {
        assert_eq!(seek_line(None), None, "no seek, no line");
        assert_eq!(seek_line(Some(Duration::from_millis(300))), None);
        let line = seek_line(Some(SEEK_STALL + Duration::from_secs(7))).expect("past the stall");
        assert!(line.contains("still opening the picture"), "{line}");
        assert!(line.contains("not frozen"), "{line}");
        assert!(line.contains("0:09 elapsed"), "{line}");
    }

    /// The silence card's own state machine, driven the way a repaint drives
    /// it: a worker moves its mark, the poll notices and restarts the stall
    /// clock, and the line says a read has stopped only when it actually has.
    /// The card is up through all of it -- that is the whole change, since the
    /// same decode used to run on the render thread and hold the frame for
    /// fifty-one seconds on a 25 GB film.
    #[test]
    fn a_silence_card_is_up_while_its_scan_runs_and_says_where_it_has_got_to() {
        use std::sync::Arc;
        use std::sync::atomic::Ordering::Relaxed;
        let progress = Arc::new(engine::silence::Progress::default());
        // Two hours and eight minutes, as the header claims it.
        progress.total.store(7680, Relaxed);
        let started = Instant::now() - Duration::from_secs(9);
        let mut scan = SilenceScan {
            key: (PathBuf::from("/films/A Film.mkv"), 0),
            started,
            progress: Arc::clone(&progress),
            seen: 0,
            since: started,
        };
        // Nine seconds and the mark has not moved: past the wait a person
        // tolerates, so the line stops pretending it is a progress line.
        let since = scan.poll();
        assert!(since > IMPORT_STALL, "{since}");
        let stalled = silence_line(0., 768., 9., since);
        assert!(stalled.contains("still reading the sound"), "{stalled}");
        assert!(stalled.contains("not frozen"), "{stalled}");
        assert!(stalled.contains("0:00 of ~12:48 scanned"), "{stalled}");
        assert!(stalled.contains("0:09 elapsed"), "{stalled}");
        // The worker reports, and the stall clock restarts even though the
        // elapsed one does not -- the two clocks' whole distinction.
        let mut last = 0;
        for deci in [83, 1_140, 4_002] {
            progress.scanned.store(deci, Relaxed);
            let since = scan.poll();
            assert!(since < IMPORT_STALL, "{since}");
            assert!(scan.seen > last, "{} after {last}", scan.seen);
            last = scan.seen;
        }
        let moving = silence_line(scan.seen as f32 / 10., 768., 9., 0.2);
        assert_eq!(moving, "SCANNING · 6:40 of ~12:48 scanned · 0:09 elapsed");
        assert!(!moving.contains("still"), "{moving}");
        // A header that does not say how long the track is says nothing rather
        // than guessing at it.
        let unknown = silence_line(60., 0., 61., 0.2);
        assert_eq!(unknown, "SCANNING · 1:00 scanned · 1:01 elapsed");
        // A mark that comes back the same is not the same fact as one that
        // moved: the poll only restarts on a change.
        let held = scan.since;
        scan.poll();
        assert_eq!(scan.since, held, "an unchanged mark must not reset it");
    }

    /// The cache is per source, which is what stops two films thrashing each
    /// other's fifty seconds: A, then B, then A again is *one* decode of A.
    /// And a source already being read is waited for rather than read twice --
    /// both halves of an A/V take name the same file.
    #[test]
    fn a_second_film_does_not_cost_the_first_one_its_levels() {
        let (a, b) = (
            (PathBuf::from("/films/a.mkv"), 0),
            (PathBuf::from("/films/b.mkv"), 0),
        );
        let mut cache: std::collections::HashMap<(PathBuf, usize), ()> =
            std::collections::HashMap::new();
        let mut started = Vec::new();
        // What a card open does, three times over, with the worker landing
        // between each: plan, and start what the plan says to start.
        for key in [&a, &b, &a] {
            match scan_plan(cache.contains_key(key), None, key) {
                ScanPlan::Start => {
                    started.push(key.clone());
                    cache.insert(key.clone(), ());
                }
                ScanPlan::Marks => {}
                ScanPlan::Wait => unreachable!("nothing is running"),
            }
        }
        assert_eq!(started, vec![a.clone(), b.clone()], "A was decoded twice");
        // The single-slot cache this replaced would have evicted A when B
        // landed; both are held.
        assert!(cache.contains_key(&a) && cache.contains_key(&b));
        // A scan in flight on the same source is joined, not restarted -- and
        // one on another source is not waited for.
        assert_eq!(scan_plan(false, Some(&a), &a), ScanPlan::Wait);
        assert_eq!(scan_plan(false, Some(&b), &a), ScanPlan::Start);
        // Levels in hand beat a worker either way: the marks are arithmetic.
        assert_eq!(scan_plan(true, Some(&a), &a), ScanPlan::Marks);
    }

    /// The read-ahead is a *cache warmer* and nothing else: whatever it did or
    /// failed to do, the import that follows lands exactly the rows, lengths
    /// and refusals it landed before there was a worker at all.
    #[test]
    fn reading_ahead_changes_nothing_about_what_an_import_lands() {
        use std::sync::atomic::AtomicU8;
        let stage = AtomicU8::new(ImportStage::Header as u8);
        let plain = {
            let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
            session.set_gain(0.0);
            session.import(&asset("test_av2.mp4")).expect("av2 matches");
            (
                session.sources().to_vec(),
                session.file_frames(&asset("test_av2.mp4")),
                session.timeline_duration(),
            )
        };
        let warmed = {
            let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
            session.set_gain(0.0);
            let subs = read_ahead(&asset("test_av2.mp4"), &stage).expect("an mp4 is readable");
            // This mp4 is walked now (an mp4 can carry `tx3g`) and has no
            // subtitle track in it, so the worker hands over an empty list --
            // the same thing the import lands with.
            assert!(subs.is_empty());
            session.import(&asset("test_av2.mp4")).expect("av2 matches");
            (
                session.sources().to_vec(),
                session.file_frames(&asset("test_av2.mp4")),
                session.timeline_duration(),
            )
        };
        assert_eq!(plain, warmed);
        // ...and it leaves the stage where the line can read it: a worker that
        // never announced its second read would show one that never ends.
        assert_eq!(
            ImportStage::from_u8(stage.load(std::sync::atomic::Ordering::Relaxed)),
            ImportStage::Subtitles
        );
        // A refusal is still the engine's refusal, warmed or not: a file of
        // another rate is no more importable for having been read first.
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
        session.set_gain(0.0);
        drop(read_ahead(&asset("test_25fps.mp4"), &stage));
        let warmed = session.import(&asset("test_25fps.mp4"));
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
        session.set_gain(0.0);
        assert_eq!(
            warmed.map_err(|e| e.to_string()),
            session
                .import(&asset("test_25fps.mp4"))
                .map_err(|e| e.to_string())
        );
        // A path nothing can read is the worker's business to survive, not to
        // report: the engine says so a moment later, in its own words. The
        // subtitle half *is* carried back, because nothing walks it again to
        // re-word it -- and it is a refusal, not a panic.
        assert!(
            read_ahead(std::path::Path::new("/no/such/film.mkv"), &stage).is_err(),
            "an unreadable mkv comes back as the engine's refusal"
        );
    }

    /// The import door's two halves, split across the worker hop: what
    /// [`read_ahead`] walks is exactly what the render thread would have walked,
    /// and pushing it says what walking it in place used to say.
    #[test]
    fn an_imports_subtitles_are_read_by_the_worker_and_only_pushed_here() {
        use std::sync::atomic::AtomicU8;
        let stage = AtomicU8::new(ImportStage::Header as u8);
        let film = asset("test_subs.mkv");
        // The in-place walk (`subtitle_notice`) and the split one land the same
        // tracks and the same tail on the same timeline.
        let mut in_place = PlaybackSession::open(asset("test_av.mp4")).expect("open");
        in_place.set_gain(0.0);
        let said = subtitle_notice(&mut in_place, &film);
        let mut split = PlaybackSession::open(asset("test_av.mp4")).expect("open");
        split.set_gain(0.0);
        let walked = read_ahead(&film, &stage).expect("the mkv is readable");
        assert_eq!(subtitle_tail(&mut split, Ok(walked)), said);
        assert_eq!(split.subtitles().len(), in_place.subtitles().len());
        // The same file twice is still one row, and still says so: the dedupe
        // lives in the push, which is the half that stayed here.
        let again = read_ahead(&film, &stage).expect("the mkv is readable");
        assert_eq!(subtitle_tail(&mut split, Ok(again)), None);
        // A standalone `.srt` is walked by the same worker: an import of one is
        // not a door that reads on the render thread either.
        let srt = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../engine/tests/data/test_subs.srt")
            .canonicalize()
            .expect("the subtitle fixture");
        assert_eq!(
            read_ahead(&srt, &stage).expect("the srt is readable").len(),
            1
        );
        // ...and a refusal is worded as a tail, never as a failed import.
        let unread: Subs = Err("nothing to read".into());
        assert_eq!(
            subtitle_tail(&mut split, unread),
            Some(" — SUBTITLES UNREAD: nothing to read".to_string())
        );
    }

    #[test]
    fn every_codec_row_is_offered_or_says_why_not() {
        // One row per codec, and the boxes it can go in are the container row's
        // business: seven rows that pick, one that says why it cannot. A codec
        // twice over (AV1 · MKV beside AV1 · MP4) was two rows asking the same
        // question, and five picture rows above the fold is what the card was
        // called unfriendly for.
        let offered: Vec<&[Format]> = FORMATS
            .iter()
            .map(|&(row, ..)| row)
            .filter(|row| !row.is_empty())
            .collect();
        assert_eq!(
            offered,
            vec![
                &[Format::Mp4][..],
                &[Format::Av1, Format::Av1Mp4][..],
                &[Format::Hevc, Format::HevcMp4][..],
                &[Format::Wav][..],
                &[Format::Flac][..],
                &[Format::Mp3][..],
                &[Format::Ogg][..],
            ]
        );
        // Every format the engine writes is on exactly one row: one the card
        // cannot reach is one nobody can pick.
        for format in [
            Format::Mp4,
            Format::Av1,
            Format::Av1Mp4,
            Format::Hevc,
            Format::HevcMp4,
            Format::Wav,
            Format::Flac,
            Format::Mp3,
            Format::Ogg,
        ] {
            assert_eq!(
                FORMATS
                    .iter()
                    .filter(|(row, ..)| row.contains(&format))
                    .count(),
                1,
                "{format:?}"
            );
        }
        // The boxes the container row offers, and what the extension of each is
        // -- the destination follows the format, so these are what a file gets
        // named with.
        assert_eq!(containers(Format::Av1), [Format::Av1, Format::Av1Mp4]);
        assert_eq!(containers(Format::HevcMp4), [Format::Hevc, Format::HevcMp4]);
        assert_eq!(containers(Format::Mp3), [Format::Mp3]);
        assert_eq!(Format::Av1.ext(), "mkv");
        assert_eq!(Format::Av1Mp4.ext(), "mp4");
        assert_eq!(Format::Hevc.ext(), "mkv");
        assert_eq!(Format::HevcMp4.ext(), "mp4");
        // The container key walks the row and wraps, and does nothing at all
        // where there is only one box -- a stroke must not invent a choice the
        // card is not offering.
        assert_eq!(next_container(Format::Av1), Format::Av1Mp4);
        assert_eq!(next_container(Format::Av1Mp4), Format::Av1);
        assert_eq!(next_container(Format::Hevc), Format::HevcMp4);
        assert_eq!(next_container(Format::Mp4), Format::Mp4);
        assert_eq!(next_container(Format::Wav), Format::Wav);
        for (row, stroke, label, detail) in FORMATS {
            assert!(!label.is_empty(), "a row with no name");
            // A refused row is a row with a reason, never a hidden one: an
            // empty detail column would read as an oversight.
            assert!(!detail.is_empty(), "{label} says nothing");
            // Every row that can be picked has a key of its own, and no row
            // that cannot has one: the card is drivable without a pointer.
            assert_eq!(!row.is_empty(), !stroke.is_empty(), "{label}");
            if let Some(&first) = row.first() {
                assert_eq!(
                    format_key(stroke, first),
                    Some(first),
                    "{label} keys to itself"
                );
                assert!(
                    stroke.parse::<u32>().is_err(),
                    "{label} takes a digit the bitrate needs"
                );
                // Every box on one row is the same codec, so the quality rows
                // do not change meaning when the container does.
                assert!(row.iter().all(|f| f.has_video() == first.has_video()));
                assert!(
                    row.iter()
                        .all(|f| bitrate_refusal(*f) == bitrate_refusal(first))
                );
            }
        }
        // No two rows share a key, and none of them is a stroke the card already
        // answers to itself -- an ambiguous key is a key that picks the wrong
        // thing on a card that has no other input.
        let keys: Vec<&str> = FORMATS
            .iter()
            .map(|&(_, stroke, ..)| stroke)
            .filter(|stroke| !stroke.is_empty())
            .collect();
        for (i, key) in keys.iter().enumerate() {
            assert!(!keys[i + 1..].contains(key), "{key} picks two rows");
            assert!(
                !["c", "q", "d", "g", "r", "enter", "backspace", ESCAPE].contains(key),
                "{key} is already the card's own"
            );
        }
        assert_eq!(
            format_key("a", Format::Mp4),
            Some(Format::Av1Mp4),
            "the box already chosen is kept"
        );
        assert_eq!(
            format_key("a", Format::Wav),
            Some(Format::Av1),
            "and a codec with no such box takes its first"
        );
        assert_eq!(format_key("h", Format::Av1Mp4), Some(Format::HevcMp4));
        assert_eq!(format_key("p", Format::Mp4), Some(Format::Mp3));
        assert_eq!(
            format_key("m", Format::Mp3),
            Some(Format::Mp4),
            "not MP3, which is p"
        );
        assert_eq!(
            format_key("x", Format::Mp4),
            None,
            "a stroke no row carries"
        );
        assert_eq!(format_key("o", Format::Mp4), Some(Format::Ogg));
        // The one codec left that this program reads and cannot write is a row
        // of its own, refused by name rather than absent: VP9, because AV1 is
        // the row that replaced it. Its reason travels with it, in the row or in
        // the footer line that collects them -- either way it is on screen
        // without a click. OGG was the other one until `rusty_vorbis` gave this
        // project an encoder, and the row that says so is the row above.
        let (row, _, _, detail) = FORMATS
            .into_iter()
            .find(|(_, _, name, _)| *name == "VP9")
            .expect("VP9 has a row");
        assert!(row.is_empty(), "VP9 is not offered");
        assert!(detail.contains("replaces it"), "VP9: {detail}");
        let (row, _, _, detail) = FORMATS
            .into_iter()
            .find(|(_, _, name, _)| *name == "OGG")
            .expect("OGG has a row");
        assert_eq!(row, [Format::Ogg], "OGG is a row that picks now");
        assert!(
            detail.contains("rusty_vorbis"),
            "the row names the encoder like every other live one: {detail}"
        );
        // Both AV1 boxes say they carry sound: the file used to be picture only,
        // and a line that still said so would be the lie a user plays the file
        // to find out about. HEVC says intra-only before anyone waits on one --
        // a file several times the size, which the disk would otherwise say.
        for format in [Format::Av1, Format::Av1Mp4] {
            assert!(format_line(format).starts_with("AV1 · "));
        }
        for format in [Format::Hevc, Format::HevcMp4] {
            assert!(format_line(format).starts_with("HEVC intra · "));
        }
        // The head names the box every format goes in, which is what the
        // destination is then named after.
        for format in [
            Format::Mp4,
            Format::Av1,
            Format::Av1Mp4,
            Format::Hevc,
            Format::HevcMp4,
        ] {
            assert!(
                format_line(format).contains(&format.ext().to_uppercase()),
                "{format:?}: {}",
                format_line(format)
            );
        }
        // ...and it stays inside the one line the card budgets for it: the
        // longest of them, with every field after it, against the 76 characters
        // that fit at `EXPORT_W`.
        let longest = summary_head(
            Format::Hevc,
            Some(((1920, 1080), 23.976)),
            "AAC · SW encode (rusty_aac)",
        );
        assert!(
            longest.chars().count() <= 76,
            "{longest} is {} long",
            longest.chars().count()
        );
        assert!(
            FORMATS
                .into_iter()
                .any(|(row, _, _, detail)| row.contains(&Format::Hevc) && detail.contains("intra"))
        );
        // Only a picture encoder is given a bitrate, and the quality rows dim
        // with the reason for every format that is not one.
        for format in [
            Format::Mp4,
            Format::Av1,
            Format::Av1Mp4,
            Format::Hevc,
            Format::HevcMp4,
        ] {
            assert!(
                format.has_video() && bitrate_refusal(format).is_none(),
                "{format:?}"
            );
        }
        for format in [Format::Wav, Format::Flac, Format::Mp3] {
            assert!(!format.has_video());
            assert!(
                bitrate_refusal(format).is_some(),
                "{format:?} dims silently"
            );
        }
        assert!(bitrate_refusal(Format::Wav).unwrap().contains("lossless"));
        // MP3 has a rate and it is the *Sound* row's: the quality rows are the
        // picture's, and this refusal used to claim a fixed 256 kbps that the
        // Sound row can now change under it.
        assert!(bitrate_refusal(Format::Mp3).unwrap().contains("Sound row"));
        assert!(
            !format_line(Format::Mp3).contains("256"),
            "the summary states a rate the Sound row can change under it"
        );
        // The destination follows the format and keeps the stem, mp4 included.
        assert_eq!(
            retarget(std::path::Path::new("/a/take.export.mp4"), Format::Wav),
            std::path::Path::new("/a/take.export.wav")
        );
        assert_eq!(
            retarget(std::path::Path::new("/a/take.export.wav"), Format::Mp4),
            std::path::Path::new("/a/take.export.mp4")
        );
        assert!(format_line(Format::Flac).contains("lossless"));
    }

    /// The one line the card is answerable for: what it says is on screen before
    /// the button is pressed, and every field of it is one `ffprobe` reads back
    /// off the file that comes out.
    #[test]
    fn the_summary_states_the_file_before_it_is_written() {
        let head = summary_head(Format::Mp4, Some(((1920, 1080), 30.)), "AAC copy");
        for field in ["H.264", "MP4", "1920x1080", "30 fps", "AAC copy"] {
            assert!(head.contains(field), "{field} missing from {head}");
        }
        // The rate as a person writes it, and the ratio one spelled out rather
        // than rounded to a rate nothing is written at.
        assert_eq!(fps_label(30.), "30");
        assert_eq!(fps_label(24000. / 1001.), "23.976");
        assert_eq!(fps_label(29.97002997), "29.97");
        // A format with no picture states no size and no rate it does not write.
        let audio = summary_head(Format::Wav, Some(((1920, 1080), 30.)), "PCM · SW (hound)");
        assert!(
            !audio.contains("1920x1080") && !audio.contains("fps"),
            "{audio}"
        );
        assert!(audio.contains("PCM · SW (hound)"));
        // ...and one with no sound on the timeline says that, rather than
        // leaving the field out and reading as a file with sound in it.
        assert!(
            summary_head(Format::Mp4, Some(((640, 360), 25.)), "no sound to write")
                .contains("no sound to write")
        );
        // The tail: where it lands, about how big, and what will encode it --
        // never a guessed seat, and no seat at all for a format with no picture.
        let tail = summary_tail(
            Path::new("/a/take.export.mp4"),
            Some(45_000_000),
            Some("VA-API"),
            true,
        );
        assert!(tail.starts_with("take.export.mp4"), "{tail}");
        assert!(
            tail.contains("≈ 45 MB") && tail.contains("VA-API"),
            "{tail}"
        );
        assert!(summary_tail(Path::new("/a/x.mp4"), None, None, true).contains("encoder …"));
        assert!(!summary_tail(Path::new("/a/x.wav"), None, None, false).contains("encoder"));
        assert!(!summary_tail(Path::new("/a/x.wav"), None, None, false).contains("MB"));
        // 6 Mbps over a minute is 45 MB. `Auto` has no figure to estimate from
        // and an empty timeline no length: neither invents one.
        assert_eq!(estimated_bytes(Some(6_000_000), 60.), Some(45_000_000));
        assert_eq!(estimated_bytes(Some(2_000_000), 90.), Some(22_500_000));
        assert_eq!(estimated_bytes(None, 60.), None);
        assert_eq!(estimated_bytes(Some(6_000_000), 0.), None);
        assert_eq!(estimated_bytes(Some(0), 60.), None);
        // ...and a short one is not nothing. Three seconds at the floor
        // bitrate is 375 kB, which used to round to the "≈ 0 MB" this line
        // exists to never say -- the shortest export a frame can make is
        // still a real file with a real size.
        let short = estimated_bytes(Some(1_000_000), 3.).expect("a rate and a length");
        assert_eq!(size_label(short), "375 kB");
        let frame = summary_tail(Path::new("/a/x.mp4"), estimated_bytes(Some(1_000_000), 1. / 60.), None, true);
        assert!(
            frame.contains("≈ 2 kB") && !frame.contains("0 MB") && !frame.contains("0 kB"),
            "{frame}"
        );
        // The boundary reads in the unit that can state it, either side.
        assert_eq!(size_label(999_600), "1 MB");
        assert_eq!(size_label(499_000), "499 kB");
        assert_eq!(size_label(1), "1 kB");
    }

    /// Which cue is on screen when: the whole of what the overlay decides, and
    /// the one piece of it that is arithmetic rather than layout.
    #[test]
    fn a_cue_is_on_screen_from_its_start_until_the_moment_it_ends() {
        use engine::subtitle::Cue;
        let cue = |start_us, end_us, text: &str| Cue {
            start_us,
            end_us,
            text: text.to_string(),
            image: None,
        };
        // Two cues that hand over exactly, and one that overlaps the second --
        // a sign over a line of dialogue, which is two plates at one moment.
        let cues = [
            cue(500_000, 1_500_000, "first line"),
            cue(1_500_000, 2_500_000, "second line"),
            cue(2_000_000, 2_200_000, "a sign"),
        ];
        let at = |t: f64| -> Vec<&str> {
            cues_at(&cues, t)
                .into_iter()
                .map(|c| c.text.as_str())
                .collect()
        };
        // Before the first, between none of them: nothing, which is what makes
        // the overlay disappear rather than sit there empty.
        assert!(at(0.).is_empty());
        assert!(at(0.4).is_empty());
        assert!(at(3.).is_empty());
        assert_eq!(at(0.5), ["first line"], "on at its own start");
        assert_eq!(at(1.4), ["first line"]);
        // Half-open: the frame the first ends on is the second's, never both.
        assert_eq!(at(1.5), ["second line"]);
        assert_eq!(at(2.1), ["second line", "a sign"], "two at once stack");
        assert_eq!(at(2.2), ["second line"], "the sign is over");
        // ...and the end of the last one is the end of it.
        assert!(at(2.5).is_empty());
        // A negative time is before everything rather than a panic: the
        // playhead is clamped, but nothing here depends on that.
        assert!(at(-1.).is_empty());
    }

    /// The two places a subtitle is drawn -- the plate over the picture and the
    /// strip under the lanes -- inside the 640x360 floor the rest of this window
    /// is sized for, and the plate readable on whatever the film is showing.
    #[test]
    fn the_subtitle_plate_and_strip_fit_the_smallest_window() {
        // The strip costs the panel its own row and the panel's gap, and costs
        // nothing at all when there is no track to draw.
        assert_eq!(subtitle_strip_h(false), 0.);
        assert_eq!(subtitle_strip_h(true), SUB_LANE_H + 8.);
        // What is left for the picture at the floor, with the panel a project
        // starts with and the strip under it.
        let picture = 360. - HEADER_H - panel_h(2) - subtitle_strip_h(true);
        assert!(picture > 0., "the strip pushed the picture off the window");
        // A two-line cue and the gap under it fit inside that, which is the
        // whole claim: the plate sits *over* the picture and must not need more
        // of it than there is.
        assert!(
            SUB_BOTTOM + 2. * SUB_LINE_H <= picture,
            "a two-line cue does not fit the smallest picture"
        );
        // The text sits on its line rather than being clipped by it.
        assert!(SUB_LINE_H >= SUB_TEXT);
        // White on the plate, not chrome on chrome: a cue is read against the
        // film, and this is the one pair here that has to survive any picture.
        assert!(contrast(SUB_INK, 0x000000) >= 7.);
        // The strip is a picture and not a target -- nothing on it takes a
        // click -- which is the only reason it may be under `HIT_MIN`.
        assert!(SUB_LANE_H < HIT_MIN && SUB_LANE_H > 0.);
        // The library's own list of tracks scrolls past three rather than
        // taking the media list's room.
        assert_eq!(SUB_ROWS_H / ROW_H, 3.);
        assert!(ROW_H >= HIT_MIN, "a subtitle row is clicked to pick it");
        // At the floor there are no group headers at all: the list is one row
        // tall there, and a header would name a film and then show none of it.
        // The rows go on naming their own file, so nothing is lost by it.
        assert!(!sub_headers_fit(360.), "a header at the floor eats the track");
        // At the size the window opens on there is room for a header and the
        // tracks under it, which is the only condition it is drawn on.
        assert!(sub_headers_fit(720.));
        // ...and where it is drawn it fits inside the same capped list, header
        // and two rows under it, without the tracks losing their budget.
        assert!(SUB_HEAD_H + 2. * ROW_H <= SUB_ROWS_H);
        // A header takes no click -- there is nothing to fold and nothing to
        // aim at -- which is the only reason it may be under `HIT_MIN`.
        assert!(SUB_HEAD_H < HIT_MIN && SUB_HEAD_H > 0.);
    }

    /// What the rows of both lists do with a name too long for the column: two
    /// episodes off one release differ in their last two characters, and a name
    /// cut from the right is the same row twice.
    #[test]
    fn a_long_name_is_cut_out_of_its_middle_so_two_episodes_stay_two_rows() {
        let (a, b) = (
            "A Long Release Name Of A Series 01",
            "A Long Release Name Of A Series 02",
        );
        // The two call sites at the narrowest the column ever is: a media row's
        // name gets the row's whole width, a subtitle row's file prefix gets a
        // share of it because the language has to fit beside it.
        let media = row_text_w(LIBRARY_MIN_W);
        let prefix = media * SUB_STEM_SHARE;
        for width in [media, prefix] {
            assert!(width > 0., "the floor leaves a row no words at all");
            assert_ne!(
                clip_middle(a, width),
                clip_middle(b, width),
                "{width}px: two episodes read as one row"
            );
            // Both ends survive: the release's name at the front, the number
            // that tells them apart at the back.
            assert!(clip_middle(a, width).starts_with(&a[..2]));
            assert!(clip_middle(a, width).ends_with("01"));
            assert!(clip_middle(a, width).contains('…'));
            // Never wider than the column can hold, gap included.
            assert!(clip_middle(a, width).chars().count() <= (width / 6.) as usize + 1);
        }
        // A name the width holds is left exactly as it is -- no gap, nothing
        // dropped -- and so is a name at any width once the column is wide.
        assert_eq!(clip_middle("eng.srt", 400.), "eng.srt");
        assert_eq!(clip_middle(a, 4000.), a);
        // Nothing panics at a width no column ever has, and something of both
        // ends is still there.
        assert_eq!(clip_middle(a, 0.).chars().count(), 5);
        assert!(clip_middle(a, 0.).ends_with('1'));
        // Counted in characters and not bytes: a name in another script is cut
        // between its letters, not through one.
        assert_eq!(clip_middle("ααααααααααααααα", 24.), "αα…αα");
    }

    /// The two doors subtitles arrive through, end to end on the fixtures: a
    /// file beside the media and the tracks inside an mkv, what the overlay then
    /// says at a given moment, and where the strip draws it.
    #[test]
    fn subtitles_arrive_beside_the_media_and_inside_it() {
        // Which door a path goes through is decided before anything is opened.
        // ...and a `.mks` is one, the subtitles of a Matroska file alone: it has
        // no source in it to import, so the drop door has to send it where `+ S`
        // sends it. Its two siblings are media and must not come here -- a
        // `.mka` is a song this would stop importing.
        for name in ["subs.srt", "SUBS.SRT", "a.vtt", "a.ass", "a.ssa", "s.mks", "S.MKS"] {
            assert!(is_subtitle(Path::new(name)), "{name}");
        }
        for name in ["a.mp4", "a.mkv", "song.mka", "film.mk3d", "notes.txt", "a"] {
            assert!(!is_subtitle(Path::new(name)), "{name}");
        }
        // Every container the engine can walk for tracks inside it, Matroska
        // and ISO-BMFF alike -- an mp4 carries `tx3g`, so the app has to ask it.
        // The Matroska half is the engine's own closed set, extension for
        // extension, so no file is walked here that it would refuse there.
        for name in [
            "film.MKV", "clip.webm", "clip.mp4", "a.m4v", "a.MOV", "song.mka", "s.mks", "f.MK3D",
        ] {
            assert!(carries_subtitles(Path::new(name)), "{name}");
            assert!(
                engine::demux::is_matroska(Path::new(name))
                    || matches!(
                        Path::new(name)
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(str::to_ascii_lowercase)
                            .as_deref(),
                        Some("mp4" | "m4v" | "mov")
                    ),
                "{name} is walked by the app but not by the engine"
            );
        }
        for name in ["song.wav", "still.png", "notes.txt", "a"] {
            assert!(!carries_subtitles(Path::new(name)), "{name}");
        }
        // The three doors, on the one file that used to split them: what the
        // drop/argv door does with it ([`Player::take_import`] routes on
        // `is_subtitle`), what the worker reads for it ([`walk_subtitles`]), and
        // what `+ S` reads ([`Player::add_subtitles`] -> `parse_subtitles`) are
        // the same walk of the same bytes.
        let mks = asset("test_subs.mks");
        assert!(is_subtitle(&mks), "the drop door takes it as subtitles");
        let dropped = walk_subtitles(&mks).expect("the drop door's worker reads it");
        let plus_s = PlaybackSession::parse_subtitles(&mks).expect("`+ S` reads it");
        assert!(!dropped.is_empty(), "and there are tracks in it");
        assert_eq!(
            dropped.iter().map(|t| &t.label).collect::<Vec<_>>(),
            plus_s.iter().map(|t| &t.label).collect::<Vec<_>>(),
        );

        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        // Beside the media: the drop door's own call, and the row it makes.
        let srt = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../engine/tests/data/test_subs.srt")
            .canonicalize()
            .expect("the subtitle fixture");
        assert_eq!(session.import_subtitles(&srt).expect("the .srt imports"), 1);
        let track = &session.subtitles()[0];
        assert_eq!(track.label, "test_subs.srt");
        assert_eq!(subtitle_detail(track), "3 cues");
        // What the overlay draws at a moment inside the first cue, and between
        // two of them -- the fixture's own timings (0.5-1.5 s, 2.0-3.25 s).
        let text = |t: f64| -> Vec<String> {
            cues_at(&track.cues, t)
                .into_iter()
                .map(|c| c.text.clone())
                .collect()
        };
        assert_eq!(text(0.7), ["first line"]);
        assert!(text(1.8).is_empty(), "between two cues the plate goes");
        assert_eq!(text(2.5), ["second line\nwith a break"]);
        // Where the strip puts that first cue: 0.5 s in at 40 px to the second
        // is 20 px along, and a second of cue is 40 px wide.
        let scale = Scale::default();
        assert_eq!(scale.pps, PPS_DEFAULT);
        assert_eq!(cue_box(scale, &track.cues[0]), (20., 40.));
        // Zoomed right out, a cue worth a fraction of a pixel is still a mark.
        let far = Scale {
            pps: 0.01,
            start: 0.,
        };
        assert_eq!(cue_box(far, &track.cues[0]).1, SUB_CUE_MIN_W);
        // ...and scrolled past, its left edge is negative, exactly as a clip
        // box's is: the bed clips it.
        let scrolled = Scale {
            pps: PPS_DEFAULT,
            start: 1.,
        };
        assert_eq!(cue_box(scrolled, &track.cues[0]).0, -20.);

        // Inside the media: the tracks of an mkv, taken by the same call every
        // open and import door makes. Two of them in the fixture, named by what
        // the file says rather than by number.
        let notice =
            subtitle_notice(&mut session, &asset("test_subs.mkv")).expect("the mkv carries two");
        assert!(notice.contains('2'), "{notice}");
        assert_eq!(session.subtitles().len(), 3);
        assert_eq!(session.subtitles()[1].label, "eng");
        assert_eq!(session.subtitles()[2].label, "fra — Signs");
        assert_eq!(subtitle_detail(&session.subtitles()[1]), "3 cues");
        // The same file twice adds nothing and says nothing.
        assert_eq!(subtitle_notice(&mut session, &asset("test_subs.mkv")), None);
        // An mp4 is walked too (it can carry `tx3g`); this one holds none, so
        // the notice grows no tail.
        assert_eq!(subtitle_notice(&mut session, &asset("test_av.mp4")), None);

        // A track that could not be read says why, where its cue count would
        // be: what the greyed library row prints, and the whole difference
        // between "this film has no subtitles" and "these four are pictures".
        let refused = engine::subtitle::SubtitleTrack {
            path: PathBuf::from("/a/remux.mkv"),
            track: Some(1),
            // Neither field, like `SubtitleTrack::refused` leaves them: what is
            // refused is never written, and the row keeps the label it was
            // refused under.
            language: String::new(),
            name: String::new(),
            label: "eng".into(),
            cues: Vec::new(),
            bitmap: false,
            refused: Some("S_HDMV/PGS subtitles are pictures, not text".into()),
        };
        assert!(subtitle_detail(&refused).contains("pictures"));
        assert!(cues_at(&refused.cues, 1.).is_empty());

        // ...and what the plate and the strip draw after a *cut*: the timeline's
        // own clock, asked of the engine through the very map an export writes
        // the file with (`PlaybackSession::timeline_cues`), so the preview and
        // the file cannot drift apart. The numbers are the export's own
        // (`export::a_subtitle_file_beside_the_media_keeps_the_timelines_own_
        // clock`): 0.5s..2.5s rippled out of a five-second timeline leaves
        // three seconds, which clips the second cue and takes the third away
        // altogether -- while the track itself still holds all three, which is
        // exactly why the drawing may not read them straight.
        let lanes = session.lanes();
        session
            .cut_regions(&[(15, 60)], &lanes)
            .expect("cut 0.5s..2.5s out");
        assert_eq!(session.timeline_duration(), 3.0);
        assert_eq!(session.subtitles()[0].cues.len(), 3, "the track is untouched");
        let mapped = session.timeline_cues(0);
        assert_eq!(
            mapped
                .iter()
                .map(|c| (c.start_us, c.end_us))
                .collect::<Vec<_>>(),
            vec![(500_000, 1_500_000), (2_000_000, 3_000_000)]
        );
        // The overlay's own two lines, at the same moments as above.
        let drawn = |t: f64| -> Vec<String> {
            cues_at(&mapped, t)
                .into_iter()
                .map(|c| c.text.clone())
                .collect()
        };
        assert_eq!(drawn(0.7), ["first line"]);
        assert_eq!(drawn(2.5), ["second line\nwith a break"]);
        assert!(
            drawn(4.2).is_empty(),
            "a cue past the cut end is drawn where the file writes none"
        );
        // ...and the strip's: the second cue is one second wide now, not 1.25.
        assert_eq!(cue_box(scale, &mapped[1]), (80., 40.));
    }

    /// The panel's button row against the 640x360 floor the whole editor is
    /// measured at. It does not fit and cannot be made to -- so what it may
    /// never do is *hide* the tail: the row scrolls, and the door to everything
    /// scrolled off it is pinned outside the scrolling box.
    #[test]
    fn toolbar_fits_the_smallest_window() {
        let source = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        let panel = &source[source.find("    fn panel(").expect("the panel")
            ..source.find("    fn import_bar(").expect("the panel's end")];
        // Every button in the row is a hit target, so none of them is narrower
        // than `HIT_MIN`, and they sit in 8 px gaps inside the panel's own
        // 12 px padding. The level slider is the one fixed width among them.
        let buttons = panel.matches("control(").count();
        assert!(buttons >= 15, "the row's buttons moved; this scan is blind");
        let row_w = buttons as f32 * (HIT_MIN + 8.) + VOLUME_W + 24.;
        assert!(
            row_w > 640.,
            "{row_w} px of buttons now fits the floor -- the pinned door may go"
        );
        // So Export, Save and Actions are off the right edge at that width, and
        // "it scrolls" is not "it can be found": this is the button that never
        // scrolls, and the card it opens carries every action there is
        // (`every_action_is_on_the_actions_card`).
        assert!(
            panel.contains("\"controls-more\""),
            "nothing pinned beside the scrolling row: the tail is unreachable at 640 px"
        );
        // ...and it is outside the scrolling box, which is the whole point of
        // it: one `overflow_x_scroll`, and the door is written after it closes.
        assert_eq!(panel.matches("overflow_x_scroll").count(), 1);
        assert!(
            panel.find("\"controls-more\"") > panel.find("overflow_x_scroll"),
            "the pinned door is inside the row it is meant to outlive"
        );
    }

    #[test]
    fn nothing_clickable_is_smaller_than_the_wcag_minimum() {
        // Every hit target in the panel, including the scrub strip -- whose bar
        // is 6 px to look at and whose click area must not be.
        assert!(CONTROL_H >= HIT_MIN);
        assert!(RULER_HIT_H >= HIT_MIN);
        assert!(LANE_H >= HIT_MIN);
        // A clip box is a hit target too, and its two trim strips occlude it:
        // on a box narrower than the pair there is no body left to press, so
        // the clip cannot be selected, dragged or menued at all -- which is
        // every clip a jumpcut leaves at a normal zoom. Below three handles
        // there are no strips.
        assert!(!trims(0.));
        assert!(!trims(EDGE_W));
        assert!(!trims(2. * EDGE_W), "a box that is all handle and no clip");
        assert!(!trims(3. * EDGE_W - 0.1));
        // And where they are drawn, what is left between them is a hit target
        // in its own right -- a whole handle's width of clip.
        for width in [3. * EDGE_W, 24., 100., 4000.] {
            assert!(trims(width));
            assert!(
                width - 2. * EDGE_W >= EDGE_W,
                "{width} px of box leaves no middle"
            );
        }
    }

    /// WCAG 2.1 relative luminance of a packed `0xRRGGBB`.
    fn luminance(colour: u32) -> f64 {
        let channel = |shift: u32| {
            let s = f64::from((colour >> shift) & 0xff) / 255.;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    /// WCAG 2.1 contrast ratio, 1..=21.
    fn contrast(a: u32, b: u32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn a_lane_row_is_a_fixed_header_and_a_bed_that_can_be_hit() {
        // The header column is what the ruler is offset by as well, so both
        // numbers are shared rather than repeated per row (A-MUST1/A-MUST2).
        assert!(HEADER_W > 0. && HEADER_GAP >= 0.);
        // Two lanes, a ruler, a button row and the timecode line, inside the
        // panel the window is sized for.
        assert!(
            CONTROL_H + RULER_HIT_H + 2. * LANE_H + 17. + 4. * 8. + 16. <= PANEL_H,
            "the second lane does not fit the panel"
        );
        // Headers and clip boxes are as tall as the lane, and the lane is a
        // click target (WCAG 2.5.8).
        assert!(LANE_H >= HIT_MIN);
        // A label row that ate the whole lane would leave no waveform.
        assert!(LABEL_H < LANE_H / 2.);
        // An added track adds its own row to the panel, and the two a project
        // starts with leave it exactly the height it has always been.
        assert_eq!(panel_h(2), PANEL_H);
        assert_eq!(panel_h(1), PANEL_H);
        assert_eq!(panel_h(3), PANEL_H + LANE_H + 8.);
        assert_eq!(
            panel_h(LANES_MAX),
            PANEL_H + lanes_h(LANES_MAX) - lanes_h(2)
        );
        // Past the cap the column scrolls instead: the panel stops growing, so
        // no number of tracks can push the picture off the window.
        assert_eq!(panel_h(LANES_MAX + 1), panel_h(LANES_MAX));
        assert_eq!(panel_h(50), panel_h(LANES_MAX));
        assert_eq!(lanes_h(0), 0.);
        assert_eq!(lanes_h(1), LANE_H);
        assert_eq!(lanes_h(2), 2. * LANE_H + 8.);
    }

    #[test]
    fn a_click_marks_the_whole_group_and_nothing_else() {
        let (v, a) = ((Lane::V1, 0), (Lane::A1, 0));
        // Clicking the video half of group 1 marks the audio half with it.
        assert!(marked(v, Some(1), Some(v), Some(1)));
        assert!(marked(a, Some(1), Some(v), Some(1)));
        // Another group's clips stay unmarked, in either lane.
        assert!(!marked((Lane::V1, 1), Some(2), Some(v), Some(1)));
        assert!(!marked((Lane::A1, 1), Some(2), Some(v), Some(1)));
        // A half a lift left behind has no group: it marks itself only, which
        // is what makes it separately deletable. Two ungrouped clips must not
        // mark each other by both being ungrouped.
        assert!(marked(a, None, Some(a), None));
        assert!(!marked(v, None, Some(a), None));
        // Nothing selected marks nothing.
        assert!(!marked(v, Some(1), None, None));
    }

    #[test]
    fn a_name_is_dropped_rather_than_smeared_across_a_thin_clip() {
        assert!(show_label(LABEL_MIN_W));
        assert!(show_label(400.));
        assert!(!show_label(LABEL_MIN_W - 0.1));
        // The label test is the box's own width in pixels now, which is what
        // the scale hands it -- no bed width, and so nothing to be zero.
        let scale = Scale::default();
        assert!(show_label(scale.width_px(LABEL_MIN_W as f64 / PPS_DEFAULT)));
        assert!(!show_label(scale.width_px(0.)));
    }

    #[test]
    fn an_envelope_stays_inside_the_box_it_is_drawn_in() {
        // A ramp: silence at the start, full scale at the end.
        let peaks: Vec<(f32, f32)> = (0..40)
            .map(|i| (-(i as f32) / 39., i as f32 / 39.))
            .collect();
        let (w, h) = (100., 30.);
        let cols = envelope(&peaks, 0., 1., w, h);
        assert_eq!(cols.len(), (w / WAVE_COL) as usize + 1);
        for &(x, top, bottom) in &cols {
            assert!((0. ..=w).contains(&x), "x {x} outside 0..{w}");
            assert!((0. ..=h).contains(&top), "top {top} outside 0..{h}");
            assert!(
                (0. ..=h).contains(&bottom),
                "bottom {bottom} outside 0..{h}"
            );
            // Never inverted, and never a polygon with no area: silence has to
            // read as a line rather than as nothing at all.
            assert!(
                bottom - top >= 1.,
                "column {top}..{bottom} is thinner than a pixel"
            );
        }
        // The ramp is drawn as a ramp: the last column is taller than the first.
        let height = |&(_, top, bottom): &(f32, f32, f32)| bottom - top;
        assert!(height(cols.last().unwrap()) > height(cols.first().unwrap()) + 5.);
        // Degenerate inputs draw nothing rather than panicking.
        assert!(envelope(&[], 0., 1., w, h).is_empty());
        assert!(envelope(&peaks, 0., 1., 0., h).is_empty());
        // A clip whose range runs past the peaks clamps to the last bucket.
        assert!(!envelope(&peaks, 0., 99., w, h).is_empty());
    }

    /// A box laid out wider than any screen -- a long clip at a deep zoom -- is
    /// still one screen's worth of columns: the path a repaint has to build is
    /// bounded by what can be seen, not by what the layout says the box is.
    /// Unbounded, a 5 s clip zoomed to the frame is a path of millions of points
    /// per frame, and the repaint that stalls on it is the waveform that
    /// "disappeared".
    #[test]
    fn an_envelope_never_costs_more_points_than_a_screen_can_show() {
        let peaks: Vec<(f32, f32)> = (0..200).map(|i| (-(i as f32) / 199., 1.)).collect();
        // The width a 5 s clip is laid out at when the bed shows 8 frames of it.
        let huge = 5. * 30. / 8. * 1200.;
        let cols = envelope(&peaks, 0., 5., huge, 30.);
        assert!(
            cols.len() <= WAVE_COLS_MAX + 1,
            "{} columns for a {huge} px box",
            cols.len()
        );
        // ...and the slice actually painted is the part of the box on the bed,
        // which is where that width stops mattering: a column per two visible
        // pixels, at every zoom.
        let (x, w) = visible_slice(-huge / 2., huge, 1200.);
        assert_eq!((x, w), (huge / 2., 1200.));
        assert_eq!(envelope(&peaks, 0., 5., w, 30.).len(), 601);
        // A box entirely off the bed has no slice, and one that has never been
        // measured is drawn whole -- what was drawn before there was a bed.
        assert_eq!(visible_slice(2000., 500., 1200.), (0., 0.));
        assert_eq!(visible_slice(-3000., 500., 1200.), (500., 0.));
        assert_eq!(visible_slice(-40., 500., 0.), (0., 500.));
        // Half on, at either edge.
        assert_eq!(visible_slice(-100., 500., 1200.), (100., 400.));
        assert_eq!(visible_slice(1000., 500., 1200.), (0., 200.));
    }

    /// The box a trim draws is the box its release commits, at every speed. The
    /// preview used to hand the *timeline* frame count to a source-frame field:
    /// at 2x a tail moved twice as fast as the pointer and snapped back on
    /// release, and a head drag moved the clip's other edge.
    #[test]
    fn a_trim_preview_lands_where_the_release_commits() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        for permille in SPEED_PRESETS {
            // Live, so the loop owes it no undo step: the speeds are the axis
            // this walks, and the trims below are what is undone.
            session
                .set_speed_live(Lane::V1, 0, Speed::from_permille(permille))
                .expect("a clip alone on its lane may be speeded");
            for edge in [Edge::Start, Edge::End] {
                let clip = session.lane_clips(Lane::V1)[0];
                let (lo, hi) = session
                    .trim_room(Lane::V1, 0, edge)
                    .expect("clip 0 is there");
                // Both walls and the middle of the room: the whole range a
                // pointer can be clamped to.
                for to in [lo, (lo + hi) / 2, hi] {
                    let preview = trimmed_clip(clip, edge, to, false);
                    // The drag is one edit and one undo step, so the next `to`
                    // is measured from the same clip this one was.
                    if session.trim_clip(Lane::V1, 0, edge, to) {
                        assert_eq!(
                            preview,
                            session.lane_clips(Lane::V1)[0],
                            "{edge:?} to {to} at {permille} per mille"
                        );
                        assert!(session.undo(), "the trim is one undo step");
                    } else {
                        // An edge already where it was asked to go is not an
                        // edit, and the preview draws the clip unchanged.
                        assert_eq!(preview, clip, "{edge:?} to {to} at {permille} per mille");
                    }
                    assert_eq!(session.lane_clips(Lane::V1)[0], clip, "back where it was");
                }
            }
        }
    }

    /// A still trims the same way, and the preview knows it: its head grows
    /// forward from source frame 0 -- every frame of it is the same picture --
    /// so the box stretches instead of sliding left.
    #[test]
    fn a_stills_trim_preview_grows_forward_like_the_commit() {
        let mut session = PlaybackSession::open(asset("test_still.png")).expect("a picture opens");
        for edge in [Edge::Start, Edge::End] {
            let clip = session.lane_clips(Lane::V1)[0];
            let (lo, hi) = session
                .trim_room(Lane::V1, 0, edge)
                .expect("clip 0 is there");
            for to in [lo, (lo + hi) / 2, hi] {
                let preview = trimmed_clip(clip, edge, to, true);
                match session.trim_clip(Lane::V1, 0, edge, to) {
                    true => {
                        assert_eq!(
                            preview,
                            session.lane_clips(Lane::V1)[0],
                            "a still {edge:?} to {to}"
                        );
                        assert!(session.undo(), "the trim is one undo step");
                    }
                    false => assert_eq!(preview, clip, "a still {edge:?} to {to}"),
                }
                assert_eq!(session.lane_clips(Lane::V1)[0], clip, "back where it was");
            }
        }
    }

    /// gpui freezes a drag's payload for the whole gesture, and nothing stops a
    /// stroke from editing the lane under it: the drop has to find the clip that
    /// was picked up, not whatever slid into its index.
    #[test]
    fn a_drop_moves_the_clip_that_was_picked_up_not_its_old_index() {
        let at = |start: u32| Clip {
            start,
            in_frame: 0,
            out_frame: 30,
            source: 0,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::Fit,
            speed: Speed::NORMAL,
        };
        let lane = [at(0), at(30), at(60)];
        let dragged = lane[2];
        assert_eq!(live_idx(&lane, 2, dragged), Some(2), "nothing moved");
        // A delete in front of it: the clip is now index 1, and the index the
        // drag froze names a clip nobody grabbed.
        let after = [at(0), at(60)];
        assert_eq!(live_idx(&after, 2, dragged), Some(1));
        assert_eq!(live_idx(&after, 1, dragged), Some(1));
        // Deleted mid-drag: there is nothing to move, and moving its neighbour
        // instead is exactly the bug this exists for.
        assert_eq!(live_idx(&[at(0)], 2, dragged), None);
        assert_eq!(live_idx(&[], 0, dragged), None);
    }

    #[test]
    fn a_quiet_source_still_draws_as_a_shape() {
        // An eighth of full scale, which is about where the fixtures sit.
        let quiet: Vec<(f32, f32)> = vec![(-0.125, 0.125), (-0.0625, 0.0625)];
        let loud = normalise(quiet.clone());
        assert_eq!(loud[0], (-1., 1.));
        assert_eq!(loud[1], (-0.5, 0.5));
        // Digital silence has no loudest sample to scale to; it must not divide
        // by zero and must stay flat.
        assert_eq!(normalise(vec![(0., 0.)]), vec![(0., 0.)]);
        assert!(normalise(Vec::new()).is_empty());
    }

    /// The whole waveform path, from the file on disk to the columns that get
    /// painted: what no screenshot can assert about the shape.
    #[test]
    fn the_fixtures_waveform_reaches_the_lane_as_a_shape() {
        let asset = |name: &str| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets")
                .join(name)
        };
        let peaks = normalise(
            engine::waveform::peaks(asset("test_av.mp4"), 0, WAVE_BPS)
                .expect("open the fixture")
                .expect("test_av.mp4 has audio"),
        );
        // 5 s of source at the rate the lane asks for.
        assert!(peaks.len().abs_diff(5 * WAVE_BPS as usize) <= WAVE_BPS as usize);
        let cols = envelope(&peaks, 0., 5., 600., 30.);
        let height = |&(_, top, bottom): &(f32, f32, f32)| bottom - top;
        let tallest = cols.iter().map(height).fold(0., f32::max);
        let flattest = cols.iter().map(height).fold(f32::MAX, f32::min);
        // The fixture's 1 Hz pulse: a full-scale peak and a near-silent dip in
        // every second, so the drawn envelope is a shape and not a bar.
        assert!(tallest > 25., "loudest column only {tallest} px of 30");
        assert!(
            flattest < 8.,
            "quietest column {flattest} px -- no dips drawn"
        );
        // A video-only source draws no waveform at all rather than a flat fake.
        assert!(
            engine::waveform::peaks(asset("test_baseline.mp4"), 0, WAVE_BPS)
                .expect("open the fixture")
                .is_none()
        );
    }

    #[test]
    fn every_mark_on_a_clip_is_legible_on_it() {
        for (i, tint) in SOURCE_TINTS.iter().enumerate() {
            // WCAG 1.4.3: the clip's name is body text on its tint.
            assert!(
                contrast(INK, *tint) >= 4.5,
                "source {i}: label contrast {:.2}",
                contrast(INK, *tint)
            );
            // WCAG 1.4.11: the waveform is a non-text graphic on it.
            assert!(
                contrast(INK_DIM, *tint) >= 3.,
                "source {i}: waveform contrast {:.2}",
                contrast(INK_DIM, *tint)
            );
        }
        // A selected clip's bed is the same two marks on a different colour.
        assert!(contrast(INK, SELECTED) >= 4.5);
        assert!(contrast(INK_DIM, SELECTED) >= 3.);
        // The bed a gap shows through has to read as a hole in the lane, and
        // the accent playhead has to be findable on it.
        assert!(contrast(LETTERBOX, SURFACE) >= 1.5);
        assert!(contrast(ACCENT, LETTERBOX) >= 3.);
        // The sanity check on the ratio itself: black on white is 21:1.
        assert!((contrast(0xffffff, 0x000000) - 21.).abs() < 0.01);
    }

    #[test]
    fn source_tints_differ_per_source_and_cycle() {
        // The bug: the first entry *was* `SURFACE`, so the first file imported
        // -- the one every session has -- wore the panel's own background and
        // had no visible swatch at all.
        assert_ne!(source_tint(0), SURFACE);
        // Neighbouring sources must not share one, or an import is invisible.
        assert_ne!(source_tint(0), source_tint(1));
        assert_ne!(source_tint(1), source_tint(2));
        assert_ne!(source_tint(2), source_tint(3));
        // Past the palette it wraps -- never an index panic.
        assert_eq!(source_tint(4), source_tint(0));
        assert_eq!(source_tint(9), source_tint(1));
        assert_eq!(source_tint(usize::MAX), SOURCE_TINTS[usize::MAX % 4]);
    }

    /// Not "they are different numbers" -- different *enough to see*, against
    /// each other and against the surface a swatch is drawn on. The palette is
    /// deliberately dark and low-saturation, so the margin is thin and a new
    /// tint picked by eye can land inside it without anyone noticing.
    #[test]
    fn source_tints_are_all_discernible() {
        // Summed channel distance: `SURFACE` to the warm tint is 18, and that
        // step is the one already accepted as readable on a lane.
        let apart = |a: u32, b: u32| {
            (0..3)
                .map(|i| {
                    let shift = i * 8;
                    ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff)
                })
                .sum::<u32>()
        };
        for (i, &tint) in SOURCE_TINTS.iter().enumerate() {
            assert!(
                apart(tint, SURFACE) >= 16,
                "tint {i} is {} from the panel it sits on",
                apart(tint, SURFACE)
            );
            for (j, &other) in SOURCE_TINTS.iter().enumerate().skip(i + 1) {
                assert!(
                    apart(tint, other) >= 16,
                    "tints {i} and {j} are only {} apart",
                    apart(tint, other)
                );
            }
        }
        // The two a person sees side by side first must be further apart than
        // the floor: source 0 and source 1 are the first import and the second.
        assert!(apart(SOURCE_TINTS[0], SOURCE_TINTS[1]) >= 32);
    }

    /// A `.srt` dropped on the window is nobody's stream: it has no source
    /// entry, and the lookup that used to fall back to index 0 painted it with
    /// the first file's colour -- a swatch saying it came out of a film it
    /// never touched.
    #[test]
    fn a_standalone_subtitle_wears_no_file_tint() {
        let sources = [
            Source {
                path: PathBuf::from("/films/a.mkv"),
                audio_stream: 0,
            },
            Source {
                path: PathBuf::from("/films/b.mp4"),
                audio_stream: 0,
            },
            // A second stream of the first file is a second source and the
            // same colour.
            Source {
                path: PathBuf::from("/films/a.mkv"),
                audio_stream: 1,
            },
        ];
        assert_eq!(
            file_tint(&sources, Path::new("/films/a.mkv")),
            Some(source_tint(0))
        );
        assert_eq!(
            file_tint(&sources, Path::new("/films/b.mp4")),
            Some(source_tint(1))
        );
        assert_eq!(file_tint(&sources, Path::new("/subs/a.eng.srt")), None);
        assert_eq!(file_tint(&[], Path::new("/films/a.mkv")), None);
        // The same file under two spellings is one file and one colour: a
        // source is stored symlink-resolved, everything else as it was typed.
        let here = std::fs::canonicalize(".").expect("the crate directory");
        let sources = [Source {
            path: here.join("Cargo.toml"),
            audio_stream: 0,
        }];
        assert_eq!(
            file_tint(&sources, Path::new("Cargo.toml")),
            Some(source_tint(0)),
            "a relative spelling of the source file wears the source's colour"
        );
    }

    /// The two shapes the engine really builds: a track *of* a file states a
    /// language and no title (`of_matroska`, `of_mp4`), and a standalone file
    /// states no language and is its own name (`external`).
    fn sub(path: &str, track: Option<u64>, label: &str) -> engine::subtitle::SubtitleTrack {
        let (language, name) = match track {
            Some(_) => (label.to_string(), String::new()),
            None => (String::new(), label.to_string()),
        };
        engine::subtitle::SubtitleTrack {
            path: PathBuf::from(path),
            track,
            language,
            name,
            label: label.to_string(),
            cues: Vec::new(),
            bitmap: false,
            refused: None,
        }
    }

    /// His film's two ASS tracks with the timeline trimmed to twenty seconds:
    /// one of them still has cues there and one has none, and the card said
    /// nothing whatever about the second while the Subtitles list went on
    /// showing it with its eighty-three cues.
    ///
    /// And the 25 GB remux's thirty-five: naming those wrapped the row to ten
    /// lines and pushed Destination under the fold, so past the value box's
    /// three lines the same line counts instead.
    #[test]
    fn the_card_names_the_subtitle_it_leaves_off_and_counts_them_when_it_cannot() {
        let film = "/films/An Episode 01.mkv";
        let two = [
            sub(film, Some(1), "[ASS]"),
            sub(film, Some(2), "[ASS] [FOR DUB]"),
        ];
        // What the engine answers about the one pick that reached it, and what
        // this side knows about the row that did not.
        let named = subtitle_plan("[ASS] → embedded".to_string(), &two, &[0]);
        assert_eq!(named, "[ASS] → embedded; [ASS] [FOR DUB] — no cues here");
        assert!(
            named.chars().count() <= SUB_PLAN_CHARS,
            "two tracks fit the value box: {named}"
        );
        // Thirty-five off one file: twenty-two carry cues here, nine are
        // pictures, one could not be read, three have nothing on this timeline.
        let many: Vec<_> = (0..35)
            .map(|i| {
                let mut track = sub("/films/A Remux.mkv", Some(i), "eng — Subtitles");
                track.bitmap = (22..31).contains(&i);
                track.refused = (i == 31).then(|| "VobSub is pictures".to_string());
                track
            })
            .collect();
        // The picks are every track with a cue on the timeline: the twenty-two
        // and the nine picture ones, which the engine drops itself.
        let picks: Vec<usize> = (0..31usize).collect();
        let counted = subtitle_plan("22 tracks → embedded (…)".to_string(), &many, &picks);
        assert_eq!(counted, "22 of 35 → embedded; 9 pictures; 1 unread; 3 no cues here");
        assert!(
            counted.chars().count() <= SUB_PLAN_CHARS,
            "thirty-five tracks still fit the value box: {counted}"
        );
        // Nothing on the timeline at all is still the engine's word for it.
        assert_eq!(subtitle_plan("none".to_string(), &[], &[]), "none");
    }

    /// The list is in the order tracks were added, which is not the order a
    /// person reads it in: importing a second film puts its tracks after the
    /// first film's, and importing a third `.srt` for the first film puts that
    /// one last of all. The rows still read as three sources.
    #[test]
    fn subtitle_rows_group_a_source_however_they_were_added() {
        let tracks = [
            sub("/films/a.mkv", Some(1), "eng"),
            sub("/films/b.mkv", Some(1), "eng"),
            sub("/films/a.mkv", Some(2), "fre"),
            sub("/subs/late.srt", None, "late.srt"),
            sub("/films/b.mkv", Some(3), "ger"),
        ];
        let groups = subtitle_rows(&tracks);
        assert_eq!(
            groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "late"],
            "one group per file, in the order the files first appear"
        );
        assert_eq!(
            groups
                .iter()
                .map(|g| g.rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            [vec!["eng", "fre"], vec!["eng", "ger"], vec!["late.srt"]],
            "a file's tracks are contiguous and in add order"
        );
        // Numbered within the file, the way `row_name` numbers audio streams:
        // two tracks that both say "eng" are told apart by nothing else.
        assert_eq!(
            groups
                .iter()
                .map(|g| g.rows.iter().map(|r| r.number).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            [vec![1, 2], vec![1, 2], vec![1]]
        );
        // The swatch key is the file, so a group and that file's media rows
        // wear one colour -- and the standalone one has none.
        let sources = [Source {
            path: PathBuf::from("/films/a.mkv"),
            audio_stream: 0,
        }];
        assert_eq!(
            file_tint(&sources, &groups[0].path),
            Some(source_tint(0)),
            "the group carries the path the tint is asked by"
        );
        assert_eq!(file_tint(&sources, &groups[2].path), None);
    }

    /// What the strip header, the section heading and the toggle's notice all
    /// say. Two films each carrying an "eng" track are one word apart until the
    /// film is in the name; a film carrying two is one word apart from itself.
    #[test]
    fn the_picked_subtitle_is_named_with_the_film_it_came_out_of() {
        let tracks = [
            sub("/films/a.mkv", Some(1), "eng"),
            sub("/films/b.mkv", Some(1), "eng"),
            sub("/films/a.mkv", Some(2), "eng"),
            sub("/films/b.mkv", Some(2), "und"),
            sub("/subs/late.srt", None, "late.srt"),
        ];
        let name = |track| sub_pick_name(&tracks, track).expect("a track that is there");
        // The two "eng"s of two films: one file gave several so its tracks are
        // numbered, the other gave one so it is not.
        assert_eq!(name(0), "eng 1 — a");
        assert_eq!(name(2), "eng 2 — a");
        assert_eq!(name(1), "eng 1 — b");
        assert_ne!(name(0), name(1), "two films' eng tracks read apart");
        for picked in [name(0), name(1), name(2)] {
            assert!(picked.contains(" — a") || picked.contains(" — b"));
        }
        // "und" is the tag for "nobody said", humanised once by
        // `subtitle_rows` and not again here.
        assert_eq!(name(3), "unknown language 2 — b");
        // A standalone `.srt` is its own file and its label already says so:
        // one track, so no number, and no stem after it saying it again.
        assert_eq!(name(4), "late.srt");
        // The silence `subtitle_track` gives at the same moment.
        assert_eq!(sub_pick_name(&tracks, 5), None);
        assert_eq!(sub_pick_name(&[], 0), None);
    }

    /// The one thing regrouping must not break: `sub_track` is a flat index
    /// into the add-order list, a click sets it and a save writes it into the
    /// `.edith`. Every row must still name the track it was made from -- rows
    /// for refused tracks included, because they take a number in that list
    /// whether or not anyone can pick them.
    #[test]
    fn a_subtitle_row_names_the_flat_track_it_was_made_from() {
        let mut tracks = vec![
            sub("/films/a.mkv", Some(1), "eng"),
            sub("/films/b.mkv", Some(1), "eng"),
            sub("/films/a.mkv", Some(2), "fre"),
        ];
        // Track 1 of b is pictures: still a row, still index 1 of the list.
        tracks[1].bitmap = true;
        tracks[1].refused = Some("PGS subtitles are pictures".to_string());
        let groups = subtitle_rows(&tracks);
        let flat: Vec<(usize, &str)> = groups
            .iter()
            .flat_map(|g| g.rows.iter().map(|r| (r.track, r.label.as_str())))
            .collect();
        assert_eq!(flat, [(0, "eng"), (2, "fre"), (1, "eng")]);
        for (track, label) in flat {
            assert_eq!(
                tracks[track].label, label,
                "row {track} picks the track it names"
            );
        }
        // (c) The refused one is here, saying why, and greyable by that alone.
        let refused = &groups[1].rows[0];
        assert_eq!(
            refused.refused.as_deref(),
            Some("PGS subtitles are pictures")
        );
        assert_eq!(refused.detail, "PGS subtitles are pictures");
        assert!(refused.bitmap);
        // ...and it was counted: the row after it in add order is still 2.
        assert_eq!(groups[0].rows[1].track, 2);
    }

    /// The × on a row shifts every track after it down one, and the pick is
    /// what an export writes into the file -- so a pick that stayed put would
    /// silently change which track the next export carries. Every relation
    /// between the pick and the row that went, on a list of three.
    #[test]
    fn removing_a_subtitle_row_carries_the_pick_with_it() {
        // A row *before* the pick: the same track stays picked, one index down.
        assert_eq!(sub_pick_after_removal(2, 0, 2), 1);
        assert_eq!(sub_pick_after_removal(2, 1, 2), 1);
        // A row *after* it: the pick has not moved and neither has its index.
        assert_eq!(sub_pick_after_removal(0, 2, 2), 0);
        assert_eq!(sub_pick_after_removal(1, 2, 2), 1);
        // The picked row itself: the one that slid into its place...
        assert_eq!(sub_pick_after_removal(1, 1, 2), 1);
        // ...and the last row when the picked one was the last, since there is
        // nothing after it to slide.
        assert_eq!(sub_pick_after_removal(2, 2, 2), 1);
        // The last row of all: an emptied list is legal for subtitles, and the
        // section is not drawn at all at that point.
        assert_eq!(sub_pick_after_removal(0, 0, 0), 0);
    }

    /// The same claim from the click's end, on the order imports actually
    /// arrive in: two films opened one after the other and an `.srt` dropped
    /// last interleave in the flat list, and the display reorders them. What a
    /// click sets is the row's own `track`, so the *n*th row on screen has to
    /// pick the track it shows and the echoes have to name that same file.
    #[test]
    fn a_click_on_a_regrouped_row_picks_the_track_that_row_shows() {
        let tracks = [
            sub("/films/a.mkv", Some(1), "eng"),
            sub("/films/b.mkv", Some(1), "eng"),
            sub("/films/a.mkv", Some(2), "fre"),
            sub("/subs/late.srt", None, "late.srt"),
            sub("/films/b.mkv", Some(3), "ger"),
        ];
        let rows: Vec<_> = subtitle_rows(&tracks)
            .into_iter()
            .flat_map(|group| {
                group
                    .rows
                    .into_iter()
                    .map(move |row| (group.path.clone(), row))
            })
            .collect();
        // Read top to bottom, the rows are no longer in add order...
        assert_eq!(
            rows.iter().map(|(_, row)| row.track).collect::<Vec<_>>(),
            [0, 2, 1, 4, 3]
        );
        for (path, row) in &rows {
            // ...and what the click writes into `sub_track` -- and a save into
            // the `.edith` -- still lands on the track the row is showing.
            let picked = &tracks[row.track];
            assert_eq!(&picked.path, path, "row {} picks another file", row.track);
            assert_eq!(lang_human(&picked.label), row.label);
            // And the heading, the strip and the notice name that same file
            // back, so a click cannot leave the echoes pointing elsewhere.
            let echo = sub_pick_name(&tracks, row.track).expect("the row's own track");
            let stem = path.file_stem().expect("a fixture path").to_string_lossy();
            assert!(
                echo.contains(&*stem),
                "picked {}, echoed {echo}",
                path.display()
            );
        }
    }

    /// "und" is what a muxer writes when nobody said what the language is. A
    /// row showing it verbatim names a language nobody speaks.
    #[test]
    fn an_untagged_language_says_it_is_unknown() {
        assert_eq!(lang_human("und"), "unknown language");
        assert_eq!(lang_human("eng"), "eng");
        assert_eq!(lang_human("fre — Commentary"), "fre — Commentary");
        // Reaching the subtitle rows too: a track whose only name was the tag.
        let groups = subtitle_rows(&[sub("/films/a.mkv", Some(1), "und")]);
        assert_eq!(groups[0].rows[0].label, "unknown language");
        // The pair, read as the pair: the row title comes off `language` and
        // `name` and never out of the flattened label, which is what let an
        // "und" beside a title through as a language nobody speaks. A refused
        // track states neither and keeps its label.
        let titled = |language: &str, name: &str, label: &str| engine::subtitle::SubtitleTrack {
            path: PathBuf::from("/films/a.mkv"),
            track: Some(1),
            language: language.into(),
            name: name.into(),
            label: label.into(),
            cues: Vec::new(),
            bitmap: false,
            refused: None,
        };
        for (language, name, label, title) in [
            ("fra", "Signs", "fra — Signs", "fra — Signs"),
            ("und", "Signs", "Signs", "Signs"),
            ("und", "", "und", "unknown language"),
            ("", "late.srt", "late.srt", "late.srt"),
            ("", "", "eng", "eng"),
        ] {
            let rows = subtitle_rows(&[titled(language, name, label)]);
            assert_eq!(rows[0].rows[0].label, title, "{language:?} {name:?}");
        }
    }

    /// The bug: an empty timeline is end-of-stream from its one black frame
    /// onward, so the pump had `done` set before anything was ever pressed --
    /// and the transport's restart branch read that as "played out, start from
    /// the top". It started a clock against a zero-length timeline, which was
    /// `done` again by the next repaint, so every further press restarted it
    /// too: the button read "Pause" and no press of it ever paused.
    ///
    /// What holds it now is one predicate, checked here against real sessions
    /// on both sides -- the emptied one refuses, a timeline with clips on it
    /// does not.
    #[test]
    fn an_empty_timeline_has_nothing_to_play() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        // Silent like the engine suite: this opens the real device.
        session.set_gain(0.0);
        // A timeline with clips on it plays, and always did: the guard must not
        // touch that side.
        assert!(!session.is_empty());
        assert!(!nothing_to_play(Some(&session)));

        // Every clip taken off, which is a state and not a failure.
        while session.delete_clip(Lane::V1, 0) {}
        while session.delete_clip(Lane::A1, 0) {}
        assert!(session.is_empty(), "the timeline is empty");
        assert_eq!(session.timeline_duration(), 0.0);

        // What the pump does every render, and what set `done` before the fix:
        // the black frame goes by and the session is at its end at once.
        for _ in 0..40 {
            while session.try_frame().is_some() {}
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            session.is_eos(),
            "an empty timeline is done before it starts"
        );

        // So the press is refused rather than sent down the restart branch --
        // and with no session at all it is the same refusal.
        assert!(nothing_to_play(Some(&session)));
        assert!(nothing_to_play(None));
        assert!(!session.is_playing(), "and nothing was started");
    }

    /// The slider writes the same numbers the keys do, and mute is not one of
    /// them: dragging while muted picks the level unmuting comes back to.
    #[test]
    fn the_slider_lands_on_the_grid_the_keys_move_on() {
        let mut volume = Volume::default();
        // Both ends exactly, and clamped past them.
        volume.set_along(0.);
        assert_eq!(volume.gain(), 0.0);
        assert_eq!(volume.label(), "Vol 0%");
        volume.set_along(1.5);
        assert_eq!(volume.gain(), 1.0);
        assert_eq!(volume.along(), 1.0);

        // Halfway is 50%, and a key press from there is 5% -- the same step
        // count as before, on a finer grid.
        volume.set_along(0.5);
        assert_eq!(volume.label(), "Vol 50%");
        volume.step(true);
        assert_eq!(volume.label(), "Vol 55%");
        volume.step(false);
        assert_eq!(volume.gain(), 0.5);

        // A number no step lands on comes back as the nearest one, so the label
        // and the fill are the same value the device was handed.
        volume.set_along(0.333);
        assert_eq!(volume.label(), "Vol 33%");
        assert_eq!(volume.along(), 0.33);

        // Muted, the drag moves the level and nothing comes out.
        volume.muted = true;
        volume.set_along(0.8);
        assert_eq!(volume.gain(), 0.0);
        assert_eq!(volume.label(), "Muted 80%");
        volume.muted = false;
        assert_eq!(volume.gain(), 0.8);
    }

    /// The slider lands where it paints: the arithmetic `Player::drag_volume`
    /// runs over the bar's own painted width, which is the one thing a test of
    /// it can share without re-deriving it.
    #[test]
    fn the_volume_slider_lands_where_it_paints() {
        let bar = Bounds {
            origin: point(px(420.), px(508.)),
            size: size(px(VOLUME_W), px(CONTROL_H)),
        };
        let at = |x: f32| {
            let mut volume = Volume::default();
            volume.set_along(frac_along(px(x), bar));
            volume
        };
        assert_eq!(at(420.).gain(), 0.0, "the left end is silence");
        assert_eq!(at(420. + VOLUME_W).gain(), 1.0, "the right end is full");
        assert_eq!(at(-4000.).gain(), 0.0, "off the left clamps");
        assert_eq!(at(9999.).gain(), 1.0, "off the right clamps");
        // Every pixel along it: a level the keys could also reach, painted back
        // where the hand pressed to within the half step the rounding costs.
        for step in 0..=(VOLUME_W as u32) {
            let along = step as f32 / VOLUME_W;
            let volume = at(420. + along * VOLUME_W);
            let painted = volume.along();
            let slack = 0.5 / f32::from(Volume::MAX_STEPS) + 1e-4;
            assert!(
                (painted - along).abs() <= slack,
                "pressed at {along}, paints at {painted}"
            );
        }
    }
}

fn main() {
    // A keymap file that cannot be read leaves the defaults in force, and takes
    // the notice slot ahead of an open or import refusal: it is about every key
    // the window has, and those refusals are on stderr either way.
    let (keymap, notice) = Keymap::load();
    if let Some(text) = &notice {
        eprintln!("{text}");
    }
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
                    selected: None,
                    context_menu: None,
                    picker: None,
                    library_menu: None,
                    selected_asset: None,
                    waves: HashMap::new(),
                    streams: HashMap::new(),
                    bitrates: HashMap::new(),
                    sizes: HashMap::new(),
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
                    notice: notice.clone().map(SharedString::from),
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
