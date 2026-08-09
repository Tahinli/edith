use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::time::Instant;

use engine::{Frame, PlaybackSession};
use gpui::{
    App, Application, Bounds, Context, FocusHandle, KeyDownEvent, RenderImage, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, img, prelude::*, px, rgb, size,
};

struct Player {
    session: PlaybackSession,
    /// Timeline seconds -> frame index, so the clock can be compared to what
    /// the decoder hands over.
    fps: f64,
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
        if !self.done {
            window.request_animation_frame();
        }

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, _| {
                // `is_held` filters auto-repeat, which would otherwise toggle
                // playback many times a second.
                if event.keystroke.key == "space" && !event.is_held {
                    this.session.toggle();
                }
            }))
            .size_full()
            .bg(rgb(0x101010))
            .flex()
            .justify_center()
            .items_center()
            .children(
                self.image
                    .clone()
                    .map(|i| img(i).size_full().object_fit(gpui::ObjectFit::Contain)),
            )
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
