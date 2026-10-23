//! How the viewer looks at the picture (REDESIGN §2.1): the zoom, the playback
//! quality, the guides and the aspect mask. All of it is view state -- it
//! changes what is drawn on this screen, never the project and never an
//! export -- so it is kept in the project's view file ([`crate::view_state`])
//! and has no MCP operation.

/// The viewer's zoom: `Fit` letterboxes the whole frame, the rest show the
/// frame at a fraction of the project's own pixels, centred and pannable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Zoom {
    #[default]
    Fit,
    P25,
    P50,
    P100,
    P200,
}

impl Zoom {
    pub(crate) const ALL: [Zoom; 5] = [Zoom::Fit, Zoom::P25, Zoom::P50, Zoom::P100, Zoom::P200];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Zoom::Fit => "Fit",
            Zoom::P25 => "25%",
            Zoom::P50 => "50%",
            Zoom::P100 => "100%",
            Zoom::P200 => "200%",
        }
    }

    /// The project pixels per device pixel; `None` for `Fit`.
    pub(crate) fn factor(self) -> Option<f32> {
        match self {
            Zoom::Fit => None,
            Zoom::P25 => Some(0.25),
            Zoom::P50 => Some(0.5),
            Zoom::P100 => Some(1.),
            Zoom::P200 => Some(2.),
        }
    }

    /// The stop one wheel notch away; `Fit` is the bottom of the ladder's far
    /// end, so a zoom-out never leaves the list.
    pub(crate) fn step(self, closer: bool) -> Zoom {
        let at = Zoom::ALL.iter().position(|z| *z == self).unwrap_or(0);
        // Fit sits first in the list but is the "smallest" stop to a wheel:
        // zooming out from 25% lands on it.
        let order = [Zoom::Fit, Zoom::P25, Zoom::P50, Zoom::P100, Zoom::P200];
        let next = match closer {
            true => (at + 1).min(order.len() - 1),
            false => at.saturating_sub(1),
        };
        order[next]
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Zoom::Fit => "fit",
            Zoom::P25 => "25",
            Zoom::P50 => "50",
            Zoom::P100 => "100",
            Zoom::P200 => "200",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Zoom> {
        Zoom::ALL.into_iter().find(|z| z.key() == key)
    }
}

/// The resolution the picture is decoded and scaled at *while playing*. A
/// paused frame is always full quality, and an export never reads this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Quality {
    #[default]
    Full,
    Half,
    Quarter,
}

impl Quality {
    pub(crate) const ALL: [Quality; 3] = [Quality::Full, Quality::Half, Quality::Quarter];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Quality::Full => "Full",
            Quality::Half => "Half",
            Quality::Quarter => "Quarter",
        }
    }

    /// The label on the closed header dropdown: "Full" alone read as a zoom next to "Fit".
    pub(crate) fn closed_label(self) -> &'static str {
        match self {
            Quality::Full => "Full quality",
            Quality::Half => "Half quality",
            Quality::Quarter => "Quarter quality",
        }
    }

    pub(crate) fn detail(self) -> &'static str {
        match self {
            Quality::Full => "every pixel",
            Quality::Half => "half width and height while playing",
            Quality::Quarter => "quarter width and height while playing",
        }
    }

    /// The share of each side kept while playing.
    pub(crate) fn factor(self) -> f32 {
        match self {
            Quality::Full => 1.,
            Quality::Half => 0.5,
            Quality::Quarter => 0.25,
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Quality::Full => "full",
            Quality::Half => "half",
            Quality::Quarter => "quarter",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Quality> {
        Quality::ALL.into_iter().find(|q| q.key() == key)
    }
}

/// The guide lines drawn over the picture to judge a composition by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Guide {
    #[default]
    Off,
    /// The action-safe and title-safe rectangles, named in a small label.
    Safe,
    /// The rule of thirds.
    Thirds,
    /// A cross at the centre of the frame.
    Centre,
}

impl Guide {
    pub(crate) const ALL: [Guide; 4] = [Guide::Off, Guide::Safe, Guide::Thirds, Guide::Centre];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Guide::Off => "Off",
            Guide::Safe => "Safe areas",
            Guide::Thirds => "Thirds",
            Guide::Centre => "Centre cross",
        }
    }

    pub(crate) fn detail(self) -> &'static str {
        match self {
            Guide::Off => "",
            Guide::Safe => "action and title",
            Guide::Thirds => "rule of thirds",
            Guide::Centre => "",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Guide::Off => "off",
            Guide::Safe => "safe",
            Guide::Thirds => "thirds",
            Guide::Centre => "centre",
        }
    }

    /// A key as saved; the old on/off flag (`1`, `0`) still reads.
    pub(crate) fn from_key(key: &str) -> Option<Guide> {
        match key {
            "1" => Some(Guide::Safe),
            "0" => Some(Guide::Off),
            _ => Guide::ALL.into_iter().find(|g| g.key() == key),
        }
    }
}

/// A crop shape to judge a vertical or square delivery against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Mask {
    #[default]
    None,
    Vertical,
    Square,
    Portrait,
}

impl Mask {
    pub(crate) const ALL: [Mask; 4] = [Mask::None, Mask::Vertical, Mask::Square, Mask::Portrait];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Mask::None => "None",
            Mask::Vertical => "9:16",
            Mask::Square => "1:1",
            Mask::Portrait => "4:5",
        }
    }

    pub(crate) fn detail(self) -> &'static str {
        match self {
            Mask::None => "",
            Mask::Vertical => "Reels, Shorts, TikTok",
            Mask::Square => "square feed",
            Mask::Portrait => "portrait feed",
        }
    }

    /// Width over height of the kept window.
    pub(crate) fn ratio(self) -> Option<f32> {
        match self {
            Mask::None => None,
            Mask::Vertical => Some(9. / 16.),
            Mask::Square => Some(1.),
            Mask::Portrait => Some(4. / 5.),
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Mask::None => "none",
            Mask::Vertical => "9:16",
            Mask::Square => "1:1",
            Mask::Portrait => "4:5",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Mask> {
        Mask::ALL.into_iter().find(|m| m.key() == key)
    }
}

use crate::*;

/// Everything about the viewer's look that a project remembers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct Prefs {
    pub(crate) zoom: Zoom,
    pub(crate) quality: Quality,
    pub(crate) guide: Guide,
    pub(crate) mask: Mask,
}

/// An axis-aligned rectangle `(x, y, w, h)` in logical pixels.
pub(crate) type Rect = (f32, f32, f32, f32);

/// The size the picture is drawn at inside a box `(bw, bh)`: the whole frame
/// fitted for `Fit`, otherwise the project's `native` pixels scaled by the
/// zoom (device pixels over `dpr`).
pub(crate) fn shown_size(zoom: Zoom, native: (f32, f32), bx: (f32, f32), dpr: f32) -> (f32, f32) {
    match zoom.factor() {
        None => {
            let s = (bx.0 / native.0).min(bx.1 / native.1);
            (native.0 * s, native.1 * s)
        }
        Some(z) => (native.0 * z / dpr.max(0.1), native.1 * z / dpr.max(0.1)),
    }
}

/// The pan offset (from centred) clamped so the picture never leaves its box:
/// zero on an axis the picture fits on.
pub(crate) fn clamp_pan(pan: (f32, f32), shown: (f32, f32), bx: (f32, f32)) -> (f32, f32) {
    let lim = |s: f32, b: f32| ((s - b) / 2.).max(0.);
    let (lx, ly) = (lim(shown.0, bx.0), lim(shown.1, bx.1));
    (pan.0.clamp(-lx, lx), pan.1.clamp(-ly, ly))
}

/// Where the picture lands inside a box of size `bx` whose top-left is `origin`:
/// centred, offset by the (clamped) `pan`, whole pixels. The one rectangle the
/// picture, its guides and the titles and captions over it are placed against.
pub(crate) fn picture_rect(zoom: Zoom, native: (f32, f32), bx: (f32, f32), origin: (f32, f32), pan: (f32, f32), dpr: f32) -> Rect {
    let shown = shown_size(zoom, native, bx, dpr);
    let pan = clamp_pan(pan, shown, bx);
    (
        (origin.0 + (bx.0 - shown.0) / 2. + pan.0).round(),
        (origin.1 + (bx.1 - shown.1) / 2. + pan.1).round(),
        shown.0.round(),
        shown.1.round(),
    )
}

/// A rectangle of `share` of the frame on each side, centred.
pub(crate) fn inset(frame: Rect, share: f32) -> Rect {
    let (x, y, w, h) = frame;
    (x + w * (1. - share) / 2., y + h * (1. - share) / 2., w * share, h * share)
}

/// The window of ratio `ratio` (w/h) kept inside `frame`, centred.
pub(crate) fn mask_window(frame: Rect, ratio: f32) -> Rect {
    let (x, y, w, h) = frame;
    if w / h > ratio {
        let kw = h * ratio;
        (x + (w - kw) / 2., y, kw, h)
    } else {
        let kh = w / ratio;
        (x, y + (h - kh) / 2., w, kh)
    }
}

/// The pieces of `frame` outside `window`: up to four rectangles (top, bottom,
/// left, right) that a mask dims, empty ones left out.
pub(crate) fn outside(frame: Rect, window: Rect) -> Vec<Rect> {
    let (fx, fy, fw, fh) = frame;
    let (wx, wy, ww, wh) = window;
    [
        (fx, fy, fw, wy - fy),
        (fx, wy + wh, fw, fy + fh - (wy + wh)),
        (fx, wy, wx - fx, wh),
        (wx + ww, wy, fx + fw - (wx + ww), wh),
    ]
    .into_iter()
    .filter(|r| r.2 > 0.5 && r.3 > 0.5)
    .collect()
}

impl Player {
    /// Where a header control's list hangs: under the control, or under the header's left end
    /// before the first paint measured anything.
    pub(crate) fn viewer_anchor(&self, i: usize) -> Point<Pixels> {
        let b = self.viewer_btns[i].get();
        match b.size.width > px(0.) {
            true => point(b.origin.x, b.origin.y + b.size.height + px(2.)),
            false => point(px(12.), px(60.)),
        }
    }

    /// A viewer setting changed: keep it with the project's view and repaint.
    pub(crate) fn viewer_changed(&mut self, cx: &mut Context<Self>) {
        self.save_view_state();
        cx.notify();
    }

    pub(crate) fn set_viewer_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        if zoom != self.viewer.zoom {
            self.viewer_pan = (0., 0.);
        }
        self.viewer.zoom = zoom;
        self.viewer_changed(cx);
    }

    /// The picture's native pixels, or `None` with nothing loaded.
    fn viewer_native(&self) -> Option<(f32, f32)> {
        let meta = self.active_session()?.meta();
        Some((meta.width as f32, meta.height as f32))
    }

    /// The native size, or the image's own when no session is up to ask.
    pub(crate) fn viewer_native_or(&self, image: &RenderImage) -> (f32, f32) {
        self.viewer_native().unwrap_or_else(|| {
            let s = image.size(0);
            (s.width.0 as f32, s.height.0 as f32)
        })
    }

    /// The project frame's outline over an empty picture: a 1 px line at the project's aspect,
    /// fitted into the region, so the canvas edge reads on a near-black picture. `None` while a
    /// picture is shown (its own edge is the outline), with no project, or before the first paint.
    pub(crate) fn empty_canvas_outline(&self) -> Option<AnyElement> {
        if self.image.is_some() {
            return None;
        }
        let (w, h) = self.session.as_ref()?.resolution();
        let b = self.picture_box.get();
        let (bw, bh) = (f32::from(b.size.width), f32::from(b.size.height));
        if bw <= 1. || bh <= 1. || w == 0 || h == 0 {
            return None;
        }
        let k = (bw / w as f32).min(bh / h as f32);
        let (fw, fh) = (w as f32 * k, h as f32 * k);
        let (x, y) = ((bw - fw) / 2., (bh - fh) / 2.);
        Some(
            div()
                .absolute()
                .left(px(x.round()))
                .top(px(y.round()))
                .w(px(fw.round()))
                .h(px(fh.round()))
                .border_1()
                .border_color(rgba(crate::ui::theme::tokens().line))
                .into_any_element(),
        )
    }

    /// The picture's rectangle in the picture region's own coordinates, as it is
    /// drawn at the current zoom and pan; `None` with nothing loaded or measured.
    pub(crate) fn viewer_frame_local(&self, dpr: f32) -> Option<Rect> {
        let native = self.viewer_native()?;
        let b = self.picture_box.get();
        let bx = (f32::from(b.size.width), f32::from(b.size.height));
        (bx.0 > 1. && bx.1 > 1.).then(|| picture_rect(self.viewer.zoom, native, bx, (0., 0.), self.viewer_pan, dpr))
    }

    /// Drags the zoomed picture by `(dx, dy)`, kept inside its box.
    fn viewer_pan_by(&mut self, d: (f32, f32), dpr: f32, cx: &mut Context<Self>) {
        let (Some(native), b) = (self.viewer_native(), self.picture_box.get()) else {
            return;
        };
        let bx = (f32::from(b.size.width), f32::from(b.size.height));
        let shown = shown_size(self.viewer.zoom, native, bx, dpr);
        self.viewer_pan = clamp_pan((self.viewer_pan.0 + d.0, self.viewer_pan.1 + d.1), shown, bx);
        cx.notify();
    }

    /// Wheel over the picture: Ctrl steps the zoom, a bare wheel pans a zoomed picture.
    pub(crate) fn viewer_wheel(&mut self, event: &ScrollWheelEvent, window: &Window, cx: &mut Context<Self>) {
        if event.modifiers.control {
            let d = crate::timeline_math::wheel_delta(event);
            if d != 0. {
                self.set_viewer_zoom(self.viewer.zoom.step(d > 0.), cx);
            }
        } else if self.viewer.zoom != Zoom::Fit {
            let d = event.delta.pixel_delta(px(20.));
            self.viewer_pan_by((f32::from(d.x), f32::from(d.y)), window.scale_factor(), cx);
        }
    }

    pub(crate) fn viewer_drag_start(&mut self, at: Point<Pixels>) {
        if self.viewer.zoom != Zoom::Fit {
            self.viewer_drag = Some(at);
        }
    }

    /// The button came up: the pan (or the handle drag) is over.
    pub(crate) fn viewer_drag_end(&mut self, cx: &mut Context<Self>) {
        self.viewer_drag = None;
        self.viewer_handle_end(cx);
    }

    pub(crate) fn viewer_drag_move(&mut self, at: Point<Pixels>, shift: bool, window: &Window, cx: &mut Context<Self>) {
        if self.viewer_handle.is_some() {
            self.viewer_handle_move(at, shift, cx);
        } else if let Some(from) = self.viewer_drag {
            self.viewer_drag = Some(at);
            self.viewer_pan_by((f32::from(at.x - from.x), f32::from(at.y - from.y)), window.scale_factor(), cx);
        }
    }

    /// The size, in device pixels, the session is asked to paint at inside `region`: the
    /// zoom's own size (or the fitted one), reduced by the playback quality while it plays.
    pub(crate) fn viewer_target(&self, fitted: (u32, u32), native: (u32, u32), playing: bool) -> (u32, u32) {
        let (w, h) = match self.viewer.zoom.factor() {
            Some(z) => ((native.0 as f32 * z).round() as u32, (native.1 as f32 * z).round() as u32),
            None => fitted,
        };
        let q = if playing { self.viewer.quality.factor() } else { 1. };
        (((w as f32) * q).round() as u32, ((h as f32) * q).round() as u32)
    }

    /// Whether the picture is being drawn below full resolution right now, for the pill.
    pub(crate) fn viewer_reduced(&self) -> bool {
        self.viewer.quality != Quality::Full && self.active_session().is_some_and(|s| s.is_playing())
    }
}

/// How close, in screen px, a moved picture's centre or edge must come to the frame's centre or
/// edge to snap onto it.
pub(crate) const SNAP_PX: f32 = 6.;

/// The smallest distance, in screen px, from a corner grab to the picture's centre that still
/// scales it: nearer than this the ratio means nothing, so the grab does nothing.
const CORNER_MIN_PX: f32 = 4.;

/// What a viewer drag on the selected clip's picture takes: the whole picture, or the scale from
/// a corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandleGrab {
    Move,
    Corner,
}

/// A viewer drag in progress: the grab, where the pointer began (picture-local px), the picture
/// rectangle and placement it began from, the snap guides it is showing, and whether the next
/// write is the first (the one that takes the undo step).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ViewerHandle {
    pub(crate) grab: HandleGrab,
    pub(crate) start: (f32, f32),
    pub(crate) frame: Rect,
    pub(crate) base: engine::transform::TransformParams,
    pub(crate) src: (u32, u32),
    pub(crate) canvas: (u32, u32),
    pub(crate) first: bool,
    /// The frame line each axis snapped to, as a fraction of the frame (0, centre or 1).
    pub(crate) guide: (Option<f32>, Option<f32>),
}

/// One axis of a moved picture: `pos` is its centre as a canvas fraction from the centre, `half`
/// its half size, and the picture's centre and both edges snap to the frame's centre and edges
/// within `thr` (canvas fractions). Returns the snapped `pos` and the frame line it landed on.
fn snap_axis(pos: f32, half: f32, thr: f32) -> (f32, Option<f32>) {
    let mut best: Option<(f32, f32)> = None;
    for feature in [0.5 + pos - half, 0.5 + pos, 0.5 + pos + half] {
        for line in [0., 0.5, 1.] {
            let d = line - feature;
            if d.abs() <= thr && best.is_none_or(|(b, _)| d.abs() < b.abs()) {
                best = Some((d, line));
            }
        }
    }
    match best {
        Some((d, line)) => (pos + d, Some(line)),
        None => (pos, None),
    }
}

/// [`snap_axis`] on both axes: the snapped centre offset and the guide each axis shows.
pub(crate) fn snap_move(pos: (f32, f32), half: (f32, f32), thr: (f32, f32)) -> ((f32, f32), (Option<f32>, Option<f32>)) {
    let (x, gx) = snap_axis(pos.0, half.0, thr.0);
    let (y, gy) = snap_axis(pos.1, half.1, thr.1);
    ((x, y), (gx, gy))
}

/// The scale a corner grab gives: the picture's scale `base` times how much farther the pointer
/// is from the picture's `centre` now than where the grab began. `None` when the grab began on
/// the centre, or the scale would leave the slider's range.
pub(crate) fn corner_scale(base: f32, centre: (f32, f32), start: (f32, f32), now: (f32, f32)) -> Option<f32> {
    let dist = |p: (f32, f32)| ((p.0 - centre.0).powi(2) + (p.1 - centre.1).powi(2)).sqrt();
    let d0 = dist(start);
    if d0 < CORNER_MIN_PX {
        return None;
    }
    let (_, low, high) = crate::transform_ui::TRANSFORM_BANDS[crate::transform_ui::SCALE_BAND];
    Some((base * dist(now) / d0).clamp(low, high))
}

impl Player {
    /// The clip a viewer drag acts on: the selected video clip, when it is the one under the
    /// playhead, the picture is fitted and no source is up in the monitor. Zoomed in, a drag
    /// pans instead, so there is no clip to act on.
    fn viewer_handle_target(&self) -> Option<(Lane, usize)> {
        let session = self.session.as_ref()?;
        if self.viewer.zoom != Zoom::Fit || self.preview_session.is_some() {
            return None;
        }
        let (lane, idx) = self.selected.anchor()?;
        let under = session.video_clip_at(session.now()) == Some((lane, idx));
        (lane.kind == LaneKind::Video && under && self.clip_picture_size(lane, idx).is_some()).then_some((lane, idx))
    }

    /// Where the picture region's own box starts, in window px: the pointer's local origin.
    fn picture_origin(&self) -> (f32, f32) {
        let b = self.picture_box.get();
        (f32::from(b.origin.x), f32::from(b.origin.y))
    }

    /// A press on the selected clip's picture or one of its corners: the card opens on that clip
    /// (as the Transform card does), and the drag starts from the placement it has now.
    pub(crate) fn viewer_handle_start(&mut self, grab: HandleGrab, at: Point<Pixels>, window: &Window, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.viewer_handle_target() else {
            return;
        };
        if self.transform_open != Some((lane, idx)) {
            self.open_transform(cx);
        }
        if self.transform_open != Some((lane, idx)) {
            return;
        }
        let (Some(frame), Some(src)) = (self.viewer_frame_local(window.scale_factor()), self.clip_picture_size(lane, idx)) else {
            return;
        };
        let o = self.picture_origin();
        self.viewer_handle = Some(ViewerHandle {
            grab,
            start: (f32::from(at.x) - o.0, f32::from(at.y) - o.1),
            frame,
            base: self.transform_params(),
            src,
            canvas: self.transform_canvas(),
            first: true,
            guide: (None, None),
        });
        cx.notify();
    }

    /// A drag sample: the placement the grab gives from where it began, snapped, written through
    /// the Transform card's own path (keys and undo included). Shift keeps a move to one axis.
    pub(crate) fn viewer_handle_move(&mut self, at: Point<Pixels>, shift: bool, cx: &mut Context<Self>) {
        let Some(h) = self.viewer_handle else {
            return;
        };
        let o = self.picture_origin();
        let local = (f32::from(at.x) - o.0, f32::from(at.y) - o.1);
        let (fx, fy, fw, fh) = h.frame;
        let mut params = h.base;
        let mut guide = (None, None);
        match h.grab {
            HandleGrab::Move => {
                let (mut dx, mut dy) = (local.0 - h.start.0, local.1 - h.start.1);
                if shift {
                    match dx.abs() >= dy.abs() {
                        true => dy = 0.,
                        false => dx = 0.,
                    }
                }
                let pos = (h.base.pos_x + dx / fw, h.base.pos_y + dy / fh);
                let half = engine::transform::placed_half(&h.base, h.src, h.canvas);
                let (snapped, g) = snap_move(pos, half, (SNAP_PX / fw, SNAP_PX / fh));
                params.pos_x = snapped.0;
                params.pos_y = snapped.1;
                guide = g;
            }
            HandleGrab::Corner => {
                let centre = (fx + fw * (0.5 + h.base.pos_x), fy + fh * (0.5 + h.base.pos_y));
                let Some(scale) = corner_scale(h.base.scale, centre, h.start, local) else {
                    return;
                };
                params.scale = scale;
            }
        }
        let first = h.first;
        if params == self.transform_params() {
            self.viewer_handle = Some(ViewerHandle { guide, ..h });
            cx.notify();
            return;
        }
        self.viewer_handle = Some(ViewerHandle { guide, first: false, ..h });
        let busy = self.seek_since.is_some();
        match crate::stash_or_write(&mut self.pending_transform, params, first, busy) {
            Some(params) => self.write_transform(params, !first, cx),
            None => cx.notify(),
        }
    }

    /// The button came up, or the pointer left it: the drag is over and its guides are gone.
    pub(crate) fn viewer_handle_end(&mut self, cx: &mut Context<Self>) {
        if self.viewer_handle.take().is_some() {
            cx.notify();
        }
    }

    /// The handles over the selected clip's picture, placed in the picture stage's own
    /// coordinates (the stage is the layer's box): an outline with a move area, four corner
    /// squares, and the snap guide while one is held. `None` when no clip qualifies.
    pub(crate) fn viewer_handle_layer(&self, dpr: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        // The compare view draws both halves of the picture: the box and its handles would cross them.
        if self.compare != Compare::Off {
            return None;
        }
        let (lane, idx) = self.viewer_handle_target()?;
        let session = self.session.as_ref()?;
        let placement = match self.transform_open == Some((lane, idx)) {
            true => self.transform_params(),
            false => session.transform_at(lane, idx, self.key_frame()).unwrap_or_default(),
        };
        let src = self.clip_picture_size(lane, idx)?;
        let canvas = self.transform_canvas();
        let (lx, ly, fw, fh) = self.viewer_frame_local(dpr)?;
        let container = crate::ui::stance::PICTURE_BOUNDS.with(std::rc::Rc::clone).get();
        let o = self.picture_origin();
        let offset = (
            o.0 - f32::from(container.origin.x) + lx,
            o.1 - f32::from(container.origin.y) + ly,
        );
        let (hw, hh) = engine::transform::placed_half(&placement, src, canvas);
        let (ccx, ccy) = (offset.0 + fw * (0.5 + placement.pos_x), offset.1 + fh * (0.5 + placement.pos_y));
        let (bw, bh) = (2. * hw * fw, 2. * hh * fh);
        let (bx, by) = (ccx - bw / 2., ccy - bh / 2.);
        let t = crate::ui::theme::tokens();
        let me = cx.entity();
        let mut layer = div().absolute().top_0().left_0().size_full().overflow_hidden();
        if let Some(h) = self.viewer_handle {
            let (gx, gy) = h.guide;
            if let Some(line) = gx {
                layer = layer.child(
                    div().absolute().top_0().bottom_0().left(px((offset.0 + fw * line).round())).w(px(1.)).bg(rgb(t.accent)),
                );
            }
            if let Some(line) = gy {
                layer = layer.child(
                    div().absolute().left_0().right_0().top(px((offset.1 + fh * line).round())).h(px(1.)).bg(rgb(t.accent)),
                );
            }
        }
        let me_move = me.clone();
        let outline = div()
            .id("viewer-handle-move")
            .absolute()
            .left(px(bx.round()))
            .top(px(by.round()))
            .w(px(bw.round()))
            .h(px(bh.round()))
            .border_1()
            .border_color(rgb(t.accent))
            .cursor(gpui::CursorStyle::OpenHand)
            // No tip during a drag: the status bar already says what the drag does.
            .when(self.viewer_handle.is_none(), |o| {
                o.tooltip(crate::ui::widgets::overlay_tip_hover(
                    "Picture",
                    "",
                    Some("Drag to move, and it snaps to the centre and the edges (Shift keeps to one axis). Drag a corner to scale it. Zoomed in, a drag pans instead."),
                ))
            })
            .on_mouse_down(gpui::MouseButton::Left, move |e: &gpui::MouseDownEvent, window, app| {
                me_move.update(app, |this, cx| this.viewer_handle_start(HandleGrab::Move, e.position, window, cx));
                app.stop_propagation();
            });
        layer = layer.child(outline);
        let corners = [
            (bx, by, gpui::CursorStyle::ResizeUpLeftDownRight),
            (bx + bw, by, gpui::CursorStyle::ResizeUpRightDownLeft),
            (bx, by + bh, gpui::CursorStyle::ResizeUpRightDownLeft),
            (bx + bw, by + bh, gpui::CursorStyle::ResizeUpLeftDownRight),
        ];
        for (i, (x, y, cursor)) in corners.into_iter().enumerate() {
            let me_corner = me.clone();
            layer = layer.child(
                div()
                    .id(("viewer-handle-corner", i))
                    .absolute()
                    .left(px((x - 5.).round()))
                    .top(px((y - 5.).round()))
                    .size(px(10.))
                    .bg(rgb(t.accent))
                    .border_1()
                    .border_color(rgb(t.e0))
                    .cursor(cursor)
                    .on_mouse_down(gpui::MouseButton::Left, move |e: &gpui::MouseDownEvent, window, app| {
                        me_corner.update(app, |this, cx| this.viewer_handle_start(HandleGrab::Corner, e.position, window, cx));
                        app.stop_propagation();
                    }),
            );
        }
        Some(layer.into_any_element())
    }
}

/// Paints the frame `image` in a box: fitted, or at the zoom's size with `pan` applied, then the
/// guides and the aspect mask over the frame. Absolute and full-box, like [`crate::render::letterboxed_image`].
pub(crate) fn viewer_picture(
    image: Arc<RenderImage>,
    native: (f32, f32),
    prefs: Prefs,
    pan: (f32, f32),
    dpr: f32,
) -> impl IntoElement + Styled {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, app| {
            let bx = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let origin = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let frame = picture_rect(prefs.zoom, native, bx, origin, pan, dpr);
            let _ = window.paint_image(quad(frame), Corners::default(), image.clone(), 0, false);
            let t = crate::ui::theme::tokens();
            let line = rgba(crate::ui::theme::with_alpha(t.accent, 0xc8));
            if let Some(ratio) = prefs.mask.ratio() {
                let keep = mask_window(frame, ratio);
                let dim = rgba(crate::ui::theme::with_alpha(t.e0, 0xa8));
                for r in outside(frame, keep) {
                    window.paint_quad(gpui::fill(quad(r), dim));
                }
                stroke(window, keep, line);
            }
            match prefs.guide {
                Guide::Off => {}
                Guide::Safe => {
                    stroke(window, inset(frame, 0.9), line);
                    stroke(window, inset(frame, 0.8), line);
                    for (share, name) in [(0.9, "Action safe"), (0.8, "Title safe")] {
                        let r = inset(frame, share);
                        guide_label(window, app, name, (r.0 + 4., r.1 + 2.), t.ink2, t.e0);
                    }
                }
                Guide::Thirds => {
                    let (x, y, w, h) = frame;
                    for k in [1., 2.] {
                        window.paint_quad(gpui::fill(quad(((x + w * k / 3.).round(), y, 1., h)), line));
                        window.paint_quad(gpui::fill(quad((x, (y + h * k / 3.).round(), w, 1.)), line));
                    }
                }
                Guide::Centre => {
                    let (cx, cy) = (frame.0 + frame.2 / 2., frame.1 + frame.3 / 2.);
                    let arm = (frame.2.min(frame.3) * 0.04).max(8.);
                    window.paint_quad(gpui::fill(quad((cx - arm, cy.round(), arm * 2., 1.)), line));
                    window.paint_quad(gpui::fill(quad((cx.round(), cy - arm, 1., arm * 2.)), line));
                }
            }
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .left_0()
    .right_0()
    .size_full()
}

/// The viewer's compare view (View > Compare grade): the picture as shot against the graded one.
/// Preview only: it changes what this screen draws, never the project or an export.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Compare {
    #[default]
    Off,
    /// A vertical wipe at the centre: as shot on the left, graded on the right.
    Split,
    /// Both halves, each fitted to its half: as shot on the left, graded on the right.
    Side,
}

impl Compare {
    /// The choices in the order the card and the menu list them.
    pub(crate) const ALL: [(Compare, &'static str); 3] = [(Compare::Off, "Off"), (Compare::Split, "Split"), (Compare::Side, "Side by side")];
}

/// The compare view's picture: `after` (the graded frame) in the box, and `before` (the frame as
/// shot) over it. A split cuts `before` off at the centre, leaving the graded half on the right;
/// side by side fits each frame to its own half. Absolute and full-box, like [`viewer_picture`].
pub(crate) fn compare_picture(
    mode: Compare,
    before: Option<Arc<RenderImage>>,
    after: Arc<RenderImage>,
    native: (f32, f32),
    zoom: Zoom,
    pan: (f32, f32),
    dpr: f32,
) -> impl IntoElement + Styled {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, app| {
            let bx = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let origin = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let t = crate::ui::theme::tokens();
            let centre = origin.0 + bx.0 / 2.;
            let (left, right) = ((origin.0, origin.1, bx.0 / 2., bx.1), (centre, origin.1, bx.0 / 2., bx.1));
            match mode {
                Compare::Side => {
                    // Each half fits its own picture; its caption sits inside the picture's bottom left.
                    let fit_r = rect_of(gpui::ObjectFit::Contain.get_bounds(quad(right), after.size(0)));
                    if let Some(b) = &before {
                        let fit_l = rect_of(gpui::ObjectFit::Contain.get_bounds(quad(left), b.size(0)));
                        let _ = window.paint_image(quad(fit_l), Corners::default(), b.clone(), 0, false);
                        compare_label(window, app, "Before", fit_l);
                    }
                    let _ = window.paint_image(quad(fit_r), Corners::default(), after.clone(), 0, false);
                    compare_label(window, app, "After", fit_r);
                }
                _ => {
                    let frame = picture_rect(zoom, native, bx, origin, pan, dpr);
                    let _ = window.paint_image(quad(frame), Corners::default(), after.clone(), 0, false);
                    if let Some(b) = &before {
                        window.with_content_mask(Some(gpui::ContentMask { bounds: quad(left) }), |window| {
                            let _ = window.paint_image(quad(frame), Corners::default(), b.clone(), 0, false);
                        });
                    }
                    // The part of the picture that is on screen, split at the centre.
                    let vis = clip_rect(frame, (origin.0, origin.1, bx.0, bx.1));
                    window.paint_quad(gpui::fill(quad((centre.round() - 1., vis.1, 2., vis.3)), rgb(t.ink1)));
                    if before.is_some() {
                        compare_label(window, app, "Before", (vis.0, vis.1, centre - vis.0, vis.3));
                    }
                    compare_label(window, app, "After", (centre, vis.1, vis.0 + vis.2 - centre, vis.3));
                }
            }
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .left_0()
    .right_0()
    .size_full()
}

/// A bounds as a rectangle.
fn rect_of(b: Bounds<Pixels>) -> Rect {
    (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height))
}

/// `r` cut down to the part inside `to`.
fn clip_rect(r: Rect, to: Rect) -> Rect {
    let (x0, y0) = (r.0.max(to.0), r.1.max(to.1));
    let (x1, y1) = ((r.0 + r.2).min(to.0 + to.2), (r.1 + r.3).min(to.1 + to.3));
    (x0, y0, (x1 - x0).max(0.), (y1 - y0).max(0.))
}

/// A caption at the bottom left of `img`, on the dim plate the guide names use, so it reads over the picture.
fn compare_label(window: &mut Window, app: &mut App, text: &str, img: Rect) {
    let t = crate::ui::theme::tokens();
    let h = f32::from(crate::ui::type_scale::caption().size) + 6.;
    // guide_label starts its plate 4 px left of the point it is given.
    guide_label(window, app, text, (img.0 + 12., img.1 + img.3 - 8. - h), t.ink1, t.e0);
}

/// A small caption naming a guide, on a dim plate so it reads over any picture; its top-left at `at`.
fn guide_label(window: &mut Window, app: &mut App, text: &str, at: (f32, f32), ink: u32, plate: u32) {
    let style = crate::ui::type_scale::caption();
    let run = gpui::TextRun {
        len: text.len(),
        font: style.font.clone(),
        color: rgb(ink).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(text.to_string().into(), style.size, &[run], None);
    let (w, h) = (f32::from(line.width) + 8., f32::from(style.size) + 6.);
    window.paint_quad(gpui::fill(quad((at.0 - 4., at.1, w, h)), rgba(crate::ui::theme::with_alpha(plate, 0xb0))));
    let _ = line.paint(point(px(at.0), px(at.1 + 3.)), px(f32::from(style.size) + 1.), window, app);
}

fn quad(r: Rect) -> Bounds<Pixels> {
    Bounds::new(point(px(r.0), px(r.1)), size(px(r.2), px(r.3)))
}

/// A 1px outline of `r`.
fn stroke(window: &mut Window, r: Rect, color: gpui::Rgba) {
    let (x, y, w, h) = (r.0.round(), r.1.round(), r.2.round(), r.3.round());
    for edge in [(x, y, w, 1.), (x, y + h - 1., w, 1.), (x, y, 1., h), (x + w - 1., y, 1., h)] {
        window.paint_quad(gpui::fill(quad(edge), color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterboxes_and_zoom_scales_native_pixels() {
        assert_eq!(shown_size(Zoom::Fit, (1920., 1080.), (960., 960.), 1.), (960., 540.));
        assert_eq!(shown_size(Zoom::P50, (1920., 1080.), (100., 100.), 1.), (960., 540.));
        assert_eq!(shown_size(Zoom::P100, (1920., 1080.), (100., 100.), 2.), (960., 540.));
    }

    #[test]
    fn pan_never_leaves_the_box_and_is_zero_when_the_picture_fits() {
        assert_eq!(clamp_pan((50., 50.), (400., 200.), (500., 300.)), (0., 0.));
        assert_eq!(clamp_pan((500., -500.), (1000., 800.), (600., 600.)), (200., -100.));
    }

    #[test]
    fn wheel_steps_stay_on_the_ladder() {
        assert_eq!(Zoom::Fit.step(true), Zoom::P25);
        assert_eq!(Zoom::P25.step(false), Zoom::Fit);
        assert_eq!(Zoom::Fit.step(false), Zoom::Fit);
        assert_eq!(Zoom::P200.step(true), Zoom::P200);
    }

    #[test]
    fn keys_round_trip() {
        for z in Zoom::ALL {
            assert_eq!(Zoom::from_key(z.key()), Some(z));
        }
        for q in Quality::ALL {
            assert_eq!(Quality::from_key(q.key()), Some(q));
        }
        for m in Mask::ALL {
            assert_eq!(Mask::from_key(m.key()), Some(m));
        }
        assert_eq!(Zoom::from_key("7"), None);
    }

    #[test]
    fn safe_areas_and_masks_are_centred() {
        let frame = (0., 0., 1920., 1080.);
        let i = inset(frame, 0.9);
        assert!((i.0 - 96.).abs() < 0.01 && (i.1 - 54.).abs() < 0.01 && (i.2 - 1728.).abs() < 0.01);
        let w = mask_window(frame, 9. / 16.);
        assert!((w.2 - 607.5).abs() < 0.01 && w.3 == 1080. && (w.0 - 656.25).abs() < 0.01);
        assert_eq!(outside(frame, w).len(), 2);
        let sq = mask_window((0., 0., 400., 600.), 1.);
        assert_eq!(sq, (0., 100., 400., 400.));
        assert_eq!(outside((0., 0., 400., 600.), sq).len(), 2);
        assert!(outside(frame, frame).is_empty());
    }

    #[test]
    fn quarter_keeps_a_quarter_of_each_side() {
        assert_eq!(Quality::Quarter.factor(), 0.25);
        assert_eq!(Quality::Full.factor(), 1.);
    }

    #[test]
    fn a_move_snaps_its_centre_and_edges_to_the_frame() {
        let thr = (0.01, 0.01);
        // A box half the frame wide, nudged 0.004 off centre: the centre snaps back to 0.
        let (p, g) = snap_move((0.004, 0.0), (0.25, 0.25), thr);
        assert!(p.0.abs() < 1e-6, "{}", p.0);
        assert_eq!(g.0, Some(0.5), "the centre guide sits on the frame's centre");
        // A box 0.4 wide, its left edge 0.004 off the frame's left edge: it moves so the edge sits on it.
        let (p, g) = snap_move((-0.296, 0.1), (0.2, 0.2), thr);
        assert!((p.0 - (-0.3)).abs() < 1e-6, "{}", p.0);
        assert_eq!(g.0, Some(0.0));
        assert_eq!(g.1, None, "nothing near on the other axis, so no guide there");
        assert_eq!(p.1, 0.1, "an axis with nothing to snap to keeps its place");
        // Far from every line, nothing moves and no guide shows.
        assert_eq!(snap_move((0.13, 0.13), (0.1, 0.1), thr), ((0.13, 0.13), (None, None)));
    }

    #[test]
    fn a_corner_grab_scales_by_the_pointer_distance_from_the_centre() {
        let centre = (100., 100.);
        // Twice as far out from the centre: twice the scale.
        let s = corner_scale(0.5, centre, (140., 100.), (180., 100.)).unwrap();
        assert!((s - 1.0).abs() < 1e-5, "{s}");
        // Half way back in.
        let s = corner_scale(1.0, centre, (140., 100.), (120., 100.)).unwrap();
        assert!((s - 0.5).abs() < 1e-5, "{s}");
        // A grab on the centre has no ratio, and the scale stays in the slider's range.
        assert_eq!(corner_scale(1.0, centre, (101., 100.), (150., 100.)), None);
        assert_eq!(corner_scale(1.0, centre, (110., 100.), (1100., 100.)), Some(4.0));
    }
}
