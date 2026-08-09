use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use engine::{DecodeSession, Frame};
use gpui::{
    App, Application, Bounds, Context, RenderImage, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, img, prelude::*, px, rgb, size,
};

struct Player {
    rx: Receiver<Frame>,
    image: Option<Arc<RenderImage>>,
    finished: bool,
}

impl Player {
    /// Takes at most one frame per rendered frame; the decoder's channel is
    /// bounded so it simply waits for us.
    fn pump(&mut self, window: &mut Window) {
        match self.rx.try_recv() {
            Ok(frame) => {
                let buf =
                    image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
                        .expect("frame buffer sized width*height*4");
                let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
                if let Some(old) = self.image.replace(next) {
                    // Every RenderImage gets a fresh id and its own atlas tile:
                    // without this the sprite atlas grows for the whole video.
                    let _ = window.drop_image(old);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.finished = true,
        }
    }
}

impl Render for Player {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.pump(window);
        if !self.finished {
            window.request_animation_frame();
        }

        div()
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
    let (meta, rx) = match DecodeSession::open(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cannot open {path}: {e}");
            std::process::exit(1);
        }
    };
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
            |_, cx| {
                cx.new(|_| Player {
                    rx,
                    image: None,
                    finished: false,
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
