use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::{Clip, ExportHandle, Frame, PlaybackSession};
use gpui::{
    App, Application, Bounds, Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, RenderImage, SharedString, TitlebarOptions,
    Window, WindowBounds, WindowOptions, canvas, div, img, point, prelude::*, px, relative, rgb,
    size,
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

/// Fixed so the video region takes every pixel the window gains and the controls
/// never clip at 640x360.
const HEADER_H: f32 = 32.;
const PANEL_H: f32 = 180.;
const LANE_H: f32 = 48.;
/// Wide enough for `HH:MM:SS:FF / HH:MM:SS:FF`, and fixed so changing digits
/// cannot push the layout around.
const TIME_W: f32 = 200.;

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
    /// What the last export left in the timecode slot, and when. Cleared by
    /// `NOTICE` passing or by the next key, whichever comes first.
    notice: Option<(SharedString, Instant)>,
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
        self.notice = Some((text.into(), Instant::now()));
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
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= NOTICE)
        {
            self.notice = None;
        }
        // No shadow flag: the clock is the only truth about play state.
        let playing = self.session.is_playing();
        // A paused timeline has nothing to animate; the toggle handlers notify,
        // which is what starts the loop again. A paused seek keeps the loop
        // running by itself until `pump` has the frame it asked for. An export
        // pauses playback and still needs the loop: its progress, and the
        // message it leaves behind, only reach the screen on a repaint.
        if (playing && !self.done)
            || self.pending_seek
            || self.export.is_some()
            || self.notice.is_some()
        {
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
                // Any key retires the last export's message, whatever it was.
                this.notice = None;
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
                    ("c", true) => this.copy_selected(),
                    ("v", true) => this.paste(cx),
                    ("c", false) => this.cut(cx),
                    ("x", _) | ("delete", _) => this.delete_selected(cx),
                    ("z", _) => this.undo(cx),
                    _ => {}
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
            .child(self.panel(position, duration, playing, cx))
    }
}

impl Player {
    /// Transport, timecode, playhead, clips lane.
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
        // An export owns the timecode slot and the ruler while it runs: the
        // percentage and the accent bar are the same number, so the playhead
        // fill doubles as the progress bar for free.
        let (label, filled) = if let Some(export) = self.exporting() {
            let progress = export.progress();
            (
                format!("EXPORT {}% — esc cancels", (progress * 100.) as u32),
                progress,
            )
        } else if let Some((notice, _)) = &self.notice {
            (notice.to_string(), filled)
        } else {
            (
                format!(
                    "{} / {}",
                    timecode(position, self.fps),
                    timecode(duration, self.fps)
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(control(
                        transport_glyph(playing),
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.toggle_or_restart(cx);
                        }),
                    ))
                    .child(control(
                        cut_glyph(),
                        cx.listener(|this, _: &MouseDownEvent, _, cx| this.cut(cx)),
                    ))
                    .child(control(
                        delete_glyph(),
                        cx.listener(|this, _: &MouseDownEvent, _, cx| this.delete_selected(cx)),
                    ))
                    // Fixed width and one line whatever it says: an export
                    // label is longer than a timecode and must not push the
                    // row around, nor wrap and change its height.
                    //
                    // ponytail: a long file name is ellipsized at TIME_W --
                    // the whole path is on stderr. Upgrade path is a status
                    // line of its own under the ruler, not a wider slot.
                    .child(div().flex_none().w(px(TIME_W)).truncate().child(label)),
            )
            // Press to seek, drag to scrub: the move and release halves live on
            // the root, since the pointer leaves this 6 px strip immediately.
            .child(
                div()
                    .flex_none()
                    .h(px(6.))
                    .rounded(px(3.))
                    .bg(rgb(SURFACE))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.scrubbing = true;
                            this.scrub_to(event.position.x, true, cx);
                        }),
                    )
                    .child(bounds_probe(self.ruler.clone()))
                    .child(
                        div()
                            .h_full()
                            .w(relative(filled))
                            .rounded(px(3.))
                            .bg(rgb(ACCENT)),
                    ),
            )
            .child(self.clips_lane(duration, cx))
            .child(track_lane())
    }

    /// The edit list made visible: one box per clip, sized by its share of the
    /// timeline. A cut adds a box without moving anything, a delete closes the
    /// gap. Plain divs, so the root keeps focus and space still works after a
    /// click (ledger:182).
    fn clips_lane(&self, duration: f64, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .h(px(LANE_H))
            .flex()
            .gap(px(1.))
            .overflow_hidden()
            .children(
                self.session
                    .clip_spans()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (_, len))| {
                        let selected = self.selected == Some(i);
                        div()
                            .h_full()
                            .w(relative(width_frac(len, duration)))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(rgb(if selected { ACCENT } else { SURFACE }))
                            .bg(rgb(if selected { SELECTED } else { SURFACE }))
                            .cursor_pointer()
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

/// How long a finished export's message holds the timecode slot.
const NOTICE: Duration = Duration::from_secs(2);

/// Where an export goes: the source path with `.export.mp4` for an extension,
/// so it lands beside the original and can never be the original.
fn export_path(source: &str) -> PathBuf {
    let mut path = PathBuf::from(source);
    path.set_extension("export.mp4");
    path
}

/// The tail of a path, for showing. A path that is all root has none, and reads
/// as itself.
fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
}

/// A clip's share of the lane. A timeline with no length reads as one full-width
/// box rather than as NaN, which gpui would carry into layout.
fn width_frac(len: f64, total: f64) -> f32 {
    if total > 0. { (len / total) as f32 } else { 1. }
}

/// The 32x28 control shape shared by transport, cut and delete. A plain div:
/// nothing here tracks focus, so the root's own key listener keeps working
/// after a press.
fn control(
    glyph: impl IntoElement,
    on_press: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .w(px(32.))
        .h(px(28.))
        .flex()
        .justify_center()
        .items_center()
        .rounded(px(3.))
        .bg(rgb(SURFACE))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_press)
        .child(glyph)
}

/// A clip split in two -- the gap is the cut. Drawn, like every glyph here.
fn cut_glyph() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(3.))
        .child(div().w(px(5.)).h(px(13.)).bg(rgb(INK)))
        .child(div().w(px(5.)).h(px(13.)).bg(rgb(INK)))
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
    use super::{export_path, frac_along, scrub_due, timecode, width_frac};
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
    fn clip_boxes_split_the_lane_by_duration() {
        // 1 s + 3 s of a 4 s timeline: a quarter and three quarters.
        assert_eq!(width_frac(1., 4.), 0.25);
        assert_eq!(width_frac(3., 4.), 0.75);
        assert_eq!(width_frac(4., 4.), 1.);
        // A timeline with no length must not hand gpui a NaN width.
        assert_eq!(width_frac(0., 0.), 1.);
    }
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: app <video.mp4>");
        std::process::exit(2);
    };
    let session = match PlaybackSession::open(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cannot open {path}: {e}");
            std::process::exit(1);
        }
    };
    let meta = *session.meta();
    let out = export_path(&path);
    let name: SharedString = std::path::Path::new(&path)
        .file_name()
        .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned())
        .into();
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
                    title: Some("video_editor".into()),
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
                    notice: None,
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
