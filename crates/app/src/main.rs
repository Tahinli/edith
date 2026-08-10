mod keymap;

use keymap::{ActionId, Keymap};

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::audio::StreamInfo;
use engine::export::{ExportSettings, Format};
use engine::project::{Lane, LaneKind, Source};
use engine::{Clip, ExportHandle, Frame, PlaybackSession};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, Div, FocusHandle, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point,
    RenderImage, SharedString, Size, Stateful, TitlebarOptions, Window, WindowBounds,
    WindowOptions, canvas, div, img, point, prelude::*, px, relative, rgb, rgba, size,
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
/// (ledger:187), and the first source keeps `SURFACE` exactly.
const SOURCE_TINTS: [u32; 4] = [SURFACE, 0x3b3329, 0x293b33, 0x33293b];
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
const WAVE_BPS: u32 = 40;
/// Pixels per envelope column. Coarser than a pixel: the eye reads the shape,
/// and a path with a point per pixel is a path per repaint.
const WAVE_COL: f32 = 2.;
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
const RULER_HIT_H: f32 = HIT_MIN;
/// Wide enough for `HH:MM:SS:FF / HH:MM:SS:FF`, and fixed so changing digits
/// cannot push the layout around.
const TIME_W: f32 = 200.;
/// The keybindings card: a row per action, a title and a status line, inside a
/// 360 px tall window. The rows are click targets, so `HIT_MIN` binds them too.
const KEYS_W: f32 = 320.;
const KEYS_ROW_H: f32 = HIT_MIN;
/// How much of the row list is on screen at once; past this it scrolls. What
/// keeps the card inside the smallest window no matter how many actions the
/// editor grows -- ten rows fit here, and the eleventh is a scroll away.
const KEYS_ROWS_H: f32 = 10. * KEYS_ROW_H;
/// The same for the export card, which carries a fixed-format line and a button
/// under its list and so has less room: destination, six formats and five
/// qualities are twelve rows, eight of them on screen.
const EXPORT_ROWS_H: f32 = 8. * KEYS_ROW_H;
/// The menu a right-click on a clip opens: wide enough for the longest label
/// beside the stroke that does the same thing, with the click targets `HIT_MIN`
/// binds like every other list here.
const MENU_W: f32 = 260.;
const MENU_ROW_H: f32 = HIT_MIN;
const MENU_PAD: f32 = 6.;

/// The one key name this file still spells out, and gpui's spelling of it: it
/// is the way out of a capture and out of the overlay, and both have to work
/// while the keymap itself is what is being changed -- so neither can go
/// through the keymap to find it.
const ESCAPE: &str = "escape";

/// What the header says with no timeline open, and what the window title reads
/// as a program name rather than as a file name.
const NO_FILE: &str = "no file open";

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
    /// 5% a press: twenty of them across the range.
    const MAX_STEPS: u8 = 20;

    /// What the device is set to: mute wins, and the level is what it returns
    /// to. `0.0..=1.0`, which is the range the plugin's ABI accepts.
    fn gain(self) -> f32 {
        if self.muted {
            0.
        } else {
            f32::from(self.steps) / f32::from(Self::MAX_STEPS)
        }
    }

    /// One press up or down, clamped at both ends -- saturating, so the count
    /// cannot wrap past silence into full volume.
    fn step(&mut self, up: bool) {
        self.steps = if up {
            self.steps.saturating_add(1).min(Self::MAX_STEPS)
        } else {
            self.steps.saturating_sub(1)
        };
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
    Peaks(Arc<Vec<(f32, f32)>>),
}

/// A library row being dragged: the file and which of its audio streams that
/// row is, which is the whole of what a row names. Where it lands does not
/// change what is inserted.
struct AssetDrag(PathBuf, usize);

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

/// What the menu offers, in the order it lists them. Every one of these is an
/// action a stroke already reaches -- the menu is a second way *to* the actions
/// and never a second version of them -- so both the label and the hint come
/// out of the keymap registry and the two can never disagree.
const MENU_ITEMS: [ActionId; 5] = [
    ActionId::Cut,
    ActionId::Delete,
    ActionId::Lift,
    ActionId::Regroup,
    ActionId::ToggleMute,
];

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
    /// A frame that arrived before its time; shown on the tick it comes due.
    held: Option<Frame>,
    /// The decoder's channel closed. Frames may still be waiting in `held`.
    eos: bool,
    /// Last frame shown; nothing left to animate.
    done: bool,
    /// A seek is waiting for its frame. Keeps the repaint loop alive while
    /// paused, which is the only way the new still ever reaches the screen.
    pending_seek: bool,
    /// The ruler's own box, recorded at prepaint: a mouse listener is handed
    /// the window position and nothing else.
    ruler: Rc<Cell<Bounds<Pixels>>>,
    /// Which clip the edit keys act on: the lane it is in and its index there.
    /// The *clicked* half, not the group -- a group is what gets marked on
    /// screen, but Lift has to know which half it was aimed at. Indices move
    /// under every edit, so this is cleared by all of them.
    selected: Option<(Lane, usize)>,
    /// The clip menu a right-click opened, if one is up. Holds an index like
    /// `selected` does, so it is closed by anything that can move indices --
    /// every stroke, and every item of its own.
    context_menu: Option<ContextMenu>,
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
    /// The copied clip. Frame ranges only, so it survives the clip it was taken
    /// from being deleted -- and it outlives the selection.
    clipboard: Option<Clip>,
    /// A drag that started on the ruler. Moves anywhere in the window scrub
    /// while it is set; the release commits the exact position.
    scrubbing: bool,
    last_scrub: Instant,
    last_target: u32,
    /// The running export. While it owns the UI the editor is read-only.
    export: Option<ExportHandle>,
    /// The export above was cancelled and is only winding down. The editor is
    /// already free -- the worker took its own copy of the edit list -- but the
    /// handle is held until the worker settles, because its last act is to
    /// delete the output file and a second export must not be what it deletes.
    cancelling: bool,
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
    /// The keybindings overlay is up. While it is, it owns the keyboard and the
    /// pointer: a stroke or a click meant for a row must not also cut the
    /// timeline.
    keys_open: bool,
    /// The export options card is up: what the export action opens now, so
    /// nothing is written until the card's own button says so. One card at a
    /// time -- opening either closes the other, since both are the whole window
    /// and two stacked scrims say nothing about which one is listening.
    export_open: bool,
    /// Which quality row the card has picked, and the megabits typed against
    /// the custom one. Kept across closes, so a second export offers what the
    /// first one chose.
    quality: Quality,
    custom_mbps: u32,
    /// Which file the card will write. Kept across closes like the quality, and
    /// what [`Player::export_path`](Player) is named after.
    format: Format,
    /// The action whose row is waiting for a stroke. The next key that is
    /// neither escape nor a lone modifier becomes the whole of what reaches it.
    rebinding: Option<ActionId>,
    /// What the last file action had to say. Holds its own bar above the panel
    /// until it is answered -- any key retires it, so does a click on it -- so a
    /// failure is read in full instead of blinking past.
    notice: Option<SharedString>,
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
                    None => {
                        self.eos = session.is_eos();
                        break;
                    }
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
            self.pending_seek = false;
            self.started.get_or_insert_with(|| {
                eprintln!("first frame displayed (index {})", frame.index);
                Instant::now()
            });
            let buf = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
                .expect("frame buffer sized width*height*4");
            let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
            if let Some(old) = self.image.replace(next) {
                // Every RenderImage gets a fresh id and its own atlas tile:
                // without this the sprite atlas grows for the whole video.
                let _ = window.drop_image(old);
            }
        }

        if self.eos && self.held.is_none() && !self.done {
            self.done = true;
            // A seek whose worker never produced a frame (vanished file) would
            // otherwise repaint at vsync forever.
            self.pending_seek = false;
            let elapsed = self.started.map_or(0.0, |t| t.elapsed().as_secs_f64());
            eprintln!(
                "eof after {elapsed:.3}s wall: {} frames displayed, {} dropped, clock {:.3}s",
                self.displayed,
                self.dropped,
                session.now()
            );
        }
    }

    /// Every end-of-stream flag is app state and the engine knows nothing about
    /// it, so clearing them after a reseek is what stops the picture from
    /// staying frozen on the old last frame. Edits reseek inside the engine and
    /// still owe this.
    fn reset_after_reseek(&mut self) {
        self.held = None;
        self.eos = false;
        self.done = false;
        self.pending_seek = true;
    }

    /// What an action does, wherever it was asked for -- a stroke, or the clip
    /// menu item that names the same action. One table, so the two can never
    /// come to mean different things.
    fn act(&mut self, action: ActionId, cx: &mut Context<Self>) {
        match action {
            ActionId::Play => self.toggle_or_restart(cx),
            ActionId::Export => self.open_export(cx),
            ActionId::Save => self.save_project(cx),
            ActionId::Copy => self.copy_selected(),
            ActionId::Paste => self.paste(cx),
            ActionId::Cut => self.cut(cx),
            ActionId::Regroup => self.regroup(cx),
            ActionId::Delete => self.delete_selected(cx),
            ActionId::Lift => self.lift_selected(cx),
            ActionId::Undo => self.undo(cx),
            ActionId::ToggleMute => self.set_volume(|volume| volume.muted = !volume.muted, cx),
            ActionId::VolumeUp => self.set_volume(|volume| volume.step(true), cx),
            ActionId::VolumeDown => self.set_volume(|volume| volume.step(false), cx),
            // Nothing to cancel while nothing is exporting; the export guard in
            // the key handler is what answers this one while there is.
            ActionId::CancelExport => {}
        }
    }

    /// Whether a card owns the window. While one does the timeline under it is
    /// out of reach, so a right-click there opens no menu -- the same rule the
    /// key handler and the drop target already follow.
    fn modal(&self) -> bool {
        self.keys_open || self.export_open || self.exporting().is_some()
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
        // Exhaustive over the kind, and explicit about the ord: both engine
        // calls below take a *first*-lane index (`delete_clip` deletes the V1
        // clip at `idx`, `video_half` reads A1), so a selection on a second
        // lane of either kind would delete a different clip than the one
        // clicked. It is refused by name until the add-track slice teaches
        // those two a lane -- a wildcard here would drop it silently.
        let deleted = match (&mut self.session, selected) {
            (Some(session), Some((lane, idx))) if lane.ord == 0 => match lane.kind {
                LaneKind::Video => session.delete_clip(idx),
                LaneKind::Audio => match video_half(session, idx) {
                    Some(video) => session.delete_clip(video),
                    None => session.lift_clip(lane, idx),
                },
            },
            _ => false,
        };
        if let Some((lane, _)) = selected.filter(|_| !deleted) {
            if lane.ord > 0 {
                self.notice = Some(
                    format!(
                        "NOTHING DELETED — delete does not reach {} yet",
                        lane.label()
                    )
                    .into(),
                );
            }
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
                    self.notice = Some("NOTHING LIFTED — the timeline cannot be emptied".into());
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
        for key in unseen_sources(session.sources(), &self.waves) {
            self.waves.insert(key.clone(), Wave::Loading);
            let decoded = cx.background_executor().spawn({
                let (path, stream) = key.clone();
                async move {
                    engine::waveform::peaks(&path, stream, WAVE_BPS)
                        .ok()
                        .flatten()
                        .map(|peaks| Arc::new(normalise(peaks)))
                }
            });
            cx.spawn(async move |this, cx| {
                let decoded = decoded.await;
                this.update(cx, |this, cx| {
                    this.waves.insert(
                        key,
                        match decoded {
                            Some(peaks) => Wave::Peaks(peaks),
                            // Either no audio track or a file we could not read
                            // it from; both mean the lane has nothing to draw,
                            // and neither is worth asking about again.
                            None => Wave::Silent,
                        },
                    );
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
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

    /// The one way a library row reaches the timeline: the Add button and a row
    /// dragged onto a lane both come here, so there is a single answer to what
    /// "add this source" does. The whole source goes in at the playhead as one
    /// grouped take -- the same insert a paste makes, so everything after it
    /// moves along rather than being painted over. Reseeks like every other
    /// edit, and drops the timeline's selection with it: the insert has just
    /// moved the indices it pointed at.
    fn insert_source(
        &mut self,
        path: &Path,
        stream: usize,
        onto: Option<Lane>,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        // A file with no picture belongs on the audio lane and nowhere else:
        // dropped on the video lane it is refused by name, and asked for by the
        // Add button (which names no lane) it goes to the audio one -- which is
        // the engine's choice, in `place_stream_at`, not one made twice here.
        if engine::is_audio(path) && onto == Some(Lane::V1) {
            let name = file_name(path);
            self.notice = Some(
                format!("NOT ON THE VIDEO LANE — {name} has no picture; drop it on A1").into(),
            );
            cx.notify();
            return;
        }
        let frames = self.session.as_ref().map_or(0, |session| {
            source_frames(lane_clips(session), session.sources(), path)
        });
        // Zero means nothing on the timeline plays from that file any more
        // -- an undone import leaves the source entry behind (project.rs:264)
        // -- so there is no length to insert and no file to ask for one.
        let placed = match (&mut self.session, frames) {
            (Some(session), 1..) => session.place_stream_at(session.now(), path, stream, frames),
            _ => Ok(false),
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
                self.notice = Some(
                    "NOTHING ADDED — no clip on the timeline plays from that file any more".into(),
                )
            }
        }
        cx.notify();
    }

    /// The rate and layout the whole timeline's audio is, taken from source 0's
    /// own stream: what a library row has to match to be placeable. `None`
    /// until that file has been probed, and then nothing is greyed for it.
    fn timeline_audio(&self) -> Option<(u32, u16)> {
        let first = self.session.as_ref()?.sources().first()?;
        let info = self
            .streams
            .get(&first.path)?
            .iter()
            .find(|s| s.index == first.audio_stream)?;
        Some((info.sample_rate, info.channels))
    }

    /// Appends a file to the end of the timeline. A drop is not a key press, so
    /// the export guard on the key handler does not cover it and this checks for
    /// itself. The engine reseeks, so like a delete it owes the flag reset; a
    /// refusal is shown as the engine worded it and changes nothing.
    fn import(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // An empty window has nothing to append to: there the first file *is*
        // the timeline and is opened as one, which is the same fork a launch
        // makes between its first argument and the rest.
        let text = match self.session.as_mut().map(|session| session.import(path)) {
            Some(Ok(())) => {
                self.reset_after_reseek();
                format!("IMPORTED {}", file_name(path))
            }
            Some(Err(e)) => format!("IMPORT FAILED: {e}"),
            None => self.open_media(path),
        };
        eprintln!("{text}");
        self.notice = Some(text.into());
        cx.notify();
    }

    /// Takes a file as the whole timeline, which is what an empty window is
    /// waiting for. Everything derived from the media -- the clock, the title,
    /// where an export and a save go -- is set here, exactly as a launch with a
    /// file argument sets it. Paused with its first frame showing, like every
    /// other way a timeline arrives.
    fn open_media(&mut self, path: &std::path::Path) -> String {
        match PlaybackSession::open(path) {
            Ok(session) => {
                self.fps = session.meta().frame_rate;
                // Read before the session moves: a file that plays silent says
                // so here or nowhere.
                let silent = audio_notice(&session);
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
                format!("OPENED {}{}", file_name(path), silent.unwrap_or_default())
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
        let picked = cx.background_executor().spawn(async { pick_file() });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                // The same fork the drop handler makes: a project replaces the
                // timeline, media is appended to it.
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

    /// Swaps the whole timeline for one restored from a `.edith`. Like an
    /// import this arrives by drop and so checks the export guard for itself.
    /// The new session is built before anything is replaced, so a refusal is
    /// shown as the engine worded it and leaves what is playing alone.
    fn load_project(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let text = match PlaybackSession::open_project(path) {
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
                // would name some other timeline's clip.
                self.context_menu = None;
                // A different set of sources: the row that was picked is not
                // the file that index names any more.
                self.selected_asset = None;
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
        let t = f64::from(frac_along(x, self.ruler.get())) * session.timeline_duration();
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

    fn toggle_or_restart(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if self.done {
            self.seek(0., cx);
            if let Some(session) = &mut self.session {
                session.play();
            }
        } else if let Some(session) = &mut self.session {
            session.toggle();
            // Past EOF nothing else asks for a repaint.
            cx.notify();
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
        // was waiting in.
        self.keys_open = false;
        self.rebinding = None;
        cx.notify();
    }

    /// A format row was clicked. The destination follows it at once -- a WAV
    /// written to a path ending in `.mp4` is a file every player will lie
    /// about -- keeping whatever stem the save dialog last left there.
    fn set_format(&mut self, format: Format) {
        self.format = format;
        self.export_path = retarget(&self.export_path, format);
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
        let settings = export_settings(self.quality, self.custom_mbps, self.format);
        let Some(session) = &mut self.session else {
            self.notice = Some("NOTHING TO EXPORT — open a file first".into());
            cx.notify();
            return;
        };
        session.pause();
        self.export = Some(session.export_to_with(&self.export_path, &settings));
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
        let Some(result) = self.export.as_ref().and_then(ExportHandle::result) else {
            return;
        };
        self.export = None;
        // A cancellation is reported as an error, and the one who asked for it
        // has had the editor back since the keystroke. Nothing to say.
        if std::mem::take(&mut self.cancelling) {
            return;
        }
        let text = match result {
            Ok(()) => format!("EXPORT DONE → {}", file_name(&self.export_path)),
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
        self.poll_export();
        // Every way a source can arrive -- argv, an import, a project load --
        // has been through a repaint by the time its clips are drawn, so this
        // is the one place that has to notice a new one.
        self.cache_media(cx);
        // What the compositor calls this window. Pushed only when it changes:
        // it is a protocol round trip and this runs at vsync.
        let title = window_title(&self.name);
        if title != self.titled {
            window.set_window_title(&title);
            self.titled = title;
        }
        // No shadow flag: the clock is the only truth about play state.
        let playing = self
            .session
            .as_ref()
            .is_some_and(PlaybackSession::is_playing);
        // A paused timeline has nothing to animate; the toggle handlers notify,
        // which is what starts the loop again. A paused seek keeps the loop
        // running by itself until `pump` has the frame it asked for. An export
        // pauses playback and still needs the loop: its progress only reaches
        // the screen on a repaint. A notice does not: it waits to be dismissed
        // rather than for a clock, so keeping the loop alive for it would spin
        // the GPU until someone answered it.
        if (playing && !self.done) || self.pending_seek || self.export.is_some() {
            window.request_animation_frame();
        }

        // Read per render, never cached: a delete shortens the timeline and the
        // timecode, the ruler and the clamp below all have to follow it.
        let duration = self
            .session
            .as_ref()
            .map_or(0., PlaybackSession::timeline_duration);
        // The clock keeps running after the last frame (wall time takes over at
        // audio EOF) while the picture is frozen, so the timeline the UI shows is
        // the clamped one, pinned to the out-point once playback is done.
        let position = if self.done {
            duration
        } else {
            self.session
                .as_ref()
                .map_or(0., PlaybackSession::now)
                .clamp(0., duration)
        };

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                // `is_held` filters auto-repeat, which would otherwise toggle
                // playback -- or cut the timeline -- many times a second.
                if event.is_held {
                    return;
                }
                // Any key retires the last message, whatever it was -- and owes
                // the repaint itself: a notice no longer keeps the render loop
                // alive, and the arms below that do notify are not all of them
                // (an unbound key, or the copy chord, changes nothing else).
                if this.notice.take().is_some() {
                    cx.notify();
                }
                let key = event.keystroke.key.as_str();
                // A row is waiting for a stroke, and while it is, that stroke is
                // data: it means the binding and nothing else, which is why this
                // answers before the export guard and before the keymap is
                // consulted at all.
                if let Some(action) = this.rebinding {
                    if key == ESCAPE {
                        this.rebinding = None;
                    } else if !is_bare_modifier(key) {
                        this.capture(action, key, event.keystroke.modifiers.control);
                    }
                    cx.notify();
                    return;
                }
                // On linux gpui reports the copy chord as key "c" with the
                // control modifier set (the control code is mapped back), which
                // is why the keymap is keyed on the pair and never on the key
                // alone.
                let action = this.keymap.lookup(key, event.keystroke.modifiers.control);
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
                // The overlay owns the keyboard while it is up: a stroke aimed
                // at a row must not also cut the timeline, and the way out of it
                // is the same key that gets out of a capture.
                if this.keys_open {
                    if key == ESCAPE {
                        this.keys_open = false;
                        cx.notify();
                    }
                    return;
                }
                // The export card owns it the same way, and for the same
                // reason. Escape closes it -- nothing has been written yet, so
                // there is nothing here to cancel -- and a digit is the custom
                // bitrate being typed: the card has no text field (nothing in
                // it takes focus, ledger:182) so this listener is its input,
                // exactly as it is a waiting row's.
                if this.export_open {
                    if key == ESCAPE {
                        this.export_open = false;
                    } else if key == "enter" {
                        // The card's own button, by keyboard: the one thing in
                        // it that writes a file must not be pointer-only either.
                        this.start_export(cx);
                    } else if let Some(format) = format_key(key) {
                        // The format rows by their initial, so the card can be
                        // driven without a mouse -- the same card-local input
                        // the typed bitrate is, and for the same reason: a
                        // choice reachable only by pointer is not reachable by
                        // everyone. Not a keymap binding: it means nothing
                        // outside this card, exactly like the digits.
                        this.set_format(format);
                    } else if let Ok(digit) = key.parse::<u32>() {
                        this.custom_mbps = push_digit(this.custom_mbps, digit);
                        this.quality = Quality::Custom;
                    } else if key == "backspace" {
                        this.custom_mbps /= 10;
                        this.quality = Quality::Custom;
                    }
                    cx.notify();
                    return;
                }
                // A clip menu names an index, and every edit below moves
                // indices -- so a stroke closes it before it acts. Escape means
                // that and nothing else, which is the `esc` the keys menu
                // already lists (keymap.rs `FIXED`).
                if this.context_menu.take().is_some() {
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
                if this.keys_open || this.export_open {
                    return;
                }
                for path in paths.paths() {
                    // A project replaces the timeline, media is appended to it.
                    if is_project(path) {
                        this.load_project(path, cx);
                    } else {
                        this.import(path, cx);
                    }
                }
            }))
            // Scrubbing is tracked on the root because the pointer leaves the
            // 6 px ruler on the first drag and its own listeners then stop
            // firing; the root's hitbox is the whole window.
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
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
                    .child(self.library(library_w(f32::from(window.viewport_size().width)), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
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
                            ),
                    ),
            )
            // Above the panel and only when there is one to show, so it costs
            // the picture nothing the rest of the time.
            .children(self.notice_bar(cx))
            .child(self.panel(position, duration, playing, cx))
            // Over the panel it was opened on, and under the cards: it is only
            // ever up while neither of them is (`modal`).
            .children(self.context_card(window.viewport_size(), cx))
            // Last, so they are over everything -- they take no room in the
            // column, and only one of the two is ever up.
            .children(self.keys_overlay(cx))
            .children(self.export_card(cx))
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
    fn library(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let exporting = self.exporting().is_some();
        let sources = self
            .session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources);
        // Every source matches the first in size and rate or it was refused at
        // the door (the import policy, ledger:436), so the session's own meta
        // describes every row and nothing has to be probed to say so.
        let meta = self.session.as_ref().map(PlaybackSession::meta);
        let clips: Vec<Clip> = self
            .session
            .as_ref()
            .map_or_else(Vec::new, |session| lane_clips(session).copied().collect());
        let rows: Vec<_> = library_rows(sources, &self.streams, self.timeline_audio(), |path| {
            source_frames(clips.iter(), sources, path)
        })
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let picked = self
                .selected_asset
                .as_ref()
                .is_some_and(|p| *p == (row.path.clone(), row.stream));
            let name: SharedString = row.name.clone().into();
            let tip: SharedString = match (&row.unusable, meta) {
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
                (None, Some(meta)) => format!(
                    "{} — {}x{} @ {:.2} fps · drag onto a lane, or Add at playhead",
                    row.path.display(),
                    meta.width,
                    meta.height,
                    meta.frame_rate
                ),
                (None, None) => row.path.display().to_string(),
            }
            .into();
            let ghost = name.clone();
            // What the second line says: the stream, then either its length or
            // the reason it cannot be used.
            let under = match &row.unusable {
                Some(why) => join_detail(&row.detail, why),
                None => join_detail(
                    &row.detail,
                    &timecode(f64::from(row.frames) / self.fps, self.fps),
                ),
            };
            let usable = row.unusable.is_none();
            let (path, stream) = (row.path.clone(), row.stream);
            let dragged = (path.clone(), stream);
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
                        .child(div().truncate().text_size(px(11.)).child(name))
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
                        "opens a file chooser — or drop a file on the window".to_string(),
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
            .child(control(
                "add-asset",
                None,
                "Add at playhead",
                match self.selected_asset {
                    Some(_) => "inserts the picked file at the playhead".to_string(),
                    None => "click a file above first — or drag one onto a lane".to_string(),
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
                        this.insert_source(&path, stream, None, cx);
                    }
                }),
            ))
    }

    /// Transport, edit and file buttons, timecode, playhead, clips lane.
    fn panel(
        &self,
        position: f64,
        duration: f64,
        playing: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let filled = if duration > 0. {
            (position / duration) as f32
        } else {
            0.
        };
        // An export owns the hint slot and the ruler while it runs: the
        // percentage and the accent bar are the same number, so the playhead
        // fill doubles as the progress bar for free.
        let exporting = self.exporting().is_some();
        // Everything but Import and Keys needs a timeline to act on: with none
        // open they are dimmed rather than silently doing nothing.
        let live = self.session.is_some() && !exporting;
        let key = |action| self.keymap.display(action);
        let (hint, filled) = if let Some(export) = self.exporting() {
            let progress = export.progress();
            (
                format!(
                    "EXPORTING {}% — {} cancels",
                    (progress * 100.) as u32,
                    key(ActionId::CancelExport)
                ),
                progress,
            )
        } else {
            // The strokes no button carries; the rest ride on the buttons'
            // tooltips. Keys first: at a 640 px window the tail is what a
            // truncation eats, and the two hints at the end are also on the
            // ruler's and Import's tooltips.
            (
                format!(
                    "{} copy · {} paste · {} undo · click the bar to seek · drop a file to import",
                    key(ActionId::Copy),
                    key(ActionId::Paste),
                    key(ActionId::Undo)
                ),
                filled,
            )
        };
        div()
            .flex_none()
            .h(px(PANEL_H))
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .bg(rgb(CHROME))
            // Transport | edit | file: three groups, so the eye can skip two of
            // them. Every button says what it does; the tooltip adds its key.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(control(
                        "transport",
                        Some(transport_glyph(playing).into_any_element()),
                        if playing { "Pause" } else { "Play" },
                        key(ActionId::Play),
                        live,
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
                    .child(separator())
                    // Import is not here: it belongs to the media list it adds
                    // to, and two doors into one action is a question about
                    // which one is the real one.
                    .child(control(
                        "export",
                        None,
                        "Export",
                        format!(
                            "{} — quality and destination, then writes the timeline out",
                            key(ActionId::Export)
                        ),
                        live && self.export.is_none(),
                        cx.listener(|this, _: &ClickEvent, _, cx| this.open_export(cx)),
                    ))
                    .child(control(
                        "save",
                        None,
                        "Save",
                        format!("{} — writes the project file", key(ActionId::Save)),
                        live,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.save_project(cx)),
                    ))
                    // No shortcut of its own -- and closed while an export runs,
                    // which is what keeps a waiting row from swallowing the
                    // escape the progress line promises cancels the export.
                    .child(control(
                        "keys",
                        None,
                        "Keys",
                        "show and change the keybindings".to_string(),
                        !exporting,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.keys_open = !this.keys_open;
                            this.rebinding = None;
                            // One card at a time, both ways round.
                            this.export_open = false;
                            cx.notify();
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
                    .child(div().flex_none().w(px(TIME_W)).truncate().child(format!(
                        "{} / {}",
                        timecode(position, self.fps),
                        timecode(duration, self.fps)
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
                            .tooltip(|_, cx| cx.new(|_| Tip("Seek — click or drag".into())).into())
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
                                            .w(relative(filled))
                                            .rounded(px(3.))
                                            .bg(rgb(ACCENT)),
                                    ),
                            ),
                    ),
            )
            .child(self.lane_row(Lane::V1, "V1", duration, filled, cx))
            .child(self.lane_row(Lane::A1, "A1", duration, filled, cx))
    }

    /// A notice holds its own bar, full width, until it is answered: any key
    /// retires it (the key handler) and so does a click on it. Its own surface
    /// because the message is the point -- a failure cut to the timecode's slot
    /// is a failure nobody read.
    fn notice_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let notice = self.notice.clone()?;
        Some(
            div()
                .id("notice")
                .flex_none()
                .flex()
                .items_start()
                .gap(px(12.))
                .px(px(12.))
                .py(px(6.))
                .bg(rgb(SURFACE))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.notice = None;
                    cx.notify();
                }))
                .child(div().flex_1().min_w(px(0.)).child(notice))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(rgb(INK_DIM))
                        .child("click or press any key to dismiss"),
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
        // Every stroke that works, under its heading, and both halves of the
        // list come from the registry: one row per action -- an action with two
        // strokes reads as one line ("x or delete") and a rebind replaces that
        // whole set -- then the strokes the modal cards answer to, which are
        // shown but not offered, because nothing may unbind the way out.
        let mut rows: Vec<AnyElement> = Vec::new();
        for category in keymap::Category::ALL {
            let actions = ActionId::ALL
                .into_iter()
                .enumerate()
                .filter(|(_, a)| a.category() == category);
            let fixed = keymap::FIXED.iter().filter(|f| f.category == category);
            let mut headed = false;
            let mut head = |rows: &mut Vec<AnyElement>| {
                if !std::mem::replace(&mut headed, true) {
                    rows.push(
                        div()
                            .flex_none()
                            .px(px(6.))
                            .pt(px(4.))
                            .text_size(px(11.))
                            .text_color(rgb(INK_DIM))
                            .child(category.label())
                            .into_any_element(),
                    );
                }
            };
            for (i, action) in actions {
                head(&mut rows);
                let capturing = self.rebinding == Some(action);
                let out = out.clone();
                rows.push(
                    div()
                        .id(("bind", i))
                        .flex()
                        // The floor, not the height: a row that needed two lines
                        // would otherwise paint over the one under it.
                        .min_h(px(KEYS_ROW_H))
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .px(px(6.))
                        .rounded(px(3.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(HOVER)))
                        .when(capturing, |d| d.bg(rgb(SELECTED)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.rebinding = Some(action);
                            cx.notify();
                        }))
                        .child(action.label())
                        .child(if capturing {
                            div()
                                .text_color(rgb(INK_DIM))
                                .child(format!("press a key — {out} cancels"))
                        } else {
                            div().child(self.keymap.display(action))
                        })
                        .into_any_element(),
                );
            }
            for f in fixed {
                head(&mut rows);
                rows.push(
                    div()
                        .flex()
                        .min_h(px(KEYS_ROW_H))
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .px(px(6.))
                        .child(f.label)
                        // Dim, and no hover: this one is not a row you can click.
                        .child(div().text_color(rgb(INK_DIM)).child(f.chord))
                        .into_any_element(),
                );
            }
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_center()
                // The picture behind is out of reach, and looks it. The press is
                // swallowed here so a button under the scrim cannot take it.
                .bg(rgba(0x101010cc))
                .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                    cx.stop_propagation()
                })
                .child(
                    div()
                        .w(px(KEYS_W))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        // Title and instruction are two children, never one
                        // wrapping line: a fixed-height slot whose text wrapped
                        // painted its second line over the first row.
                        .child(div().flex_none().px(px(6.)).child("Keybindings"))
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
                                        .unwrap_or_else(|| "click a row, then press a key".into()),
                                ),
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
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                ),
        )
    }

    /// What an export is going to be, before there is one: the destination, the
    /// quality rows, and the two things that are not a choice at all. The same
    /// scrim, width and row shape as the keybindings overlay -- two cards of
    /// different builds over one window read as two different programs -- and
    /// the same plain divs, so the root keeps the keyboard and a typed digit
    /// reaches the custom row.
    fn export_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.export_open {
            return None;
        }
        let row = |id: (&'static str, usize)| {
            div()
                .id(id)
                .flex()
                // The floor, not the height: the destination's path wraps on a
                // long name and must not paint over the row under it.
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
            d.when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(rgb(HOVER))))
        };
        // The formats first: the quality rows below are *video* bitrate, so
        // which file is being written decides whether they mean anything.
        let formats: Vec<_> = FORMATS
            .into_iter()
            .enumerate()
            .map(|(i, (format, label, detail))| {
                let picked = format == Some(self.format);
                live(row(("format", i)), format.is_some())
                    .when(picked, |d| d.bg(rgb(SELECTED)))
                    .when_some(format, |d, format| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_format(format);
                            cx.notify();
                        }))
                    })
                    .child(label)
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(INK_DIM))
                            .child(detail),
                    )
            })
            .collect();
        let video = self.format.has_video();
        let rows: Vec<_> = Quality::ALL
            .into_iter()
            .enumerate()
            .map(|(i, quality)| {
                live(row(("quality", i)), video)
                    .when(video && self.quality == quality, |d| d.bg(rgb(SELECTED)))
                    .when(video, |d| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.quality = quality;
                            cx.notify();
                        }))
                    })
                    .child(quality.label())
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(INK_DIM))
                            .child(quality.detail(self.custom_mbps)),
                    )
            })
            .collect();
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(0x101010cc))
                .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                    cx.stop_propagation()
                })
                .child(
                    div()
                        .w(px(KEYS_W))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(SURFACE))
                        .child(div().flex_none().px(px(6.)).child("Export"))
                        // The status line, where a refusal from the save dialog
                        // lands: the notice bar it would otherwise take is under
                        // the scrim.
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(self.notice.clone().unwrap_or_else(|| {
                                    "pick a format (m/w/f), then Export — esc closes".into()
                                })),
                        )
                        // Capped and scrolling like the keybindings list: the
                        // destination, six formats and five qualities are more
                        // rows than a 360 px window has room for, and it is the
                        // list that scrolls, never the card that grows.
                        .child(
                            div()
                                .id("export-rows")
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .max_h(px(EXPORT_ROWS_H))
                                .overflow_y_scroll()
                                .child(
                                    live(row(("destination", 0)), true)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.pick_destination(cx)
                                        }))
                                        .child("Destination")
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(rgb(INK_DIM))
                                                .child(file_name(&self.export_path)),
                                        ),
                                )
                                .children(formats)
                                .children(rows),
                        )
                        // What the picked row really writes. `moov` last is the
                        // muxer's own shape -- an mp4 is playable only once it
                        // is finished.
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child(format_line(self.format)),
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
                                .bg(rgb(SELECTED))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(HOVER)))
                                .on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.start_export(cx)
                                    }),
                                )
                                .child("Export"),
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
        let playhead = frame_at(session.now(), self.fps);
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
                ("This clip", secs(clip.len())),
                (
                    "Source duration",
                    secs(source_frames(
                        lane_clips(session),
                        session.sources(),
                        &source.path,
                    )),
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
            for action in MENU_ITEMS {
                let enabled = applicable(&clip, action, playhead);
                // The one item that is not about this clip says so, and says it
                // here rather than in the registry: the stroke is global too,
                // but its row in the keys menu is not sitting on a clip.
                let label = if action == ActionId::ToggleMute {
                    format!("{} (global)", action.label())
                } else {
                    action.label().to_string()
                };
                rows.push(
                    row(rows.len())
                        .child(label)
                        .child(
                            div()
                                .text_color(rgb(INK_DIM))
                                .child(self.keymap.display(action)),
                        )
                        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(rgb(HOVER)))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    // Closed first: the action moves the very
                                    // indices this menu is holding.
                                    this.context_menu = None;
                                    this.act(action, cx);
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
        let (x, y) = menu_at(
            menu.at,
            viewport,
            MENU_PAD * 2. + rows.len() as f32 * MENU_ROW_H,
        );
        let full: SharedString = source.path.display().to_string().into();
        Some(
            div()
                .absolute()
                .inset_0()
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
                        .children(rows),
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
    fn lane_row(
        &self,
        lane: Lane,
        name: &'static str,
        duration: f64,
        filled: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // The bed's own width, measured last repaint off the ruler's bar: the
        // two are laid out identically (same header offset, same `flex_1`), so
        // one probe answers for both. Zero before the first paint, which only
        // costs the labels one frame.
        let bed_w = f32::from(self.ruler.get().size.width);
        let (clips, others) = match &self.session {
            Some(session) => (
                session.lane_clips(lane),
                session.lane_clips(if lane.kind == LaneKind::Video {
                    Lane::A1
                } else {
                    Lane::V1
                }),
            ),
            None => (&[][..], &[][..]),
        };
        let sources = self
            .session
            .as_ref()
            .map_or(&[][..], PlaybackSession::sources);
        let (sel, sel_link) = (self.selected, self.selected_link());
        let audio = lane.kind == LaneKind::Audio;
        let tip: SharedString = format!(
            "Select — {} removes the take, {} leaves a gap, {} rejoins a cut",
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
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .bg(rgb(SURFACE))
                    .text_size(px(11.))
                    .text_color(rgb(INK_DIM))
                    .child(name),
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
                    // Add button makes, through the same call -- at the
                    // playhead, not at the pointer: clips here are placed end
                    // to end, so where along the bed it landed says nothing.
                    .on_drop(cx.listener(move |this, drag: &AssetDrag, _, cx| {
                        this.insert_source(&drag.0.clone(), drag.1, Some(lane), cx)
                    }))
                    .drag_over::<AssetDrag>(|s, _, _, _| s.bg(rgb(HOVER_DIM)))
                    .children(clips.iter().enumerate().map(|(i, clip)| {
                        let (start, len) = (
                            f64::from(clip.start) / self.fps,
                            f64::from(clip.len()) / self.fps,
                        );
                        let on = marked((lane, i), clip.link, sel, sel_link);
                        // A group with a half in the other lane wears its tint;
                        // one without is outlined, so a detached half is visible
                        // as detached before anyone clicks it.
                        let grouped =
                            clip.link.is_some() && others.iter().any(|o| o.link == clip.link);
                        // Tinted by *file*, not by source entry: two audio
                        // streams of one file are two sources, and the library
                        // gives them one swatch because they are one file.
                        let tint = source_tint(
                            sources
                                .get(clip.source)
                                .and_then(|s| sources.iter().position(|o| o.path == s.path))
                                .unwrap_or(clip.source),
                        );
                        let width = width_frac(len, duration);
                        let label = sources.get(clip.source).map(|s| file_name(&s.path));
                        let wave = sources
                            .get(clip.source)
                            .and_then(|s| self.waves.get(&(s.path.clone(), s.audio_stream)))
                            .cloned();
                        let (from, to) = (
                            f64::from(clip.in_frame) / self.fps,
                            f64::from(clip.out_frame) / self.fps,
                        );
                        let tip = tip.clone();
                        div()
                            .id((if audio { "aclip" } else { "vclip" }, i))
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(relative(start_frac(start, duration)))
                            .w(relative(width))
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
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    this.selected = Some((lane, i));
                                    cx.notify();
                                }),
                            )
                            // The right button selects exactly as the left one
                            // does -- the menu acts on the clip it names, so
                            // opening one has to pick it -- and then hangs the
                            // menu at the pointer.
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    if this.modal() {
                                        return;
                                    }
                                    this.selected = Some((lane, i));
                                    this.context_menu = Some(ContextMenu {
                                        lane,
                                        idx: i,
                                        at: event.position,
                                        details: false,
                                    });
                                    cx.notify();
                                }),
                            )
                            // Under the label row, never through it.
                            .children(wave.filter(|_| audio).and_then(|wave| {
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
                                };
                                Some(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .right_0()
                                        .top(px(LABEL_H))
                                        .bottom_0()
                                        .child(inner),
                                )
                            }))
                            .when_some(label.filter(|_| show_label(bed_w * width)), |d, label| {
                                d.child(
                                    div()
                                        .relative()
                                        .h(px(LABEL_H))
                                        .px(px(4.))
                                        .truncate()
                                        .text_size(px(10.))
                                        .child(label),
                                )
                            })
                    }))
                    // Last, so it is over the clips: the same fraction in both
                    // lanes, which is the playhead being one line.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(relative(filled))
                            .w(px(1.))
                            .bg(rgb(ACCENT)),
                    ),
            )
    }
}

/// Rate limit for scrub seeks: a video worker reopen costs 72-87 ms on the
/// hardware path (215 ms in software), so one seek per mouse move would only
/// queue workers that are cancelled before they decode anything.
const SCRUB_GAP: Duration = Duration::from_millis(100);

fn scrub_due(target: u32, last_target: u32, since: Duration) -> bool {
    target != last_target && since >= SCRUB_GAP
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

fn pick_file() -> Result<Option<PathBuf>, &'static str> {
    run_picker([
        (
            "zenity",
            vec!["--file-selection".into(), "--title=edith — import".into()],
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

/// The same, for the per-file stream probe: one entry per *file*, however many
/// of its streams the timeline plays.
fn unseen_paths(sources: &[Source], streams: &HashMap<PathBuf, Vec<StreamInfo>>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for s in sources {
        if !streams.contains_key(&s.path) && !out.contains(&s.path) {
            out.push(s.path.clone());
        }
    }
    out
}

/// Both lanes' clips, which is everything the timeline knows about its sources.
fn lane_clips(session: &PlaybackSession) -> impl Iterator<Item = &Clip> {
    session
        .lane_clips(Lane::V1)
        .iter()
        .chain(session.lane_clips(Lane::A1))
}

/// How long a source is, as the timeline knows it: the furthest frame any clip
/// plays from *the file*. Exact for a source the timeline still holds whole --
/// which is every source the moment it is imported -- and never longer than the
/// file, since every clip was checked against it, so a clip built from this can
/// always be played. Zero for a file nothing plays from any more.
///
/// By file rather than by source entry, because a second audio stream of a file
/// already on the timeline is a row that has no clips of its own yet, and every
/// stream of a file is the same length -- the picture is what the length is.
///
/// ponytail: a source trimmed on the timeline reads as its trimmed length. The
/// file's own length would need a `Demuxer::open` probe per source, off the
/// render path like the peak decode is -- that is the upgrade path.
fn source_frames<'a>(
    clips: impl Iterator<Item = &'a Clip>,
    sources: &[Source],
    path: &Path,
) -> u32 {
    clips
        .filter(|clip| sources.get(clip.source).is_some_and(|s| s.path == path))
        .map(|clip| clip.out_frame)
        .max()
        .unwrap_or(0)
}

/// Which timeline frame the playhead is on, by the rule the engine's own edits
/// use (playback.rs `secs_to_frame`): the frame that has started, with the
/// epsilon that keeps a clock sitting exactly on a boundary from reading as the
/// frame before it. Only ever a hint here -- what an edit does is still the
/// engine's answer, taken from the same seconds.
fn frame_at(secs: f64, fps: f64) -> u32 {
    (secs * fps + 1e-6).floor().max(0.) as u32
}

/// Whether the clip menu offers `action` on the clip it was opened on. Two of
/// the items act on the *playhead* rather than on the clip, so a menu opened
/// away from it dims them instead of looking broken when they are clicked.
fn applicable(clip: &Clip, action: ActionId, playhead: u32) -> bool {
    match action {
        // Splits this clip only from inside it: at either edge there is nothing
        // to split off (project.rs `splittable`).
        ActionId::Cut => clip.start < playhead && playhead < clip.end(),
        // Rejoins whatever meets at the playhead, so it can mean something only
        // at an edge of this clip. Whether those two halves were ever one take
        // is the engine's question, and it words that refusal itself.
        ActionId::Regroup => playhead == clip.start || playhead == clip.end(),
        _ => true,
    }
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
            // Only where there is a choice to describe: a file with one audio
            // track is the row it has always been, name and length, and the
            // length is what would be squeezed out at the panel's least width.
            detail: info
                .filter(|_| of_file.len() > 1)
                .map_or_else(String::new, stream_detail),
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
    match detail.is_empty() {
        true => tail.to_string(),
        false => format!("{detail} · {tail}"),
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
        parts.push(lang.clone());
    }
    if info.sample_rate > 0 {
        parts.push(format!("{} kHz", f64::from(info.sample_rate) / 1000.));
    }
    parts.extend(layout(info.channels));
    parts.join(" ")
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
        // ponytail: mp4 0.14 keeps no fourcc for a sample entry it does not
        // parse (`StreamInfo::codec`), so an AC-3 track can only be named as
        // unsupported, not as AC-3. Upgrade path is reading the stsd ourselves.
        return Some(match info.codec.as_str() {
            "unknown" => "unsupported codec".to_string(),
            codec => format!("{codec} is not supported"),
        });
    }
    let (rate, channels) = timeline_audio?;
    ((info.sample_rate, info.channels) != (rate, channels)).then(|| {
        format!(
            "the timeline is {} kHz {}",
            f64::from(rate) / 1000.,
            layout(channels).unwrap_or_else(|| "silent".to_string())
        )
    })
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
            Quality::Custom => format!("{custom_mbps} Mbps — type a number, 1–20"),
            other => format!(
                "{} Mbps",
                export_settings(other, 0, Format::Mp4)
                    .bitrate
                    .unwrap_or_default()
                    / 1_000_000
            ),
        }
    }
}

/// The export card's format rows: what this program can write, and what it
/// cannot with the reason it cannot. A format with no entry at all would read
/// as an oversight, and a menu of three as a claim that nothing else exists --
/// so the refusals are rows too, dimmed and unclickable.
///
/// `None` is exactly that kind of row. The MP3 reason is a *licence* and not a
/// capability: `shine-rs` encodes mp3 in pure Rust under LGPL-2.0, which is a
/// decision about this project rather than about the code, and the row says so
/// instead of pretending the encoder does not exist.
const FORMATS: [(Option<Format>, &str, &str); 6] = [
    (Some(Format::Mp4), "MP4", "H.264 picture + copied AAC"),
    (Some(Format::Wav), "WAV", "16-bit PCM — audio only"),
    (Some(Format::Flac), "FLAC", "lossless — audio only"),
    (None, "MP3", "encoder is LGPL — licence decision pending"),
    (None, "OGG", "no pure-Rust Vorbis or Opus encoder"),
    (None, "AAC", "no pure-Rust AAC encoder"),
];

/// The format a key picks while the export card is up: the row's own initial,
/// which is unambiguous across the three that can be picked. `None` for
/// everything else, digits included -- those are the bitrate's.
fn format_key(key: &str) -> Option<Format> {
    FORMATS
        .into_iter()
        .filter_map(|(format, label, _)| format.zip(label.get(..1)))
        .find(|(_, initial)| initial.eq_ignore_ascii_case(key))
        .map(|(format, _)| format)
}

/// The line under the rows: what the picked format really writes, in the terms
/// a file is judged by afterwards.
fn format_line(format: Format) -> &'static str {
    match format {
        Format::Mp4 => "H.264 · MP4 · moov at end",
        Format::Wav => "16-bit PCM · WAV · timeline audio only",
        Format::Flac => "FLAC · lossless · timeline audio only",
    }
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
fn export_settings(quality: Quality, custom_mbps: u32, format: Format) -> ExportSettings {
    ExportSettings {
        format,
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
    }
}

/// A digit typed against the custom row, appended to what is already there.
/// Refused rather than truncated past two digits: the engine's ceiling is
/// 20 Mbps, so a third digit can only be a mistake, and a silently dropped one
/// would leave the card showing a number nobody typed.
fn push_digit(mbps: u32, digit: u32) -> u32 {
    let next = mbps * 10 + digit;
    if next <= 99 { next } else { mbps }
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
fn width_frac(len: f64, total: f64) -> f32 {
    if total > 0. { (len / total) as f32 } else { 1. }
}

/// Where along the lane a clip starts, 0..1. Unlike [`width_frac`] a timeline
/// with no length pins this to the left edge -- a full-width offset would push
/// the box out of the lane it belongs to.
fn start_frac(start: f64, total: f64) -> f32 {
    if total > 0. {
        (start / total).clamp(0., 1.) as f32
    } else {
        0.
    }
}

/// The video-lane index of the take an audio clip belongs to, if the video lane
/// still holds that half. `None` for a half whose picture was lifted -- which is
/// what makes it a thing of its own to delete.
fn video_half(session: &PlaybackSession, audio: usize) -> Option<usize> {
    let link = session.lane_clips(Lane::A1).get(audio)?.link?;
    session
        .lane_clips(Lane::V1)
        .iter()
        .position(|clip| clip.link == Some(link))
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
    let cols = (w / WAVE_COL).ceil().max(1.) as usize;
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

/// The line between two groups of buttons.
fn separator() -> impl IntoElement {
    div()
        .flex_none()
        .mx(px(4.))
        .w(px(1.))
        .h(px(18.))
        .bg(rgb(HOVER))
}

/// A tooltip is a view in gpui and nothing smaller, so this is the smallest one
/// that carries a line of text. It paints outside the window's element tree and
/// therefore owns its colours.
struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
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

/// Two bars while playing, a triangle while paused. Drawn, so there is no icon
/// font and no glyph coverage to depend on.
fn transport_glyph(playing: bool) -> impl IntoElement {
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

#[cfg(test)]
mod tests {
    use super::{
        ACCENT, CONTROL_H, Clip, EXPORT_ROWS_H, FORMATS, Format, HEADER_GAP, HEADER_W, HIT_MIN,
        INK, INK_DIM, KEYS_ROW_H, KEYS_ROWS_H, KEYS_W, LABEL_H, LABEL_MIN_W, LANE_H, LETTERBOX,
        LIBRARY_MAX_W, LIBRARY_MIN_W, Lane, MENU_ITEMS, MENU_W, NO_FILE, PANEL_H, Quality, ROW_H,
        RULER_HIT_H, SELECTED, SOURCE_TINTS, SURFACE, SWATCH_W, Source, StreamInfo, Volume,
        WAVE_BPS, WAVE_COL, Wave, applicable, can_add, cancels_export, envelope, export_path,
        export_settings, format_line, frac_along, frame_at, is_bare_modifier, is_project, keymap,
        lane_clips, marked, menu_at, normalise, project_path, push_digit, retarget, scrub_due,
        show_label, source_frames, source_tint, start_frac, timecode, unseen_paths, unseen_sources,
        width_frac, window_title,
    };
    use super::{file_name, library_rows};
    use engine::PlaybackSession;
    use gpui::{Bounds, Pixels, point, px, size};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    /// A clip of `source`, `frames` long, at the top of the timeline. Only the
    /// two fields the library reads are meant to be looked at.
    fn clip(source: usize, frames: u32) -> Clip {
        Clip {
            start: 0,
            in_frame: 0,
            out_frame: frames,
            source,
            link: None,
        }
    }

    /// A source entry for `path` on `stream`, as the project keeps them.
    fn source(path: &str, stream: usize) -> Source {
        Source {
            path: PathBuf::from(path),
            audio_stream: stream,
        }
    }

    /// The whole of the library's row data comes off the clips: which files
    /// there are, how long each one is, and therefore which tint each row wears
    /// -- one index into one list, so a swatch cannot name a different file from
    /// the boxes it is meant to point at.
    #[test]
    fn a_rows_swatch_and_duration_come_off_the_clips_that_name_it() {
        // Source 0 whole, source 1 trimmed to half of what it was, source 2 on
        // the audio lane only (its picture was lifted), source 3 imported and
        // then undone -- an entry with nothing playing from it.
        let sources: Vec<Source> = (0..4).map(|i| source(&format!("/m/{i}.mp4"), 0)).collect();
        let video = [clip(0, 150), clip(1, 60)];
        let audio = [clip(0, 150), clip(1, 30), clip(2, 90)];
        for (row, frames) in [(0, 150), (1, 60), (2, 90), (3, 0)] {
            assert_eq!(
                source_frames(video.iter().chain(&audio), &sources, &sources[row].path),
                frames,
                "row {row}"
            );
            // The swatch is the clip colour, by the same index and the same
            // function -- what makes the panel and the lanes one association.
            assert_eq!(source_tint(row), SOURCE_TINTS[row % SOURCE_TINTS.len()]);
        }
        // The longest clip of a file wins, whichever lane it is in and
        // whatever order the lanes are read in.
        let one = [source("/m/0.mp4", 0)];
        let path = &one[0].path;
        assert_eq!(
            source_frames([clip(0, 10), clip(0, 90)].iter(), &one, path),
            90
        );
        assert_eq!(
            source_frames([clip(0, 90), clip(0, 10)].iter(), &one, path),
            90
        );
        assert_eq!(source_frames([].iter(), &one, path), 0);
        // Two audio streams of one file are two source entries and one length:
        // the second stream's row is placeable the moment the file is there,
        // before any clip names that entry.
        let two = [source("/m/0.mp4", 0), source("/m/0.mp4", 1)];
        assert_eq!(source_frames([clip(0, 90)].iter(), &two, &two[1].path), 90);
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
        let rows = library_rows(&sources, &streams, Some((44_100, 2)), |_| 90);
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
        // it. The 22 kHz mono track cannot join a 44.1 kHz stereo timeline --
        // one device and one copied AAC track for the whole of it -- and the
        // codec we cannot read cannot join anything. Both say which.
        assert_eq!((&rows[0].unusable, &rows[1].unusable), (&None, &None));
        assert_eq!(
            rows[2].unusable.as_deref(),
            Some("the timeline is 44.1 kHz stereo")
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
        let rows = library_rows(&plain, &one, Some((44_100, 2)), |_| 90);
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
            let rows = library_rows(&plain, &probe, None, |_| 90);
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
        let rows = library_rows(&placed, &streams, Some((44_100, 2)), |_| 90);
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
        };
        assert_eq!(clip.end(), 90);
        // Cut splits from inside only: neither edge has anything to split off.
        assert!(applicable(&clip, ActionId::Cut, 31));
        assert!(applicable(&clip, ActionId::Cut, 89));
        assert!(!applicable(&clip, ActionId::Cut, 30));
        assert!(!applicable(&clip, ActionId::Cut, 90));
        assert!(!applicable(&clip, ActionId::Cut, 200));
        // Regroup is the other way round: only where this clip meets another.
        assert!(applicable(&clip, ActionId::Regroup, 30));
        assert!(applicable(&clip, ActionId::Regroup, 90));
        assert!(!applicable(&clip, ActionId::Regroup, 60));
        // The rest act on the clip that was clicked, so they always mean
        // something -- the engine words its own refusals.
        for action in [ActionId::Delete, ActionId::Lift, ActionId::ToggleMute] {
            assert!(applicable(&clip, action, 0));
            assert!(applicable(&clip, action, 60));
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
        let refusal = session
            .import(&asset("test_mismatch.mp4"))
            .expect_err("640x360 must not join a 1280x720 timeline")
            .to_string();
        assert!(refusal.contains("640"), "refusal must name it: {refusal}");
        assert_eq!(session.sources().len(), 1, "a refusal added a row");
        // An accepted one does add a row, and it reads as the whole file: 4 s
        // at 30 fps, exactly what was appended.
        session.import(&asset("test_av2.mp4")).expect("av2 matches");
        assert_eq!(session.sources().len(), 2);
        let second = session.sources()[1].path.clone();
        assert_eq!(
            source_frames(lane_clips(&session), session.sources(), &second),
            120
        );
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
        assert_eq!(session.timeline_duration(), 9.0);
        // Two seconds in, which is inside the first take: the insert splits it.
        session.seek(2.0);
        let second = session.sources()[1].path.clone();
        let frames = source_frames(lane_clips(&session), session.sources(), &second);
        // Through the engine door `insert_source` uses, with the row's own
        // stream: the button, the drop and this are one call.
        assert!(
            session
                .place_stream_at(2.0, &second, 0, frames)
                .expect("av2 is already on the timeline")
        );
        // The whole of source 1 went in and nothing was painted over: the
        // timeline is longer by exactly that file.
        assert_eq!(session.timeline_duration(), 13.0);
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
        let frames = source_frames(lane_clips(&session), session.sources(), &path);
        let end = session.timeline_duration();
        assert!(
            session
                .place_stream_at(end, &path, 1, frames)
                .expect("the French track shares the timeline's parameters")
        );
        assert_eq!(session.sources()[1].audio_stream, 1);
        assert_eq!(session.timeline_duration(), end * 2.0);
        // Both rows are the same file, so both rows are that file's length --
        // the second one before any clip of its own existed.
        assert_eq!(
            source_frames(lane_clips(&session), session.sources(), &path),
            frames
        );
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
        for fixed in &keymap::FIXED {
            assert!(keymap::Category::ALL.contains(&fixed.category));
        }
    }

    #[test]
    fn clip_boxes_split_the_lane_by_duration() {
        // 1 s + 3 s of a 4 s timeline: a quarter and three quarters.
        assert_eq!(width_frac(1., 4.), 0.25);
        assert_eq!(width_frac(3., 4.), 0.75);
        assert_eq!(width_frac(4., 4.), 1.);
        // A timeline with no length must not hand gpui a NaN width.
        assert_eq!(width_frac(0., 0.), 1.);
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

    #[test]
    fn the_keybindings_card_fits_the_smallest_window() {
        // The row list is capped and scrolls, so the card's height no longer
        // depends on how many actions there are: a title, a status line and the
        // viewport, inside the 640x360 the rest of the layout is sized for.
        let title = 17.; // 13 px text on its own line
        let status = 28.; // 11 px text, two lines: a refusal wraps
        let gaps = 3. * 2.;
        let padding = 24.;
        assert!(
            title + status + KEYS_ROWS_H + gaps + padding <= 360.,
            "card too tall"
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
        assert_eq!(export_settings(Quality::Auto, 7, mp4).bitrate, None);
        assert_eq!(
            export_settings(Quality::Low, 0, mp4).bitrate,
            Some(2_000_000)
        );
        assert_eq!(
            export_settings(Quality::Medium, 0, mp4).bitrate,
            Some(6_000_000)
        );
        assert_eq!(
            export_settings(Quality::High, 0, mp4).bitrate,
            Some(12_000_000)
        );
        // Megabits as typed, and as the row says it back.
        assert_eq!(
            export_settings(Quality::Custom, 7, mp4).bitrate,
            Some(7_000_000)
        );
        assert_eq!(Quality::Low.detail(0), "2 Mbps");
        // The picked format travels, or the card's rows would be a picture of a
        // choice the engine never hears about.
        for format in [Format::Mp4, Format::Wav, Format::Flac] {
            assert_eq!(export_settings(Quality::Auto, 0, format).format, format);
        }
        // Every fixed row sits inside the engine's clamp (export.rs:290), so no
        // row can promise a bitrate the exporter silently changes.
        for quality in Quality::ALL {
            let settings = export_settings(quality, 7, mp4);
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

    #[test]
    fn a_typed_bitrate_stops_at_two_digits() {
        assert_eq!(push_digit(0, 8), 8);
        assert_eq!(push_digit(1, 2), 12);
        // A third digit is refused whole: the ceiling is 20 Mbps, so it can
        // only be a mistake, and dropping it silently would leave the card
        // showing a number nobody typed.
        assert_eq!(push_digit(12, 3), 12);
        assert_eq!(push_digit(99, 9), 99);
        // Never past what the clamp can take back to a real bitrate.
        assert!(u64::from(push_digit(99, 9)) * 1_000_000 < u64::from(u32::MAX));
    }

    #[test]
    fn the_export_card_fits_the_smallest_window() {
        // Same 640x360 floor the keybindings card is measured against: the
        // capped row list, the fixed-format line and the confirm button, under
        // a title and a status line.
        let title = 17.;
        let status = 28.;
        let fixed = 17.;
        let gaps = 4. * 2.;
        let padding = 24.;
        assert!(
            title + status + EXPORT_ROWS_H + fixed + CONTROL_H + 4. + gaps + padding <= 360.,
            "card too tall"
        );
        // The cap is only honest if enough of the list is on screen to read as
        // one -- the destination and the first formats, not a slot.
        assert!(EXPORT_ROWS_H / KEYS_ROW_H >= 6.);
        // Clickable rows, so WCAG 2.5.8 binds them as it binds the panel's.
        assert!(KEYS_ROW_H >= HIT_MIN);
        assert!(CONTROL_H >= HIT_MIN);
    }

    #[test]
    fn every_format_row_is_offered_or_says_why_not() {
        // The three this program can write are rows that pick, each named after
        // its own extension so a destination can never disagree with its bytes.
        let offered: Vec<Format> = FORMATS.iter().filter_map(|&(f, ..)| f).collect();
        assert_eq!(offered, vec![Format::Mp4, Format::Wav, Format::Flac]);
        for (format, label, _) in FORMATS {
            match format {
                Some(format) => assert_eq!(format.ext(), label.to_lowercase()),
                // A refused format is a row with a reason, never a hidden one:
                // an empty detail column would read as an oversight.
                None => assert!(!label.is_empty()),
            }
        }
        for (format, label, detail) in FORMATS {
            assert!(!detail.is_empty(), "{label} says nothing");
            // Only mp4 carries the picture, so only mp4 leaves the bitrate rows
            // live -- the card dims them off exactly this.
            assert_eq!(
                format.is_some_and(Format::has_video),
                format == Some(Format::Mp4)
            );
        }
        // The destination follows the format and keeps the stem, mp4 included.
        assert_eq!(
            retarget(std::path::Path::new("/a/take.export.mp4"), Format::Wav),
            std::path::Path::new("/a/take.export.wav")
        );
        assert_eq!(
            retarget(std::path::Path::new("/a/take.export.wav"), Format::Mp4),
            std::path::Path::new("/a/take.export.mp4")
        );
        assert!(format_line(Format::Flac).contains("audio only"));
    }

    #[test]
    fn nothing_clickable_is_smaller_than_the_wcag_minimum() {
        // Every hit target in the panel, including the scrub strip -- whose bar
        // is 6 px to look at and whose click area must not be.
        assert!(CONTROL_H >= HIT_MIN);
        assert!(RULER_HIT_H >= HIT_MIN);
        assert!(LANE_H >= HIT_MIN);
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
        // A timeline with no length must not put a box outside its lane.
        assert_eq!(start_frac(3., 0.), 0.);
        assert_eq!(start_frac(1., 4.), 0.25);
        // Past the end clamps rather than running off the bed.
        assert_eq!(start_frac(8., 4.), 1.);
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
        // The file the session opened with is the unchanged lane colour.
        assert_eq!(source_tint(0), SURFACE);
        // Neighbouring sources must not share one, or an import is invisible.
        assert_ne!(source_tint(0), source_tint(1));
        assert_ne!(source_tint(1), source_tint(2));
        assert_ne!(source_tint(2), source_tint(3));
        // Past the palette it wraps -- never an index panic.
        assert_eq!(source_tint(4), source_tint(0));
        assert_eq!(source_tint(9), source_tint(1));
        assert_eq!(source_tint(usize::MAX), SOURCE_TINTS[usize::MAX % 4]);
    }
}

fn main() {
    let arg = std::env::args().nth(1).map(PathBuf::from);
    // A `.edith` restores a whole timeline, anything else *is* the timeline.
    // Either way the rest of argv is appended to what comes out. No argument at
    // all opens the window empty -- the timeline then arrives by drop or by the
    // Import button, and everything below is derived from it at that point
    // instead.
    let mut session = match &arg {
        Some(arg) => {
            let opened = if is_project(arg) {
                PlaybackSession::open_project(arg)
            } else {
                PlaybackSession::open(arg)
            };
            match opened {
                Ok(v) => Some(v),
                Err(e) => {
                    // A failed load by drop leaves the running session alone;
                    // here a file was named and could not be opened, so the
                    // refusal is the whole run.
                    eprintln!("cannot open {}: {e}", arg.display());
                    std::process::exit(1);
                }
            }
        }
        None => None,
    };
    // A keymap file that cannot be read leaves the defaults in force, and takes
    // the notice slot ahead of an import refusal below: it is about every key
    // the window has, and the import refusal is on stderr either way.
    let (keymap, mut notice) = Keymap::load();
    if let Some(text) = &notice {
        eprintln!("{text}");
    }
    // The first file makes the timeline, the rest are appended to it. A refusal
    // is not fatal -- the others still load -- but the window must not open
    // silently pretending the file was taken, so the first one seeds the notice.
    for extra in std::env::args().skip(2) {
        // Only reachable with a first argument, so there is a timeline.
        if let Some(session) = &mut session
            && let Err(e) = session.import(std::path::Path::new(&extra))
        {
            let text = format!("IMPORT FAILED: {e}");
            eprintln!("{extra}: {text}");
            notice.get_or_insert(text);
        }
    }
    // A file that plays silent is as much news at launch as it is on a drop, and
    // behind the keymap notice for the same reason an import refusal is.
    if let Some(reason) = session
        .as_ref()
        .and_then(PlaybackSession::audio_disabled_reason)
    {
        let text = format!("NO AUDIO: {reason}");
        eprintln!("{text}");
        notice.get_or_insert(text);
    }
    let meta = session.as_ref().map(|session| *session.meta());
    // Beside the media even for a project: an export has never landed anywhere
    // but next to the picture it came from.
    let out = session.as_ref().map_or_else(PathBuf::new, |session| {
        export_path(&session.sources()[0].path)
    });
    let project = match &arg {
        Some(arg) if is_project(arg) => arg.clone(),
        Some(arg) => project_path(arg),
        // Chosen with the file, once there is one to choose it from.
        None => PathBuf::new(),
    };
    let name: SharedString = arg
        .as_deref()
        .map_or_else(|| NO_FILE.into(), |arg| file_name(arg).into());
    if let (Some(arg), Some(meta)) = (&arg, &meta) {
        println!(
            "{}: {}x{} @ {:.2} fps, {} samples",
            arg.display(),
            meta.width,
            meta.height,
            meta.frame_rate,
            meta.frame_count
        );
    }

    Application::new().run(move |cx: &mut App| {
        // The picture's own size, or 720p when there is no picture yet: the
        // empty window is a landing pad, not a sliver.
        let (w, h) = meta.map_or((1280., 720.), |meta| {
            (meta.width as f32, meta.height as f32)
        });
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
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
                let player = cx.new(|cx| Player {
                    // A file named on the command line owes its first frame the
                    // same repaint a seek's still owes: nothing plays by
                    // itself, so this is what carries the poster frame to the
                    // screen. An empty window has none to wait for.
                    pending_seek: session.is_some(),
                    session,
                    // Full and unmuted, which is what the session it was just
                    // handed is already set to: nothing to push at startup.
                    volume: Volume::default(),
                    // Only ever used with a timeline; 30 keeps the empty
                    // timecode reading in frames rather than in NaN.
                    fps: meta.map_or(30., |meta| meta.frame_rate),
                    name: name.clone(),
                    image: None,
                    held: None,
                    eos: false,
                    done: false,
                    ruler: Rc::default(),
                    selected: None,
                    context_menu: None,
                    selected_asset: None,
                    waves: HashMap::new(),
                    streams: HashMap::new(),
                    clipboard: None,
                    scrubbing: false,
                    last_scrub: Instant::now(),
                    last_target: 0,
                    export: None,
                    cancelling: false,
                    export_path: out.clone(),
                    project_path: project.clone(),
                    keymap: keymap.clone(),
                    keys_open: false,
                    export_open: false,
                    // What an export is until someone says otherwise: the
                    // bitrate the picture asks for.
                    quality: Quality::Auto,
                    custom_mbps: 0,
                    // Picture and sound, which is what an export was before
                    // there was anything to pick.
                    format: Format::default(),
                    rebinding: None,
                    notice: notice.clone().map(SharedString::from),
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
