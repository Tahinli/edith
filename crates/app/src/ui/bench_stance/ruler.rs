//! The pinned ruler: ticks, timecode plate, the playhead's lamp tab and line (DESIGN §6.5).

use super::*;
use crate::timeline_math::{ruler_label, ruler_step};
use crate::ui::widgets::MarkerField;

/// The pixel pitch a tick label needs before the next one may be drawn
/// (DESIGN §6.5): 80px keeps a `1:00:00` label clear of its neighbour at
/// the 12px mono floor.
const TICK_MIN_PX: f64 = 80.;
/// The least clear gap kept between one label's end and the next label's start.
const LABEL_GAP_PX: f32 = 8.;
/// The black halo either side of the playhead line, in px (DESIGN §4.1).
const PLAYHEAD_HALO: f32 = 1.;

/// The playhead timecode plate's own width: eleven mono characters at the
/// 14px readout role plus its horizontal padding and a 1px rounding guard,
/// derived from the shared list-character calibration so the plate grows with
/// its type scale.
const PLATE_W: f32 = 11. * crate::layout::LIST_CHAR_W / type_scale::CHORD_METADATA_MIN_PX
    * type_scale::CHORD_METADATA_MAX_PX
    + 9.;

/// The ruler's ticks as `(x, label)` in the bed's own pixels. The step and
/// its labels live in `timeline_math` ([`ruler_step`], [`ruler_label`]):
/// human stops (frames, then 1 s, 2 s, 5 s, 10 s, 30 s, a minute...) read as
/// `2s`, `1:30` and `+10f`, never a colon form a reader could take for the
/// transport's `HH:MM:SS:FF`. Walked from the first tick at or after the
/// bed's left edge to its right edge; a label that would start within
/// [`LABEL_GAP_PX`] of the previous one's end, or inside the first `plate_w`
/// pixels, is skipped, so no two labels ever touch. Pure, so the test pins the
/// sequence without a window.
pub(super) fn ticks(scale: Scale, fps: f64, bed_w: f32, plate_w: f32) -> Vec<(f32, String)> {
    let mut ticks = Vec::new();
    // Guarded on `pps > 0` -- a zero scale (no session yet) has no step that
    // ever advances past the bed's own width, which would loop forever.
    if scale.pps <= 0. {
        return ticks;
    }
    let step = ruler_step(scale.pps, fps, TICK_MIN_PX);
    let secs = step.secs(fps);
    // Walked by index, not by summing `secs`, so a frame step's position
    // does not drift off its frame over a long bed.
    let mut i = (scale.start / secs).ceil().max(0.);
    let mut label_end = f32::NEG_INFINITY;
    while scale.px_at(i * secs) <= bed_w {
        let x = scale.px_at(i * secs).max(0.);
        let label = ruler_label(step, i as u64, fps);
        let label_w = label.chars().count() as f32 * char_w(type_scale::FLOOR_PX);
        if x >= plate_w && x + label_w <= bed_w && x >= label_end + LABEL_GAP_PX {
            label_end = x + label_w;
            ticks.push((x, label));
        }
        i += 1.;
    }
    ticks
}

/// The shade of the In-Out range on the ruler and the overview strip.
pub(crate) const RANGE_SHADE: u32 = 0x5aa0ff40;

/// The In-Out range as `(x, width)` in the ruler's pixels, clipped to the
/// bed; `None` when no range is set or it is wholly off the bed.
pub(super) fn range_band(
    range: Option<(u32, u32)>,
    fps: f64,
    scale: crate::viewport::Scale,
    bed_w: f32,
) -> Option<(f32, f32)> {
    let (a, b) = range?;
    let x0 = scale.px_at(f64::from(a) / fps).max(0.);
    let x1 = scale.px_at(f64::from(b) / fps).min(bed_w);
    (x1 > x0).then_some((x0, x1 - x0))
}

/// The pinned ruler: click or drag to seek, tick marks with mono `ink3`
/// timecodes, and the playhead's own lamp-white line with its grab tab
/// (DESIGN §6.5). `render` places it; the tick and timecode-plate maths live
/// here with it.
pub(super) fn ruler(
    player: &Player,
    position: f64,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let scale = player.scale;
    let bed_w = f32::from(player.ruler.get().size.width).max(1.);
    let filled = scale.px_at(position).clamp(0., bed_w);
    // Ruler ticks (DESIGN §6.5): see [`ticks`].
    // The ruler's one reserved band, which no tick label may enter: the
    // playhead timecode plate, which now shows only while a scrub
    // drag is live (the hero timecode leads the time band at rest, DESIGN
    // §4, and the ledger keeps position, §6.6 -- a third resting copy is the
    // crowding) -- at rest the band is zero and the ticks have the width
    // back.
    let plate_w = if player.scrubbing { PLATE_W } else { 0. };
    let fps = player.fps;
    let ticks = ticks(scale, fps, bed_w, plate_w);
    let flags: Vec<(f32, f32)> = player
        .session
        .as_ref()
        .map(|s| marker_flags(s.markers(), fps, scale, bed_w).into_iter().map(|f| (f.x, f.reach())).collect())
        .unwrap_or_default();
    // The playhead line is 3px while the transport runs and 2px at rest
    // (DESIGN §4.1), each with a 1px black halo either side, so the line is
    // the same white on every clip colour and the moving one reads as live
    // without a colour change.
    let playing = player.transport().is_playing();
    let playhead_w = if playing { 3. } else { 2. };
    let playhead_tc = crate::viewport::timecode(position, fps);
    div()
        .id("bench-ruler")
        .flex_none()
        .h(px(RULER_H))
        .ml(px(HEAD_W))
        .relative()
        .bg(rgb(crate::ui::theme::tokens().e1))
        .border_b_1()
        .border_color(rgba(crate::ui::theme::tokens().line))
        .cursor_pointer()
        .child(bounds_probe(player.ruler.clone()))
        // The In-Out range, shaded behind the ticks as an NLE does.
        .children(range_band(player.range, fps, scale, bed_w).map(|(x, w)| {
            div()
                .id("bench-ruler-range")
                .absolute()
                .bottom_0()
                .h(px(4.))
                .left(px(x))
                .w(px(w))
                .bg(rgba(crate::ui::theme::with_alpha(crate::ui::theme::tokens().accent, 0x66)))
        }))
        // The render bar (DESIGN.md §6.5): green where a render matches the timeline, amber
        // where a clip with effects still has none. A 3px strip laid over the ruler's top
        // edge, so no height changes when it appears.
        .children(player.render_bands.iter().filter_map(|band| {
            let (x, w) = range_band(Some((band.from, band.to)), fps, scale, bed_w)?;
            let tokens = crate::ui::theme::tokens();
            let colour = if band.cached { tokens.ok } else { tokens.warn };
            Some(
                div()
                    .id("bench-ruler-render")
                    .absolute()
                    .top_0()
                    .h(px(3.))
                    .left(px(x))
                    .w(px(w))
                    .bg(rgb(colour)),
            )
        }))
        // The render being made fills green from its start as it goes.
        .children(player.render_progress.and_then(|((from, to), done)| {
            let to_done = from + ((to - from) as f32 * done) as u32;
            let (x, w) = range_band(Some((from, to_done)), fps, scale, bed_w)?;
            Some(
                div()
                    .id("bench-ruler-render-progress")
                    .absolute()
                    .top_0()
                    .h(px(3.))
                    .left(px(x))
                    .w(px(w))
                    .bg(rgb(crate::ui::theme::tokens().ok)),
            )
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                this.scrubbing = true;
                this.clear_picks(cx);
                this.scrub_to(event.position.x, true, cx);
            }),
        )
        // The bench's own menu, on the strip that *is* the timeline:
        // walking the cuts, the zoom, the snap and the undo pair, none
        // of which is about a clip (DESIGN §9).
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                this.open_bench_menu(event.position, cx);
            }),
        )
        // Major ticks 8px with the label 4px to their right, minor ticks 4px halfway
        // between; a label that would sit under the playhead's head is left out.
        .children(ticks.iter().enumerate().flat_map(|(i, (x, label))| {
            let x = *x;
            let w = label.chars().count() as f32 * 7.2 + 6.;
            let under_head = x + 4. + w >= filled - 8. && x + 4. <= filled + 8.;
            // A flag stands at each marker: a label that would run under one yields to it.
            let under_flag = flags.iter().any(|(from, to)| x + 4. + w >= *from - 4. && x + 2. <= *to);
            let minor = ticks.get(i + 1).map(|(n, _)| x + (n - x) / 2.);
            let tick = div()
                .absolute()
                .bottom_0()
                .left(px(x))
                .w(px(1.))
                .h(px(8.))
                .bg(rgb(INK3()));
            let text = (!under_head && !under_flag).then(|| {
                div()
                    .absolute()
                    .top(px(3.))
                    .left(px(x + 3.))
                    .px(px(2.))
                    .rounded(px(2.))
                    // A plate in the ruler's own ground, so nothing drawn under the text
                    // (a range band, a neighbouring flag) cuts through it.
                    .bg(rgb(crate::ui::theme::tokens().e1))
                    .whitespace_nowrap()
                    .type_style(type_scale::caption())
                    .text_color(rgb(INK3()))
                    .child(label.clone())
            });
            let half = minor.map(|m| {
                div()
                    .absolute()
                    .bottom_0()
                    .left(px(m))
                    .w(px(1.))
                    .h(px(4.))
                    .bg(rgb(INK3()))
            });
            [Some(tick.into_any_element()), text.map(IntoElement::into_any_element), half.map(IntoElement::into_any_element)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
        }))
        .child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .left(px(filled - PLAYHEAD_HALO))
                .w(px(playhead_w + 2. * PLAYHEAD_HALO))
                .bg(rgba(DARK_SEAM())),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .left(px(filled))
                .w(px(playhead_w))
                .bg(rgb(LAMP_WHITE())),
        )
        // The lamp tab: the one part of the playhead a press can grab. Its
        // 24px box is centred on the playhead and sits inside the ruler, so a
        // press there bubbles up to the ruler's own left handler and scrubs
        // through that same path -- no second one to keep in step.
        .child(
            div()
                .absolute()
                .top(px(1.))
                .left(px(filled - crate::layout::HIT_MIN / 2.))
                .w(px(crate::layout::HIT_MIN))
                .h(px(crate::layout::HIT_MIN))
                .flex()
                .justify_center()
                .child(playhead_head()),
        )
        // The In and Out marks as brackets, facing in over the range they bound.
        .children(range_brackets(player.range, fps, scale, bed_w))
        // The comment markers: a flag in each one's colour on a strip along the ruler's foot,
        // its name beside it where there is room -- a point is a 3px post, a range runs from
        // its start to its end. Over the playhead's line, so a marker under it stays seen.
        .children(marker_marks(player, bed_w, cx))
        .when(player.scrubbing, |ruler| {
            // The playhead timecode plate, and ONLY while a scrub drag
            // is live: a drag is a state change, which is what a plate
            // is for. At rest the hero timecode leads the time band
            // (DESIGN §4) and the ledger carries position (§6.6); this
            // was the third resting copy of the same reading. Always
            // at `left_0`, never following the scrubbed x, so it never
            // overlaps the picture region above it.
            ruler.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(px(PLATE_W))
                    .flex()
                    .items_center()
                    .px(px(4.))
                    .bg(rgb(DARK_CANVAS()))
                    .type_style(type_scale::mono(
                        type_scale::CHORD_METADATA_MAX_PX,
                        gpui::FontWeight::MEDIUM,
                    ))
                    .text_color(rgb(INK1()))
                    .child(playhead_tc),
            )
        })
}

/// The playhead's head: a 12x14 pentagon in the lamp colour, point down.
fn playhead_head() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let o = bounds.origin;
            let pts = [(0., 0.), (12., 0.), (12., 8.), (6., 14.), (0., 8.)]
                .map(|(x, y)| point(o.x + px(x), o.y + px(y)));
            let mut path = gpui::PathBuilder::fill();
            path.add_polygon(&pts, true);
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(LAMP_WHITE()));
            }
        },
    )
    .w(px(12.))
    .h(px(14.))
}

/// The marker strip's marks as `(x, width, colour)` in the ruler's own pixels: a point is
/// [`MARK_POINT_W`] wide, a range as wide as it runs (never narrower than a point), and a
/// mark wholly off the bed is left out. Pure, so the test pins it without a window.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn marker_spans(
    markers: &[engine::project::Marker],
    fps: f64,
    scale: Scale,
    bed_w: f32,
) -> Vec<(f32, f32, engine::project::MarkerColor)> {
    if fps <= 0. || scale.pps <= 0. {
        return Vec::new();
    }
    markers
        .iter()
        .filter_map(|m| {
            let x = scale.px_at(f64::from(m.frame) / fps);
            let end = scale.px_at(f64::from(m.end()) / fps);
            let w = (end - x).max(MARK_POINT_W);
            (x <= bed_w && x + w >= 0.).then_some((x, w, m.color))
        })
        .collect()
}

/// A point marker's width on the ruler, in px.
pub(super) const MARK_POINT_W: f32 = 3.;

/// The marker strip's height at the ruler's foot, in px.
const MARK_H: f32 = 6.;

/// A marker's flag on the ruler: where it stands, the strip it covers, its colour and the name
/// that fits beside it (empty when there is no room).
pub(super) struct Flag {
    /// The marker's place in the session's list, the one a click on the flag names.
    pub(super) idx: usize,
    pub(super) x: f32,
    pub(super) w: f32,
    pub(super) color: engine::project::MarkerColor,
    pub(super) name: String,
}

impl Flag {
    /// Where the flag, and its name, end.
    fn reach(&self) -> f32 {
        let name = match self.name.is_empty() {
            true => 0.,
            false => self.name.chars().count() as f32 * char_w(type_scale::FLOOR_PX) + 8.,
        };
        self.x + (self.w).max(14.) + name
    }
}

/// The least room beside a flag worth a name, in px.
const FLAG_NAME_MIN_PX: f32 = 36.;

/// The flags in order along the ruler, each named as far as the room up to the next flag (or
/// the bed's end) allows: a name that does not fit whole is cut with an ellipsis, and one with
/// less than [`FLAG_NAME_MIN_PX`] left is left off. Pure, so a test pins it.
pub(super) fn marker_flags(markers: &[engine::project::Marker], fps: f64, scale: Scale, bed_w: f32) -> Vec<Flag> {
    if fps <= 0. || scale.pps <= 0. {
        return Vec::new();
    }
    let mut spans: Vec<_> = markers
        .iter()
        .enumerate()
        .filter_map(|(idx, m)| {
            let x = scale.px_at(f64::from(m.frame) / fps);
            let w = (scale.px_at(f64::from(m.end()) / fps) - x).max(MARK_POINT_W);
            (x <= bed_w && x + w >= 0.).then(|| (x, w, m.color, m.name.clone(), idx))
        })
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let ahead: Vec<f32> = spans.iter().skip(1).map(|s| s.0).chain([bed_w]).collect();
    spans
        .into_iter()
        .zip(ahead)
        .map(|((x, w, color, name, idx), next)| {
            let room = next - x.max(0.) - w.max(14.) - 8.;
            let per = char_w(type_scale::FLOOR_PX).max(1.);
            let name = match (name.is_empty(), room >= FLAG_NAME_MIN_PX) {
                (true, _) | (_, false) => String::new(),
                _ => {
                    let fit = (room / per).floor() as usize;
                    match name.chars().count() > fit {
                        true => name.chars().take(fit.saturating_sub(1)).collect::<String>().trim_end().to_string() + "\u{2026}",
                        false => name,
                    }
                }
            };
            Flag { idx, x, w, color, name }
        })
        .collect()
}

/// The width of a marker's hit area on the ruler, in px: wide enough to double-click, centred on
/// the flag.
const MARK_HIT_W: f32 = 14.;

fn marker_marks(player: &Player, bed_w: f32, cx: &mut Context<Player>) -> Vec<gpui::AnyElement> {
    let Some(session) = player.session.as_ref() else {
        return Vec::new();
    };
    // The name being retyped, if it is a marker's name: that marker's flag shows the field
    // in its name's place, so the rename happens on the ruler and nowhere else.
    let editing = player
        .marker_edit
        .as_ref()
        .filter(|e| e.field == MarkerField::Name)
        .cloned();
    marker_flags(session.markers(), player.fps, player.scale, bed_w)
        .into_iter()
        .map(|Flag { idx, x, w, color, name }| {
            // A flag in the marker's colour standing on the strip, so a marker reads at a
            // glance; the strip under it is the range it covers.
            // The hit area is centred on the flag; the flag sits inside it at its own offset.
            let hit_w = w.max(MARK_HIT_W);
            let hit_x = x + w / 2. - hit_w / 2.;
            let retyped = editing.as_ref().filter(|e| e.idx == idx);
            let flag = div()
                .absolute()
                .bottom_0()
                .left(px(x - hit_x))
                .w(px(w))
                .h(px(MARK_H))
                .bg(rgb(marker_rgb(color)))
                .child(
                    div()
                        .absolute()
                        .bottom(px(MARK_H - 1.))
                        .left(px(-3.))
                        .child(crate::ui::icons::Icon::FlagTriangleRight.el(crate::ui::icons::IconSize::Inline, marker_rgb(color))),
                )
                .when(!name.is_empty() && retyped.is_none(), |d| {
                    d.child(
                        div()
                            .absolute()
                            .bottom(px(MARK_H))
                            .left(px(w.max(14.) - 2.))
                            .px(px(3.))
                            .rounded(px(2.))
                            .bg(rgb(crate::ui::theme::tokens().e1))
                            .whitespace_nowrap()
                            .type_style(type_scale::caption())
                            .text_color(rgb(crate::ui::theme::tokens().ink1))
                            .child(name),
                    )
                });
            // The flag's own hit area, a little wider than the flag and the ruler's full height:
            // a right-click names the marker (its menu), a double-click renames it. Its
            // right-click stops here, so the bench's menu does not open under it.
            div()
                .id(("ruler-marker", idx))
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(hit_x))
                .w(px(hit_w))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.open_marker_menu(idx, event.position, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    // A double-click inside the open field is the caret's, not a new rename.
                    let typing = this.marker_edit.as_ref().is_some_and(|e| e.idx == idx && e.field == MarkerField::Name);
                    if event.click_count() >= 2 && !typing {
                        this.rename_marker_at(idx, cx);
                    }
                }))
                .child(flag)
                // The name's field, where the name was: the same field the Markers card's
                // rename was, its words all selected. A press in it stays in it -- it does not
                // move the playhead or start a new rename.
                .when_some(retyped, |d, e| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left(px(x - hit_x + w.max(14.) - 2.))
                            .on_mouse_down(MouseButton::Left, crate::interact::swallow)
                            .child(crate::ui::toolbar::seq_name_field(&crate::player::text_field::TextField {
                                text: e.text.clone(),
                                all: e.all,
                            })),
                    )
                })
                .into_any_element()
        })
        .collect()
}

/// The In and Out marks as brackets on the ruler: a 2px bar with two short arms, the In
/// opening to the right and the Out to the left, in the accent.
fn range_brackets(range: Option<(u32, u32)>, fps: f64, scale: Scale, bed_w: f32) -> Vec<gpui::AnyElement> {
    let Some((a, b)) = range else {
        return Vec::new();
    };
    let ink = rgb(crate::ui::theme::tokens().accent);
    let at = |frame: u32| scale.px_at(f64::from(frame) / fps);
    [(at(a), true), (at(b), false)]
        .into_iter()
        .filter(|(x, _)| *x >= 0. && *x <= bed_w)
        .map(|(x, opens_right)| {
            let arm = |top: bool| {
                let d = div().absolute().h(px(2.)).w(px(5.)).bg(ink);
                let d = if top { d.top_0() } else { d.bottom_0() };
                if opens_right { d.left_0() } else { d.right_0() }
            };
            div()
                .absolute()
                .top(px(1.))
                .bottom(px(1.))
                .left(px(if opens_right { x - 1. } else { x - 4. }))
                .w(px(5.))
                .child(div().absolute().top_0().bottom_0().w(px(2.)).bg(ink).when(opens_right, |d| d.left_0()).when(!opens_right, |d| d.right_0()))
                .child(arm(true))
                .child(arm(false))
                .into_any_element()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_is_a_post_and_a_range_runs_its_length() {
        use engine::project::{Marker, MarkerColor};
        let m = |frame, length| Marker { frame, length, name: "m".into(), note: String::new(), color: MarkerColor::Red };
        let scale = Scale { pps: 10., start: 0. };
        let spans = marker_spans(&[m(25, 0), m(50, 50), m(25 * 100, 0)], 25., scale, 200.);
        assert_eq!(spans.len(), 2, "a marker past the bed is left out");
        assert_eq!((spans[0].0, spans[0].1), (10., MARK_POINT_W));
        assert_eq!((spans[1].0, spans[1].1), (20., 20.));
        assert!(marker_spans(&[m(25, 0)], 25., Scale { pps: 0., start: 0. }, 200.).is_empty());
    }

    #[test]
    fn a_flag_carries_its_name_where_there_is_room_and_cuts_it_otherwise() {
        use engine::project::{Marker, MarkerColor};
        let m = |frame, name: &str| Marker { frame, length: 0, name: name.into(), note: String::new(), color: MarkerColor::Red };
        let scale = Scale { pps: 100., start: 0. };
        let flags = marker_flags(&[m(25, "Intro"), m(75, "A very long marker name"), m(78, "x")], 25., scale, 400.);
        assert_eq!(flags[0].name, "Intro");
        assert!(flags[1].name.is_empty(), "no room before the next flag");
        assert_eq!(flags.len(), 3);
        let cut = marker_flags(&[m(25, "A very long marker name here")], 25., Scale { pps: 40., start: 0. }, 100.);
        assert!(cut[0].name.ends_with('\u{2026}'), "{:?}", cut[0].name);
    }

    #[test]
    fn a_short_clip_at_25fps_reads_in_seconds_and_frames_not_timecodes() {
        // The audit's 2s clip: fitted to a 1200px bed it sits at ~600px/s,
        // where the finest step is five frames (120px). The `2s` tick at the
        // bed's very edge has no room for its label and is left out.
        let scale = Scale { pps: 600., start: 0. };
        let labels: Vec<_> = ticks(scale, 25., 1200., 0.).into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels, ["0s", "+5f", "+10f", "+15f", "+20f", "1s", "+5f", "+10f", "+15f", "+20f"]);
        // At the default 60px/s the same bed reads in whole seconds.
        let scale = Scale { pps: 60., start: 0. };
        let labels: Vec<_> = ticks(scale, 25., 1200., 0.).into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels[..4], ["0s", "2s", "4s", "6s"]);
        assert!(labels.iter().all(|l| !l.contains(':')), "{labels:?}");
        // Scrolled past a minute at 4px/s, the labels turn to M:SS.
        let scale = Scale { pps: 4., start: 50. };
        let labels: Vec<_> = ticks(scale, 25., 1200., 0.).into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels[..3], ["1:00", "1:30", "2:00"]);
        // The scrub plate's band and a zero scale give no ticks there.
        assert!(ticks(scale, 25., 1200., 2000.).is_empty());
        assert!(ticks(Scale { pps: 0., start: 0. }, 25., 1200., 0.).is_empty());
    }
}

#[cfg(test)]
mod range_band_tests {
    use super::*;

    #[test]
    fn the_in_out_range_is_shaded_between_its_marks_and_clipped_to_the_bed() {
        let scale = crate::viewport::Scale { pps: 100., start: 0. };
        assert_eq!(range_band(None, 25., scale, 500.), None);
        assert_eq!(range_band(Some((25, 75)), 25., scale, 500.), Some((100., 200.)));
        assert_eq!(range_band(Some((25, 1000)), 25., scale, 500.), Some((100., 400.)));
        assert_eq!(range_band(Some((2500, 3000)), 25., scale, 500.), None);
    }
}
