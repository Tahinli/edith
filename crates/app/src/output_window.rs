//! The video output window (View > Video output window): a window of its own
//! that shows the program picture and nothing else -- full-bleed on black,
//! letterboxed -- for a client monitor or a second screen, the way an NLE
//! feeds its full-screen video output.
//!
//! No second decode. The picture is the very frame the viewer holds in
//! [`Player::image`], which this window reads at each paint. The main window
//! repaints this one whenever it renders ([`Player::render`]), so the output
//! follows the playhead and a scrub. The window is session state only
//! ([`Player::output`]): it is not in the project file and is not reopened.

use crate::*;
use gpui::Entity;

/// The picture's surround: true black whatever the UI theme, since a client display is not chrome and should not go pale under a light theme.
const VIDEO_BLACK: u32 = 0x000000;

/// The output window's root view: the picture, read from the player on each paint.
pub(crate) struct OutputView {
    player: Entity<Player>,
    focus: FocusHandle,
}

impl OutputView {
    /// Closes the window and clears the player's record of it. Esc and a double-click both end here.
    fn close(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| {
            player.output = None;
            cx.notify();
        });
        window.remove_window();
    }
}

impl Render for OutputView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let image = self.player.read(cx).image.clone();
        div()
            .id("video-output")
            .relative()
            .flex()
            .size_full()
            .bg(rgb(VIDEO_BLACK))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                }
            }))
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                if event.click_count() == 2 {
                    this.close(window, cx);
                }
            }))
            .when_some(image, |d, image| d.child(letterboxed_image(image)))
    }
}

/// Opens the output window on `display` (the primary one when `None`), fullscreen there,
/// and hands the keyboard to it so Esc works at once. `None` when the window would not open.
pub(crate) fn open(player: Entity<Player>, display: Option<DisplayId>, cx: &mut App) -> Option<WindowHandle<OutputView>> {
    // Always a named display: an unnamed window lands wherever the system puts it, not on the
    // screen the menu ticked.
    let screen = display.and_then(|id| cx.find_display(id)).or_else(|| cx.displays().into_iter().next());
    let bounds = match &screen {
        Some(screen) => screen.bounds(),
        None => Bounds::centered(None, size(px(1280.), px(720.)), cx),
    };
    let display = screen.map(|screen| screen.id());
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Fullscreen(bounds)),
                display_id: display,
                titlebar: None,
                app_id: Some("edith".to_string()),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| OutputView { player, focus: cx.focus_handle() });
                let focus = view.read(cx).focus.clone();
                window.focus(&focus);
                view
            },
        )
        .ok()?;
    Some(handle)
}

impl Player {
    /// The display the output window goes to: the one picked, else the first the system lists.
    pub(crate) fn output_target(&self) -> Option<DisplayId> {
        self.output_display.or(self.output_displays.first().map(|(id, _)| *id))
    }

    /// Each frame, before the picture is painted: the display list the View menu reads, the
    /// output window's record (cleared if the window was closed by the system), and a repaint
    /// of that window, which shows the same picture as this one.
    pub(crate) fn sync_output(&mut self, cx: &mut Context<Self>) {
        // Numbered left to right, top to bottom: the system lists screens in no fixed order.
        let mut screens = cx.displays();
        screens.sort_by_key(|screen| {
            let origin = screen.bounds().origin;
            (f32::from(origin.x) as i64, f32::from(origin.y) as i64)
        });
        self.output_displays = screens
            .iter()
            .enumerate()
            .map(|(i, screen)| {
                let bounds = screen.bounds();
                let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                (screen.id(), format!("Display {} ({} × {})", i + 1, w, h).into())
            })
            .collect();
        if let Some(open) = self.output {
            match open.update(cx, |_, _, cx| cx.notify()) {
                Ok(()) => {}
                Err(_) => self.output = None,
            }
        }
    }

    /// View > Video output window: opens the window on the target display, or closes it.
    pub(crate) fn toggle_output_window(&mut self, cx: &mut Context<Self>) {
        if self.output.is_some() {
            self.close_output(cx);
        } else {
            self.open_output(cx);
        }
    }

    /// View > Output display: the window moves to the display picked, if it is open now.
    pub(crate) fn set_output_display(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((id, _)) = self.output_displays.get(index).cloned() else {
            return;
        };
        self.output_display = Some(id);
        if self.output.is_some() {
            self.close_output(cx);
            self.open_output(cx);
        }
    }

    /// Opens the window once the current update has finished: the window paints as it opens,
    /// and that paint reads this player, which the update is still holding.
    fn open_output(&mut self, cx: &mut Context<Self>) {
        let player = cx.entity();
        let display = self.output_display;
        cx.defer(move |cx| {
            let handle = output_window::open(player.clone(), display, cx);
            player.update(cx, |player, cx| {
                player.output = handle;
                cx.notify();
            });
        });
    }

    fn close_output(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self.output.take() {
            let _ = open.update(cx, |_, window, _| window.remove_window());
        }
    }
}
