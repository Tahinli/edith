use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::{Clip, ExportHandle, Frame, PlaybackSession};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, FocusHandle, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, RenderImage,
    SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas, div, img, point,
    prelude::*, px, relative, rgb, size,
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
/// The shortcuts no button carries; the rest ride on the buttons' tooltips.
/// Keys first: at a 640 px window the tail is what a truncation eats, and the
/// two hints at the end are also on the ruler's and Import's tooltips.
const KEY_HINTS: &str =
    "ctrl+c copy · ctrl+v paste · z undo · click the bar to seek · drop a file to import";

struct Player {
    session: PlaybackSession,
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
    /// Playback was started once, on the first render. Space owns it after that.
    launched: bool,
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
    /// Where ctrl+s writes: the project this timeline was loaded from, or the
    /// one derived beside the media it started as. Saving twice overwrites the
    /// same file rather than making a second one.
    project_path: PathBuf,
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
        let target = self.session.now() * self.fps;
        let mut newest: Option<Frame> = None;
        loop {
            let frame = match self.held.take() {
                Some(frame) => frame,
                // Nothing waiting means either a clip boundary being rebuilt or
                // the real end of the timeline, and only the engine can tell
                // them apart -- `frame.index` is already a timeline index.
                None => match self.session.try_frame() {
                    Some(frame) => frame,
                    None => {
                        self.eos = self.session.is_eos();
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
                self.session.now()
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
        self.session.seek(t);
        self.reset_after_reseek();
        cx.notify();
    }

    /// Splits the clip under the playhead. Metadata only: the timeline->source
    /// mapping is unchanged, so nothing reseeks and no flag is touched.
    fn cut(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.session.cut_at(self.session.now());
        self.selected = None;
        cx.notify();
    }

    /// Drops the selected clip. The engine reseeks itself, so all this owes is
    /// the flag reset.
    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if self
            .selected
            .take()
            .is_some_and(|i| self.session.delete_clip(i))
        {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Copies the selected clip. Nothing on screen changes, so no notify.
    fn copy_selected(&mut self) {
        if let Some(clip) = self.selected.and_then(|i| self.session.clip_at(i)) {
            self.clipboard = Some(clip);
        }
    }

    /// Drops the copied clip in at the playhead. The engine reseeks itself, so
    /// like a delete this owes the flag reset -- and the selection, whose index
    /// the insert has just moved.
    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(clip) = self.clipboard
            && self.session.paste_at(self.session.now(), clip)
        {
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
        let text = match self.session.import(path) {
            Ok(()) => {
                self.reset_after_reseek();
                format!("IMPORTED {}", file_name(path))
            }
            Err(e) => format!("IMPORT FAILED: {e}"),
        };
        eprintln!("{text}");
        self.notice = Some(text.into());
        cx.notify();
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
                self.session = session;
                self.fps = self.session.meta().frame_rate;
                // A project is named after itself but still exports beside its
                // media: that is the only place an export has ever landed.
                self.export_path = export_path(&self.session.sources()[0]);
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
        let text = match self.session.save_project(&self.project_path) {
            Ok(()) => format!("SAVED {}", file_name(&self.project_path)),
            Err(e) => format!("SAVE FAILED: {e}"),
        };
        eprintln!("{text}");
        self.notice = Some(text.into());
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if self.session.undo() {
            self.reset_after_reseek();
        }
        self.selected = None;
        cx.notify();
    }

    /// Seeks to where the pointer sits along the ruler. `commit` is the press
    /// and the release, which must land exactly even when the throttle below
    /// would have skipped them.
    fn scrub_to(&mut self, x: Pixels, commit: bool, cx: &mut Context<Self>) {
        let t = f64::from(frac_along(x, self.ruler.get())) * self.session.timeline_duration();
        let target = (t * self.fps) as u32;
        if commit || scrub_due(target, self.last_target, self.last_scrub.elapsed()) {
            self.last_target = target;
            self.last_scrub = Instant::now();
            self.seek(t, cx);
        }
    }

    /// Space and the transport button share it: once the timeline is finished
    /// the only sensible "play" is from the top.
    fn toggle_or_restart(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        if self.done {
            self.seek(0., cx);
            self.session.play();
        } else {
            self.session.toggle();
            // Past EOF nothing else asks for a repaint.
            cx.notify();
        }
    }

    /// The export that owns the UI, if any. A cancelled one does not: it has
    /// its own copy of the edit list and owes only its own cleanup.
    fn exporting(&self) -> Option<&ExportHandle> {
        self.export.as_ref().filter(|_| !self.cancelling)
    }

    /// Writes the edit list out beside the source. Playback stops first: the
    /// exporter opens its own decoder -- and, on the hardware path, an encoder --
    /// so a running player would only compete with it for the GPU. A cancelled
    /// export still winding down holds this off for the frame it takes to
    /// notice, which is what keeps its `remove_file` off the new output.
    fn start_export(&mut self, cx: &mut Context<Self>) {
        if self.export.is_some() {
            return;
        }
        self.session.pause();
        self.export = Some(self.session.export_to(&self.export_path));
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
        // Starts on launch: the session opens paused so the clock begins with
        // the first rendered frame rather than with the process (which would
        // charge window setup to the timeline and drop the frames it covers).
        if !std::mem::replace(&mut self.launched, true) {
            self.session.play();
        }
        self.session.tick();
        self.pump(window);
        self.poll_export();
        // No shadow flag: the clock is the only truth about play state.
        let playing = self.session.is_playing();
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
        let duration = self.session.timeline_duration();
        // The clock keeps running after the last frame (wall time takes over at
        // audio EOF) while the picture is frozen, so the timeline the UI shows is
        // the clamped one, pinned to the out-point once playback is done.
        let position = if self.done {
            duration
        } else {
            self.session.now().clamp(0., duration)
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
                // (an unbound key, or ctrl+c, changes nothing else).
                if this.notice.take().is_some() {
                    cx.notify();
                }
                // An export is reading the edit list every other key here would
                // change, so escape is the only one that means anything until
                // it is over.
                if this.exporting().is_some() {
                    if event.keystroke.key.as_str() == "escape" {
                        this.cancel_export();
                    }
                    cx.notify();
                    return;
                }
                // On linux gpui reports ctrl-c as key "c" with the control
                // modifier set (the control code is mapped back), so the plain
                // and modified bindings are told apart here and nowhere else.
                match (
                    event.keystroke.key.as_str(),
                    event.keystroke.modifiers.control,
                ) {
                    ("space", _) => this.toggle_or_restart(cx),
                    ("e", false) => this.start_export(cx),
                    ("s", true) => this.save_project(cx),
                    ("c", true) => this.copy_selected(),
                    ("v", true) => this.paste(cx),
                    ("c", false) => this.cut(cx),
                    ("x", _) | ("delete", _) => this.delete_selected(cx),
                    ("z", _) => this.undo(cx),
                    _ => {}
                }
            }))
            // The whole window is the drop target: gpui turns an external file
            // drop into an `ExternalPaths` drag (window.rs:3626) delivered as a
            // mouse-up to every hovered hitbox, and the root's is the only one
            // that covers the picture as well as the panel.
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
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
                            .map(|i| img(i).size_full().object_fit(gpui::ObjectFit::Contain)),
                    ),
            )
            // Above the panel and only when there is one to show, so it costs
            // the picture nothing the rest of the time.
            .children(self.notice_bar(cx))
            .child(self.panel(position, duration, playing, cx))
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
        let (hint, filled) = if let Some(export) = self.exporting() {
            let progress = export.progress();
            (
                format!("EXPORTING {}% — esc cancels", (progress * 100.) as u32),
                progress,
            )
        } else {
            (KEY_HINTS.to_string(), filled)
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
                        "space",
                        !exporting,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_or_restart(cx)),
                    ))
                    .child(separator())
                    .child(control(
                        "cut",
                        Some(cut_glyph().into_any_element()),
                        "Cut",
                        "c — splits the clip under the playhead",
                        !exporting,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.cut(cx)),
                    ))
                    .child(control(
                        "delete",
                        Some(delete_glyph().into_any_element()),
                        "Delete",
                        if self.selected.is_some() {
                            "x or delete"
                        } else {
                            "x or delete — click a clip below first"
                        },
                        !exporting && self.selected.is_some(),
                        cx.listener(|this, _: &ClickEvent, _, cx| this.delete_selected(cx)),
                    ))
                    .child(separator())
                    .child(control(
                        "import",
                        None,
                        "Import",
                        "opens a file chooser — or drop a file on the window",
                        !exporting,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.pick_and_import(cx)),
                    ))
                    .child(control(
                        "export",
                        None,
                        "Export",
                        "e — writes the timeline out beside the source",
                        self.export.is_none(),
                        cx.listener(|this, _: &ClickEvent, _, cx| this.start_export(cx)),
                    ))
                    .child(control(
                        "save",
                        None,
                        "Save",
                        "ctrl+s — writes the project file",
                        !exporting,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.save_project(cx)),
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

    /// The edit list made visible: one box per clip, sized by its share of the
    /// timeline. A cut adds a box without moving anything, a delete closes the
    /// gap. A box has no room for a label at four clips, so the tooltip is where
    /// it says what clicking it does. Never focusable, so the root keeps focus
    /// and space still works after a click (ledger:182).
    fn clips_lane(&self, duration: f64, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .h(px(LANE_H))
            .flex()
            .gap(px(1.))
            .overflow_hidden()
            .children(
                self.session
                    .clip_spans_by_source()
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

/// The desktop's own file choosers, best first. Asked for by name because gpui
/// 0.2 has no file dialog of its own and none of these is worth a dependency.
const PICKERS: [(&str, &[&str]); 2] = [
    ("zenity", &["--file-selection", "--title=edith — import"]),
    ("kdialog", &["--getopenfilename"]),
];

/// Runs the first chooser that is installed. `Ok(None)` is a cancelled dialog;
/// the error is for a machine with no chooser at all, which still has the drop
/// target -- the import path that never depended on another program.
fn pick_file() -> Result<Option<PathBuf>, &'static str> {
    for (bin, args) in PICKERS {
        // Not installed: try the next one. Anything else (a cancel, a refusal)
        // is that chooser's answer and is taken as final.
        let Ok(out) = std::process::Command::new(bin).args(args).output() else {
            continue;
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Ok((!path.is_empty()).then(|| PathBuf::from(path)));
    }
    Err(
        "NO FILE CHOOSER — install zenity or kdialog, or drag the file onto this window to import it",
    )
}

/// Where an export goes: the source path with `.export.mp4` for an extension,
/// so it lands beside the original and can never be the original.
fn export_path(source: impl Into<PathBuf>) -> PathBuf {
    let mut path = source.into();
    path.set_extension("export.mp4");
    path
}

/// Where ctrl+s goes when the timeline did not come from a project file: the
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
    shortcut: &'static str,
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
        CONTROL_H, HIT_MIN, LANE_H, RULER_HIT_H, SOURCE_TINTS, SURFACE, export_path, frac_along,
        is_project, project_path, scrub_due, source_tint, timecode, width_frac,
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
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: app <video.mp4|project.edith> [more.mp4 ...]");
        std::process::exit(2);
    };
    let arg = PathBuf::from(&path);
    // A `.edith` restores a whole timeline, anything else *is* the timeline.
    // Either way the rest of argv is appended to what comes out.
    let opened = if is_project(&arg) {
        PlaybackSession::open_project(&arg)
    } else {
        PlaybackSession::open(&arg)
    };
    let mut session = match opened {
        Ok(v) => v,
        Err(e) => {
            // A failed load by drop leaves the running session alone; here
            // there is no session to leave, so the refusal is the whole run.
            eprintln!("cannot open {path}: {e}");
            std::process::exit(1);
        }
    };
    // The first file makes the timeline, the rest are appended to it. A refusal
    // is not fatal -- the others still load -- but the window must not open
    // silently pretending the file was taken, so the first one seeds the notice.
    let mut notice = None;
    for arg in std::env::args().skip(2) {
        if let Err(e) = session.import(std::path::Path::new(&arg)) {
            let text = format!("IMPORT FAILED: {e}");
            eprintln!("{arg}: {text}");
            notice.get_or_insert(text);
        }
    }
    let meta = *session.meta();
    // Beside the media even for a project: an export has never landed anywhere
    // but next to the picture it came from.
    let out = export_path(&session.sources()[0]);
    let project = if is_project(&arg) {
        arg.clone()
    } else {
        project_path(&arg)
    };
    let name: SharedString = file_name(&arg).into();
    println!(
        "{path}: {}x{} @ {:.2} fps, {} samples",
        meta.width, meta.height, meta.frame_rate, meta.frame_count
    );

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(
            None,
            size(px(meta.width as f32), px(meta.height as f32)),
            cx,
        );
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
                    session,
                    fps: meta.frame_rate,
                    name: name.clone(),
                    image: None,
                    held: None,
                    eos: false,
                    done: false,
                    pending_seek: false,
                    ruler: Rc::default(),
                    selected: None,
                    clipboard: None,
                    scrubbing: false,
                    last_scrub: Instant::now(),
                    last_target: 0,
                    launched: false,
                    export: None,
                    cancelling: false,
                    export_path: out.clone(),
                    project_path: project.clone(),
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
