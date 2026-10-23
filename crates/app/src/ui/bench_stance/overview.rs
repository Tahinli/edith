//! The overview strip and the scrollbar under the lanes (DESIGN §3 "Overview
//! strip", §5 layout, §7 zoom). The overview is the whole film in one 14px
//! band above the ruler: each video clip in its film ink across its span,
//! audio as thin translucent slivers along the bottom, the playhead as a
//! white tick, and the bench's window as a closed 1px box. It replaces the
//! contact strip that used to sit in the time band; the export range's two
//! mark grips moved onto it with the rest of the film minimap.
//!
//! The scrollbar is the same window as a thumb, so both strips share
//! [`window_frac`] and [`grab_for`], and both drag through the one
//! `Player::scroll_drag` gesture (`player/view.rs::pan_window_to`).

use super::{HEAD_W, ROW_GAP, RULER_H};
use crate::layout::{SCROLL_HIT_DOWN, SCROLL_HIT_UP, SCROLL_ROW_H};
use crate::ui::hitmap;
use crate::*;

/// The overview's own height (REDESIGN §3.6's 12px overview strip).
pub(crate) const OVERVIEW_H: f32 = crate::layout::redesign::OVERVIEW_H;
/// An audio clip's sliver: a thin band along the overview's bottom edge.
const SLIVER_H: f32 = 3.;
/// The window box never draws narrower than this, however far the view is
/// zoomed in: a box a pixel wide is a box no hand can find.
const WINDOW_MIN_W: f32 = 4.;
/// How far either side of a mark tick the hand still grabs it.
const MARK_GRAB: f32 = 6.;

thread_local! {
    /// Which export mark a live drag on the overview is moving (the grips of
    /// [`mark_grip`]), `None` when no mark drag is up. The same transient
    /// shape as the `Player` gesture fields, kept here because the mark drag
    /// is the only thing that reads it.
    static MARK_DRAG: Cell<Option<ActionId>> = const { Cell::new(None) };
}

/// The strip rows the bench spends above its lane stack: the overview, the
/// ruler, the gaps between them and the lanes, and the scrollbar row with its
/// gap while there is a scrollbar to draw. A `const fn` so the bench floor
/// (`layout::BENCH_MIN_H`) can take the same numbers `render` does.
pub(crate) const fn chrome_h(scroll: bool) -> f32 {
    OVERVIEW_H
        + ROW_GAP
        + RULER_H
        + ROW_GAP
        + match scroll {
            true => ROW_GAP + SCROLL_ROW_H,
            false => 0.,
        }
}

/// The bench's window on the whole film, as fractions of the film's length:
/// where it starts and how much of the film it shows. A film with no length
/// yet shows all of itself, so the box never vanishes into nothing.
pub(crate) fn window_frac(start: f64, span: f64, duration: f64) -> (f32, f32) {
    if duration <= 0. {
        return (0., 1.);
    }
    let w = (span / duration).clamp(0., 1.) as f32;
    let x = ((start / duration) as f32).clamp(0., 1. - w);
    (x, w)
}

/// How far into a window a press grabbed it, in px. A press inside the
/// window keeps the point it landed on under the hand; a press outside it
/// names the window's centre, so the view jumps there and the drag carries
/// on from the middle. `track_w` is the bed's width in px, `window` the
/// fractions from [`window_frac`], `min_w` the drawn floor of the window.
pub(crate) fn grab_for(at: f32, track_w: f32, window: (f32, f32), min_w: f32) -> f32 {
    let (x, w) = window;
    let (left, width) = (x * track_w, (w * track_w).max(min_w));
    match at >= left && at <= left + width {
        true => at - left,
        false => width / 2.,
    }
}

/// The overview strip: the film's clips, the playhead, the window box, and
/// the export range's two grips. Mounted above the ruler in the bench's own
/// column, so it shares the ruler's bed (`HEAD_W` in, the bed's width).
pub(crate) fn overview(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let duration = player.drawn_duration();
    let view = player.view();
    let position = player.session.as_ref().map_or(0., PlaybackSession::now);
    let (video, audio) = spans(player, duration, player.fps);
    let (win_x, win_w) = window_frac(view.scale.start, view.span(), duration);
    let at = |t: f64| match duration > 0. {
        true => (t / duration).clamp(0., 1.) as f32,
        false => 0.,
    };
    div()
        .id("stance-overview")
        .relative()
        .flex_none()
        .ml(px(HEAD_W))
        .h(px(OVERVIEW_H))
        .bg(rgb(crate::ui::theme::tokens().e1))
        .cursor_pointer()
        // Pressing the strip outside the window centres the view there; a
        // press inside it grabs the window where it was taken. The drag then
        // rides the root's own move and release (`Player::drag_move`), so the
        // pointer may leave the 14px band and the window keeps following.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                let view = this.view();
                if view.duration <= view.span() {
                    return;
                }
                let track = this.ruler.get();
                let at = px_along(event.position.x, track);
                let window = window_frac(view.scale.start, view.span(), view.duration);
                let grab = grab_for(at, f32::from(track.size.width), window, WINDOW_MIN_W);
                this.scroll_drag = Some(grab);
                this.pan_window_to(at, cx);
            }),
        )
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
            if event.pressed_button != Some(MouseButton::Left) {
                MARK_DRAG.with(|m| m.set(None));
                return;
            }
            // A mark grip is under the hand: the drag sets that mark instead
            // of anything else (the grip's own press armed this and stopped
            // propagation).
            let Some(action) = MARK_DRAG.with(Cell::get) else {
                return;
            };
            let duration = this.drawn_duration();
            if duration > 0. {
                let frac = frac_along(event.position.x, this.ruler.get());
                let at = (f64::from(frac) * duration * this.active_fps())
                    .max(0.)
                    .round() as u32;
                let (lo, hi) = this.range.unwrap_or((at, at));
                let (a, b) = if action == ActionId::SetIn {
                    (at, hi)
                } else {
                    (lo, at)
                };
                // Dragged past its partner, the mark under the hand becomes
                // the OTHER mark (`ordered_range` keeps the pair legal, and
                // `i`/`o` swap the same way). Without this flip both ticks
                // walk along with the pointer and re-order on every move.
                if crate::player::actions::ordered_range(a, b) != (a, b) {
                    MARK_DRAG.with(|m| {
                        m.set(Some(if action == ActionId::SetIn {
                            ActionId::SetOut
                        } else {
                            ActionId::SetIn
                        }))
                    });
                }
                this.range = Some(crate::player::actions::ordered_range(a, b));
            }
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|_, _, _, _| MARK_DRAG.with(|m| m.set(None))),
        )
        .tooltip(crate::ui::widgets::tip_hover("Whole timeline. Drag to move the view.", "", None))
        .children(hitmap::control(
            "timeline.overview",
            "Timeline overview",
            true,
        ))
        .children(video.into_iter().map(|(from, to, ink)| {
            div()
                .absolute()
                .left(relative(from))
                .top_0()
                .bottom(px(SLIVER_H + 1.))
                .w(relative((to - from).max(0.002)))
                .min_w(px(1.))
                .bg(rgb(ink))
        }))
        // Audio as slivers: the source's own ink at ~40% alpha (0x66), so a
        // sound-only stretch still reads as film without drowning the picture.
        .children(audio.into_iter().map(|(from, to, ink)| {
            div()
                .absolute()
                .left(relative(from))
                .bottom_0()
                .h(px(SLIVER_H))
                .w(relative((to - from).max(0.002)))
                .min_w(px(1.))
                .bg(rgba((ink << 8) | 0x4d))
        }))
        // What the window does not show is dimmed, so a window that fills
        // the strip still reads as "everything is in view" by its box alone.
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .h_full()
                .w(relative(win_x))
                .bg(rgba(crate::ui::theme::with_alpha(0x000000, 0x80))),
        )
        .child(
            div()
                .absolute()
                .right_0()
                .top_0()
                .h_full()
                .w(relative((1. - win_x - win_w).max(0.)))
                .bg(rgba(crate::ui::theme::with_alpha(0x000000, 0x80))),
        )
        // The closed 2px box marks the window; its inside is clear so the
        // film shows through it.
        .child(
            div()
                .id("stance-overview-window")
                .absolute()
                .left(relative(win_x))
                .top_0()
                .h_full()
                .w(relative(win_w))
                .min_w(px(WINDOW_MIN_W))
                .border_2()
                // An empty timeline has nothing to window: the box is drawn quietly.
                .border_color(match duration > 0. {
                    true => rgb(INK1()),
                    false => rgba(crate::ui::theme::tokens().line),
                }),
        )
        // The export range's two grips (DESIGN §5 as amended 2026-09-10): the
        // marks live on the film itself. Nothing is drawn until a mark exists.
        .when_some(
            player.range.filter(|_| duration > 0.),
            |el, (in_f, out_f)| {
                let at_t = |f: u32| at(f64::from(f) / player.active_fps());
                el.child(
                    div()
                        .id("stance-overview-range")
                        .absolute()
                        .left(relative(at_t(in_f)))
                        .top_0()
                        .h_full()
                        .w(relative((at_t(out_f) - at_t(in_f)).max(0.)))
                        .bg(rgba(super::ruler::RANGE_SHADE)),
                )
                .child(mark_grip(
                    player,
                    "stance-strip-mark-in",
                    at_t(in_f),
                    "Mark in",
                    ActionId::SetIn,
                ))
                .child(mark_grip(
                    player,
                    "stance-strip-mark-out",
                    at_t(out_f),
                    "Mark out",
                    ActionId::SetOut,
                ))
            },
        )
        // The white playhead tick, drawn last so it sits above the film and
        // the window box: 2px, in a 1px black halo (DESIGN §4.1).
        .when(duration > 0., |el| {
            el.child(
                div()
                    .id("stance-overview-playhead")
                    .absolute()
                    .left(relative(at(position)))
                    .top_0()
                    .h_full()
                    .ml(px(-1.))
                    .w(px(2.))
                    .bg(rgb(LAMP_WHITE())),
            )
        })
}

/// The scrollbar under the lanes: the window as a thumb along a hairline
/// track, the same bed as the ruler above the lanes. Mounted only while the
/// film is longer than the window ([`render`](super::render) reserves the row
/// on the same test). A press on the thumb grabs it; a press on the track
/// centres the window there. Its hit band is the row plus [`SCROLL_HIT_UP`]
/// over the bottom of the lanes and [`SCROLL_HIT_DOWN`] of the bench's own
/// padding below it: 24px (WCAG 2.5.8) with no more than 8px taken from the
/// lanes.
pub(crate) fn scrollbar(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let view = player.view();
    let (win_x, win_w) = window_frac(view.scale.start, view.span(), view.duration);
    let bar_top = (SCROLL_ROW_H - SCROLL_BAR_H) / 2.;
    div()
        .id("stance-scrollbar")
        .relative()
        .flex_none()
        .ml(px(HEAD_W))
        .h(px(SCROLL_ROW_H))
        .child(bounds_probe(player.scroll_track.clone()))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(bar_top))
                .h(px(SCROLL_BAR_H))
                .bg(rgb(DARK_HAIRLINE())),
        )
        .child(
            div()
                .id("stance-scroll-thumb")
                .absolute()
                .left(relative(win_x))
                .top(px(bar_top))
                .h(px(SCROLL_BAR_H))
                .w(relative(win_w))
                .min_w(px(SCROLL_THUMB_MIN))
                .bg(rgb(INK3())),
        )
        .child(
            div()
                .id("stance-scroll-hit")
                .absolute()
                .left_0()
                .right_0()
                .top(px(-SCROLL_HIT_UP))
                .h(px(SCROLL_ROW_H + SCROLL_HIT_UP + SCROLL_HIT_DOWN))
                .cursor_pointer()
                .tooltip(crate::ui::widgets::tip_hover(
                    "Pan the timeline window",
                    "drag",
                    None,
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        let track = this.scroll_track.get();
                        let at = px_along(event.position.x, track);
                        let view = this.view();
                        let window = window_frac(view.scale.start, view.span(), view.duration);
                        this.scroll_drag = Some(grab_for(
                            at,
                            f32::from(track.size.width),
                            window,
                            SCROLL_THUMB_MIN,
                        ));
                        this.pan_window_to(at, cx);
                    }),
                )
                .children(hitmap::control(
                    "timeline.scrollbar",
                    "Timeline scrollbar",
                    true,
                )),
        )
}

/// One clip on the overview: its `(from, to)` fractions of the film and its ink.
type Span = (f32, f32, u32);

/// The film's clips as `(from, to, ink)` fractions of the film, split into
/// video and audio. Each clip takes its source's tint, the same one the lane
/// dot and the bench's clip anatomy draw. A film with no length yields none.
fn spans(player: &Player, duration: f64, fps: f64) -> (Vec<Span>, Vec<Span>) {
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let Some(session) = player.session.as_ref().filter(|_| duration > 0.) else {
        return (video, audio);
    };
    let frac = |f: u32| ((f64::from(f) / fps) / duration).clamp(0., 1.) as f32;
    for lane in session.lanes() {
        let out = match lane.kind {
            LaneKind::Video => &mut video,
            LaneKind::Audio => &mut audio,
            _ => continue,
        };
        for clip in session.lane_clips(lane) {
            // A compound clip shows the clips it stands for, in their own inks.
            if engine::project::is_sequence_source(clip.source) {
                for p in session.compound_pieces(lane.kind, clip) {
                    out.push((frac(clip.start + p.start), frac(clip.start + p.start + p.frames()), source_tint(p.source)));
                }
                continue;
            }
            out.push((frac(clip.start), frac(clip.end()), source_tint(clip.source)));
        }
    }
    (video, audio)
}

/// One export-range mark, drawn on the overview at its own film position: an
/// `ink1` tick with a ±[`MARK_GRAB`] door around it that takes precedence
/// over the strip's own press (`stop_propagation` on its press). Dragging it
/// sets that mark; `i`, `o` and `^u` are unchanged.
fn mark_grip(
    player: &Player,
    id: &'static str,
    frac: f32,
    name: &'static str,
    action: ActionId,
) -> impl IntoElement {
    let chord = player.keymap.chord(action);
    div()
        .id(id)
        .absolute()
        .left(relative(frac))
        .top_0()
        .h_full()
        .ml(px(-MARK_GRAB))
        .w(px(MARK_GRAB * 2. + 1.))
        .flex()
        .justify_center()
        .cursor_col_resize()
        .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, _, cx| {
            MARK_DRAG.with(|m| m.set(Some(action)));
            cx.stop_propagation();
        })
        .tooltip(crate::ui::widgets::tip_hover(name, &chord, None))
        .children(hitmap::action(action, player.enable(action, None).yes()))
        .child(div().w(px(1.)).h_full().bg(rgb(INK1())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_over_the_whole_film_fills_the_bed_and_a_film_with_no_length_shows_all_of_itself() {
        assert_eq!(window_frac(0., 100., 100.), (0., 1.));
        assert_eq!(
            window_frac(0., 500., 100.),
            (0., 1.),
            "a span past the film clamps to it"
        );
        assert_eq!(window_frac(30., 10., 0.), (0., 1.));
    }

    #[test]
    fn the_window_box_sits_where_its_start_and_span_say_on_the_film() {
        let (x, w) = window_frac(25., 25., 100.);
        assert!(
            (x - 0.25).abs() < 1e-6 && (w - 0.25).abs() < 1e-6,
            "{x} {w}"
        );
        // Parked at the far end, the box stops at the bed's right edge rather
        // than running off it.
        let (x, w) = window_frac(90., 25., 100.);
        assert!(
            (x - 0.75).abs() < 1e-6 && (x + w - 1.).abs() < 1e-6,
            "{x} {w}"
        );
        let (x, _) = window_frac(-5., 10., 100.);
        assert_eq!(x, 0., "a start before the film clamps to its head");
    }

    #[test]
    fn a_press_inside_the_window_keeps_its_grab_and_one_outside_centres_it() {
        // A 400px bed, a window at 100..200px.
        let window = (0.25, 0.25);
        assert_eq!(grab_for(150., 400., window, 4.), 50.);
        // Outside, the grab is the window's middle: a press anywhere off the
        // box centres the view on the press.
        assert_eq!(grab_for(300., 400., window, 4.), 50.);
        assert_eq!(grab_for(20., 400., window, 4.), 50.);
    }

    #[test]
    fn a_thumb_never_draws_narrower_than_its_floor() {
        // A 1-second window on a 10-hour film is a sliver; the drawn floor is
        // what a press lands on, so a press off it centres on that floor.
        let (_, w) = window_frac(0., 1., 36_000.);
        assert!(w * 400. < SCROLL_THUMB_MIN);
        assert_eq!(
            grab_for(50., 400., (0.5, w), SCROLL_THUMB_MIN),
            SCROLL_THUMB_MIN / 2.
        );
    }

    #[test]
    fn the_bench_chrome_grows_by_the_scrollbar_row_only_while_there_is_one() {
        assert_eq!(chrome_h(true) - chrome_h(false), ROW_GAP + SCROLL_ROW_H);
        assert_eq!(chrome_h(false), OVERVIEW_H + 2. * ROW_GAP + RULER_H);
    }
}
