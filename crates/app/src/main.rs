use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::time::Instant;

use engine::{Frame, PlaybackSession};
use gpui::{
    App, Application, Bounds, Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent,
    PathBuilder, RenderImage, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions,
    canvas, div, img, point, prelude::*, px, relative, rgb, size,
};

/// Editor chrome: three grays and one accent, all darker than the picture so the
/// frame is what the eye lands on. `LETTERBOX` stays the video's own bed.
const LETTERBOX: u32 = 0x101010;
const CHROME: u32 = 0x242424;
const SURFACE: u32 = 0x333333;
const INK: u32 = 0xc8c8c8;
const ACCENT: u32 = 0x4a9eff;

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
    /// `frame_count / frame_rate`: the timeline's end, and the clamp for a clock
    /// that keeps running past it.
    duration: f64,
    name: SharedString,
    image: Option<Arc<RenderImage>>,
    /// A frame that arrived before its time; shown on the tick it comes due.
    held: Option<Frame>,
    /// The decoder's channel closed. Frames may still be waiting in `held`.
    eos: bool,
    /// Last frame shown; nothing left to animate.
    done: bool,
    /// Playback was started once, on the first render. Space owns it after that.
    launched: bool,
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
                None => match self.session.frames().try_recv() {
                    Ok(frame) => frame,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.eos = true;
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
            let elapsed = self.started.map_or(0.0, |t| t.elapsed().as_secs_f64());
            eprintln!(
                "eof after {elapsed:.3}s wall: {} frames displayed, {} dropped, clock {:.3}s",
                self.displayed,
                self.dropped,
                self.session.now()
            );
        }
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
        // No shadow flag: the clock is the only truth about play state.
        let playing = self.session.is_playing();
        // A paused timeline has nothing to animate; the toggle handlers notify,
        // which is what starts the loop again.
        if playing && !self.done {
            window.request_animation_frame();
        }

        // The clock keeps running after the last frame (wall time takes over at
        // audio EOF) while the picture is frozen, so the timeline the UI shows is
        // the clamped one, pinned to the out-point once playback is done.
        let position = if self.done {
            self.duration
        } else {
            self.session.now().clamp(0., self.duration)
        };

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                // `is_held` filters auto-repeat, which would otherwise toggle
                // playback many times a second.
                if event.keystroke.key == "space" && !event.is_held {
                    this.session.toggle();
                    // Past EOF nothing else asks for a repaint.
                    cx.notify();
                }
            }))
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
            .child(self.panel(position, playing, cx))
    }
}

impl Player {
    /// Transport, timecode, playhead and the (still empty) track lanes.
    fn panel(&self, position: f64, playing: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let filled = if self.duration > 0. {
            (position / self.duration) as f32
        } else {
            0.
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
                    .child(
                        // A plain div: nothing here tracks focus, so the root's
                        // own mouse-down handler is what wins the click and the
                        // key listener keeps working after a press.
                        div()
                            .w(px(32.))
                            .h(px(28.))
                            .flex()
                            .justify_center()
                            .items_center()
                            .rounded(px(3.))
                            .bg(rgb(SURFACE))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                    this.session.toggle();
                                    cx.notify();
                                }),
                            )
                            .child(transport_glyph(playing)),
                    )
                    .child(
                        div().flex_none().w(px(TIME_W)).child(format!(
                            "{} / {}",
                            timecode(position, self.fps),
                            timecode(self.duration, self.fps)
                        )),
                    ),
            )
            // Read-only: no cursor, no hover, no listener -- a clip slice turns
            // this into a scrubber, today it only reports.
            .child(
                div()
                    .flex_none()
                    .h(px(6.))
                    .rounded(px(3.))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .h_full()
                            .w(relative(filled))
                            .rounded(px(3.))
                            .bg(rgb(ACCENT)),
                    ),
            )
            .child(track_lane())
            .child(track_lane())
    }
}

/// Placeholder chrome for the clips slice: deliberately empty.
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
    use super::timecode;

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
                    duration: f64::from(meta.frame_count) / meta.frame_rate,
                    name: name.clone(),
                    image: None,
                    held: None,
                    eos: false,
                    done: false,
                    launched: false,
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
