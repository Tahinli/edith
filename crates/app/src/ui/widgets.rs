//! The small elements every region is built out of.

use crate::*;

/// One clip's audio, drawn as a filled min/max envelope inside whatever box it
/// is given. Peaks are the source's whole envelope; `from`/`to` are the source
/// seconds this clip plays, so a cut clip shows its own stretch of the file.
pub(crate) fn waveform(peaks: Arc<Vec<(f32, f32)>>, from: f64, to: f64) -> impl IntoElement {
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
                window.paint_path(path, rgb(FG_SECONDARY));
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
pub(crate) fn control(
    id: &'static str,
    // The rect the label is allowed to change inside: 0 hugs the text, anything
    // else is reserved once and never moves again, which is what keeps a button
    // that relabels itself ("Export"/"Cancel") from shoving its neighbours
    // along the row every time its state changes.
    w: f32,
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
        .justify_center()
        .gap(px(6.))
        .px(px(8.))
        .when(w > 0., |d| d.w(px(w)).overflow_hidden())
        .rounded(px(4.))
        .bg(rgb(BG_RAISED))
        .children(glyph)
        .child(label)
        .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
        .when(!enabled, |d| d.opacity(0.4).cursor_not_allowed())
        .when(enabled, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER)))
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
pub(crate) fn volume_slider(
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
                .bg(rgb(BG_RAISED))
                .child(
                    div()
                        .h_full()
                        .w(relative(volume.along()))
                        .rounded(px(2.))
                        .bg(rgb(if volume.muted { FG_SECONDARY } else { ACCENT_PRIMARY })),
                ),
        )
}

/// The line between two groups of buttons.
pub(crate) fn separator() -> impl IntoElement {
    div()
        .flex_none()
        .mx(px(4.))
        .w(px(1.))
        .h(px(18.))
        .bg(rgb(BG_HOVER))
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
pub(crate) static OVERLAID: AtomicBool = AtomicBool::new(false);

/// A tooltip is a view in gpui and nothing smaller, so this is the smallest one
/// that carries a line of text. It paints outside the window's element tree and
/// therefore owns its colours.
pub(crate) struct Tip(pub(crate) SharedString);

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
            .border_color(rgb(BG_RAISED))
            .bg(rgb(BG_PANEL))
            .text_color(rgb(FG_PRIMARY))
            .text_size(px(12.))
            .child(self.0.clone())
    }
}

/// Scissors: two blades crossed. Drawn this way and not as a split clip because
/// two bars is what the transport wears when it is playing -- the one glyph a
/// cut must never be mistaken for.
pub(crate) fn cut_glyph() -> impl IntoElement {
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
                window.paint_path(path, rgb(FG_PRIMARY));
            }
        },
    )
    .w(px(13.))
    .h(px(13.))
}

/// A lid over a bin.
pub(crate) fn delete_glyph() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .child(div().w(px(13.)).h(px(2.)).bg(rgb(FG_PRIMARY)))
        .child(div().w(px(9.)).h(px(9.)).bg(rgb(FG_PRIMARY)))
}

/// What a window with no file open is waiting for. Both ways in are already
/// built -- the whole window is the drop target and the Import chooser takes a
/// project as readily as media -- so this only has to say so.
pub(crate) fn empty_hint() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .text_color(rgb(FG_SECONDARY))
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
pub(crate) fn transport_glyph(state: Transport) -> impl IntoElement {
    let playing = state.is_playing();
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .when(playing, |d| {
            d.child(div().w(px(3.)).h(px(12.)).bg(rgb(FG_PRIMARY)))
                .child(div().w(px(3.)).h(px(12.)).bg(rgb(FG_PRIMARY)))
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
                            window.paint_path(path, rgb(FG_PRIMARY));
                        }
                    },
                )
                .w(px(11.))
                .h(px(13.)),
            )
        })
}
