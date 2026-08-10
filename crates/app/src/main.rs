mod keymap;

use keymap::{ActionId, Keymap};

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::export::ExportSettings;
use engine::{Clip, ExportHandle, Frame, PlaybackSession};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, FocusHandle, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, RenderImage,
    SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas, div, img, point,
    prelude::*, px, relative, rgb, rgba, size,
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
/// WCAG 2.5.8: nothing clickable is smaller than this. The scrub bar stays 6 px
/// to look at -- `RULER_HIT_H` is the strip that has to be hit.
const HIT_MIN: f32 = 24.;
const CONTROL_H: f32 = 28.;
const RULER_HIT_H: f32 = HIT_MIN;
/// Wide enough for `HH:MM:SS:FF / HH:MM:SS:FF`, and fixed so changing digits
/// cannot push the layout around.
const TIME_W: f32 = 200.;
/// The keybindings card: a row per action, a title and a status line, inside a
/// 360 px tall window. The rows are click targets, so `HIT_MIN` binds them too.
const KEYS_W: f32 = 320.;
const KEYS_ROW_H: f32 = HIT_MIN;

/// The one key name this file still spells out, and gpui's spelling of it: it
/// is the way out of a capture and out of the overlay, and both have to work
/// while the keymap itself is what is being changed -- so neither can go
/// through the keymap to find it.
const ESCAPE: &str = "escape";

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
    /// Which clip the edit keys act on. Indices move under every edit, so this
    /// is cleared by all of them.
    selected: Option<usize>,
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
    /// The action whose row is waiting for a stroke. The next key that is
    /// neither escape nor a lone modifier becomes the whole of what reaches it.
    rebinding: Option<ActionId>,
    /// What the last file action had to say. Holds its own bar above the panel
    /// until it is answered -- any key retires it, so does a click on it -- so a
    /// failure is read in full instead of blinking past.
    notice: Option<SharedString>,
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

    /// Drops the selected clip. The engine reseeks itself, so all this owes is
    /// the flag reset.
    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let deleted = match (&mut self.session, self.selected.take()) {
            (Some(session), Some(i)) => session.delete_clip(i),
            _ => false,
        };
        if deleted {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Copies the selected clip. Nothing on screen changes, so no notify.
    fn copy_selected(&mut self) {
        let session = self.session.as_ref();
        if let Some(clip) = self.selected.and_then(|i| session?.clip_at(i)) {
            self.clipboard = Some(clip);
        }
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
                self.session = Some(session);
                self.export_path = export_path(path);
                self.project_path = project_path(path);
                self.name = file_name(path).into();
                self.reset_after_reseek();
                format!("OPENED {}", file_name(path))
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
                // A project is named after itself but still exports beside its
                // media: that is the only place an export has ever landed.
                self.export_path = export_path(&session.sources()[0]);
                self.session = Some(session);
                self.project_path = path.to_path_buf();
                self.name = file_name(path).into();
                // A copied clip names its source by index, which means a
                // different file -- or none -- in another project.
                self.clipboard = None;
                self.selected = None;
                // The counters describe one timeline; the eof line must not
                // report the old one's frames against the new one.
                self.displayed = 0;
                self.dropped = 0;
                self.started = None;
                // Loaded paused at its saved playhead, so the still it owes
                // reaches the screen the way a seek's does. The old picture is
                // released by the swap in `pump`, as after any other seek.
                self.reset_after_reseek();
                format!("LOADED {}", file_name(path))
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
                    Ok(Some(path)) => this.export_path = path,
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
        let settings = export_settings(self.quality, self.custom_mbps);
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
                match action {
                    Some(ActionId::Play) => this.toggle_or_restart(cx),
                    Some(ActionId::Export) => this.open_export(cx),
                    Some(ActionId::Save) => this.save_project(cx),
                    Some(ActionId::Copy) => this.copy_selected(),
                    Some(ActionId::Paste) => this.paste(cx),
                    Some(ActionId::Cut) => this.cut(cx),
                    Some(ActionId::Delete) => this.delete_selected(cx),
                    Some(ActionId::Undo) => this.undo(cx),
                    // Nothing to cancel while nothing is exporting.
                    Some(ActionId::CancelExport) | None => {}
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
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
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
                            // With no file open the letterbox is the whole
                            // window, and a black rectangle says only that
                            // something is broken -- so it says what it wants
                            // instead. The window is already the drop target.
                            .or_else(|| {
                                self.session
                                    .is_none()
                                    .then(|| empty_hint().into_any_element())
                            }),
                    ),
            )
            // Above the panel and only when there is one to show, so it costs
            // the picture nothing the rest of the time.
            .children(self.notice_bar(cx))
            .child(self.panel(position, duration, playing, cx))
            // Last, so they are over everything -- they take no room in the
            // column, and only one of the two is ever up.
            .children(self.keys_overlay(cx))
            .children(self.export_card(cx))
    }
}

impl Player {
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
                    .child(separator())
                    .child(control(
                        "import",
                        None,
                        "Import",
                        "opens a file chooser — or drop a file on the window".to_string(),
                        !exporting,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.pick_and_import(cx)),
                    ))
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
                    .id("ruler")
                    .flex_none()
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
            )
            .child(self.clips_lane(duration, cx))
            .child(track_lane())
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
        // One row per action, not per binding: an action with two strokes reads
        // as one line ("x or delete"), and a rebind replaces that whole set.
        let rows: Vec<_> = ActionId::ALL
            .into_iter()
            .enumerate()
            .map(|(i, action)| {
                let capturing = self.rebinding == Some(action);
                let out = out.clone();
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
            })
            .collect();
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
                        // ponytail: a refusal long enough to wrap to three lines
                        // pushes the card past a 360 px tall window. The upgrade
                        // path is a scrolling row list, not a shorter message.
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
                        .children(rows),
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
                .cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
        };
        let rows: Vec<_> = Quality::ALL
            .into_iter()
            .enumerate()
            .map(|(i, quality)| {
                row(("quality", i))
                    .when(self.quality == quality, |d| d.bg(rgb(SELECTED)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.quality = quality;
                        cx.notify();
                    }))
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
                                    "pick a quality, then Export — esc closes".into()
                                })),
                        )
                        .child(
                            row(("destination", 0))
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
                        .children(rows)
                        // Not offered, so said: these are what this program can
                        // write, and a menu of one is a lie about the other
                        // entries. `moov` last is the muxer's own shape -- the
                        // file is playable only once it is finished.
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.))
                                .text_size(px(11.))
                                .text_color(rgb(INK_DIM))
                                .child("H.264 · MP4 · moov at end"),
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

    /// The edit list made visible: one box per clip, sized by its share of the
    /// timeline. A cut adds a box without moving anything, a delete closes the
    /// gap. A box has no room for a label at four clips, so the tooltip is where
    /// it says what clicking it does. Never focusable, so the root keeps focus
    /// and the play binding still works after a click (ledger:182).
    fn clips_lane(&self, duration: f64, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .h(px(LANE_H))
            .flex()
            .gap(px(1.))
            .overflow_hidden()
            .children(
                self.session
                    .as_ref()
                    .map(PlaybackSession::clip_spans_by_source)
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (_, len, source))| {
                        let selected = self.selected == Some(i);
                        let tint = source_tint(source);
                        div()
                            .id(("clip", i))
                            .h_full()
                            .w(relative(width_frac(len, duration)))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(rgb(if selected { ACCENT } else { tint }))
                            .bg(rgb(if selected { SELECTED } else { tint }))
                            .cursor_pointer()
                            .hover(|s| s.border_color(rgb(ACCENT)))
                            .tooltip(|_, cx| {
                                cx.new(|_| Tip("Select clip — Delete removes it".into()))
                                    .into()
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    this.selected = Some(i);
                                    cx.notify();
                                }),
                            )
                    }),
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
                export_settings(other, 0).bitrate.unwrap_or_default() / 1_000_000
            ),
        }
    }
}

/// The card's rows as the engine takes them. `Auto` leaves the bitrate to the
/// exporter, which derives it from the picture; the fixed rows are figures that
/// hold from 720p to 1080p, and a typed one is passed exactly as typed -- the
/// engine clamps every explicit bitrate to 1..20 Mbps (export.rs:290), so this
/// must not clamp it a second time and disagree about where the edge is.
fn export_settings(quality: Quality, custom_mbps: u32) -> ExportSettings {
    ExportSettings {
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

/// A toolbar button: its glyph, its name, and its key on hover. `id` only buys
/// `on_click` and the tooltip -- it is still not focusable, so the root's own
/// key listener keeps working after a press, and the click lands on mouse-up
/// inside the button (a press that slides off does nothing).
///
/// A button that would do nothing says so: dimmed, no pointer, no listener.
fn control(
    id: &'static str,
    glyph: Option<AnyElement>,
    label: &'static str,
    shortcut: String,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
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
                .child("or click Import below to choose one"),
        )
}

/// The second lane: still a placeholder, deliberately empty.
fn track_lane() -> impl IntoElement {
    div()
        .flex_none()
        .h(px(LANE_H))
        .rounded(px(3.))
        .bg(rgb(SURFACE))
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
        CONTROL_H, HIT_MIN, KEYS_ROW_H, KEYS_W, LANE_H, Quality, RULER_HIT_H, SOURCE_TINTS,
        SURFACE, cancels_export, export_path, export_settings, frac_along, is_bare_modifier,
        is_project, keymap, project_path, push_digit, scrub_due, source_tint, timecode, width_frac,
    };
    use gpui::{Bounds, Pixels, point, px, size};
    use std::time::Duration;

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
        // A row per action -- not per binding, which is why the count is
        // `ALL` -- under a title and a status line, inside the 640x360 the rest
        // of the layout is already sized for.
        let rows = keymap::ActionId::ALL.len() as f32;
        let title = 17.; // 13 px text on its own line
        let status = 28.; // 11 px text, two lines: a refusal wraps
        let gaps = (rows + 1.) * 2.;
        let padding = 24.;
        assert!(
            title + status + rows * KEYS_ROW_H + gaps + padding <= 360.,
            "card too tall"
        );
        assert!(KEYS_W <= 640., "card too wide");
        // The rows are clickable, so WCAG 2.5.8 binds them like every other
        // target in this window.
        assert!(KEYS_ROW_H >= HIT_MIN);
    }

    #[test]
    fn a_quality_row_is_the_bitrate_it_promises() {
        // Auto is the one row that says nothing: the exporter derives it, and
        // a number typed against the custom row must not leak into it.
        assert_eq!(export_settings(Quality::Auto, 7).bitrate, None);
        assert_eq!(export_settings(Quality::Low, 0).bitrate, Some(2_000_000));
        assert_eq!(export_settings(Quality::Medium, 0).bitrate, Some(6_000_000));
        assert_eq!(export_settings(Quality::High, 0).bitrate, Some(12_000_000));
        // Megabits as typed, and as the row says it back.
        assert_eq!(export_settings(Quality::Custom, 7).bitrate, Some(7_000_000));
        assert_eq!(Quality::Low.detail(0), "2 Mbps");
        // Every fixed row sits inside the engine's clamp (export.rs:290), so no
        // row can promise a bitrate the exporter silently changes.
        for quality in Quality::ALL {
            let settings = export_settings(quality, 7);
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
        // Same 640x360 floor the keybindings card is measured against: a
        // destination row, one row per quality, the fixed-format line and the
        // confirm button, under a title and a status line.
        let rows = Quality::ALL.len() as f32 + 1.;
        let title = 17.;
        let status = 28.;
        let fixed = 17.;
        let gaps = (rows + 3.) * 2.;
        let padding = 24.;
        assert!(
            title + status + rows * KEYS_ROW_H + fixed + CONTROL_H + 4. + gaps + padding <= 360.,
            "card too tall"
        );
        // Clickable rows, so WCAG 2.5.8 binds them as it binds the panel's.
        assert!(KEYS_ROW_H >= HIT_MIN);
        assert!(CONTROL_H >= HIT_MIN);
    }

    #[test]
    fn nothing_clickable_is_smaller_than_the_wcag_minimum() {
        // Every hit target in the panel, including the scrub strip -- whose bar
        // is 6 px to look at and whose click area must not be.
        assert!(CONTROL_H >= HIT_MIN);
        assert!(RULER_HIT_H >= HIT_MIN);
        assert!(LANE_H >= HIT_MIN);
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
    let meta = session.as_ref().map(|session| *session.meta());
    // Beside the media even for a project: an export has never landed anywhere
    // but next to the picture it came from.
    let out = session
        .as_ref()
        .map_or_else(PathBuf::new, |session| export_path(&session.sources()[0]));
    let project = match &arg {
        Some(arg) if is_project(arg) => arg.clone(),
        Some(arg) => project_path(arg),
        // Chosen with the file, once there is one to choose it from.
        None => PathBuf::new(),
    };
    let name: SharedString = arg
        .as_deref()
        .map_or_else(|| "no file open".into(), |arg| file_name(arg).into());
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
                    rebinding: None,
                    notice: notice.clone().map(SharedString::from),
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
