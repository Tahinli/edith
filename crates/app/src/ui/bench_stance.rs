//! The bench's own content: lanes, ruler and clips (DESIGN.md §5's clip
//! anatomy, §7's degradation ladder). `stance.rs::bench()` owns the frame
//! (height, surfaces, section head); this module owns what fills it, the way
//! `dock_stance.rs` owns the dock's content next to `stance.rs::dock()`'s
//! frame.
//!
//! Reused rather than reimplemented (`ui/timeline.rs` already shipped this
//! plumbing on the legacy timeline): `Player::insert_source` and the
//! `AssetDrag` payload for add-at-playhead and drag-drop from the Sources
//! tab, `Player::pick`/`Player::marks`/`marked` for selection, and
//! `Player::nudge_cut`/`walk_cut`/`cut`/`toggle_loop_trim` (DESIGN §6, wired
//! in `player/actions.rs` already, and answered to by every keystroke the
//! stance's own key handler forwards) for the whole cut grammar -- none of
//! that is touched here, because a clip's `start`/`frames()` already reflect
//! a commit the moment the keymap makes one (`nudge_cut` writes straight to
//! the session), so this module reads the session straight, needing no live
//! drag-preview state to mirror it. New here: the clip anatomy (ink spine,
//! trace, name plate, splice gaps) and the width ladder that degrades it
//! (DESIGN §7) -- neither existed on the legacy timeline in this shape.
//!
//! Not reused, deferred: `ClipDrag` (moving a placed clip to another lane by
//! hand), the mouse edge-trim strips, fades/dissolve glyphs and the lane
//! header reorder drag. The cut grammar (`,` `.` `[` `]` `/` `s`) never needs
//! any of them -- `nudge_cut` and friends act on `Player::selected` directly,
//! with no element under the pointer required -- so they are out of this
//! slice's reach; `Player::pick` (click-select) and the asset drop (place)
//! are the two pointer gestures the charter actually asked this surface to
//! answer for.

use crate::*;
use crate::ui::type_scale::{self, Typeset};

/// The pinned ruler's own height, above the lane stack -- tall enough for a
/// tick line plus a mono `MM:SS` label under it (DESIGN §5's "tick marks with
/// mono ink3 timecodes").
const RULER_H: f32 = 22.;
/// The stops a tick interval is picked from (DESIGN §5, the previous
/// builder's own note): the smallest one whose pixel width at the current
/// zoom still clears [`TICK_MIN_PX`].
const TICK_STOPS: [f64; 13] = [
    0.5, 1., 2., 5., 10., 15., 30., 60., 120., 300., 600., 1800., 3600.,
];
/// The floor a tick's pixel spacing must clear before its label is legible
/// mono text at 8px.
const TICK_MIN_PX: f64 = 64.;
/// The playhead timecode plate's own width, wide enough for `HH:MM:SS:FF`
/// mono at 10px plus the plate's padding.
const PLATE_W: f32 = 74.;
/// The pinned track-head column, DESIGN §5's "track heads" -- narrower than
/// the legacy timeline's `HEADER_W` since a darkroom lane carries no mix/eye
/// button yet (deferred with the rest of the header's verbs).
const HEAD_W: f32 = 28.;
const ROW_GAP: f32 = 2.;
/// DESIGN §7: lanes compress evenly up to this many rows before the column
/// scrolls behind the pinned ruler and heads instead of compressing further.
const LANES_COMPRESS: usize = 5;
const LANE_FULL_H: f32 = 40.;
const LANE_MIN_H: f32 = 18.;

/// One clip's degradation tier (DESIGN §7), picked off the clip's own worth
/// in pixels (`span`, unclamped) rather than its drawn floor
/// ([`clip_width`]'s `HIT_MIN`) -- a clip zoomed far out is worth a fraction
/// of a pixel long before its box stops shrinking, and the ladder has to see
/// that or "sliver" would never fire under the floor that keeps it clickable.
#[derive(Clone, Copy, PartialEq)]
enum Tier {
    /// Trace + name plate + speed chip.
    Full,
    /// Trace + name plate.
    NoChip,
    /// Trace only -- DESIGN §7's explicit "<48px" boundary.
    NoLabel,
    /// Spine + trace only, thinned.
    SpineTrace,
    /// Sliver fill + splice gaps: film scale.
    Sliver,
}

/// `MM:SS` for a clip's own readout -- the bench talks in the film's units,
/// not in seconds with a decimal point.
fn mmss(secs: f64) -> String {
    let s = secs.max(0.).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `MM:SS` zero-padded on both fields, the ruler's own tick label (mock:
/// `00:30`, `01:00`, `01:30`) -- distinct from [`mmss`]'s unpadded minutes,
/// which reads right in a trim delta but not lined up under evenly spaced
/// ticks.
fn tick_mmss(secs: f64) -> String {
    let s = secs.max(0.).round() as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// The tick spacing at `pps` pixels per second: the smallest [`TICK_STOPS`]
/// entry whose pixel width still clears [`TICK_MIN_PX`], so labels never
/// crowd into soup as the bed zooms in and never thin to nothing zoomed out
/// (the far stop, an hour, is the last one there is).
fn tick_interval(pps: f64) -> f64 {
    TICK_STOPS
        .iter()
        .copied()
        .find(|&i| i * pps >= TICK_MIN_PX)
        .unwrap_or(3600.)
}

/// A clip's thumbnail, filling its box. `Cover` rather than `Contain`: a clip
/// is a strip of film, so it crops rather than letterboxing inside a lane.
fn cover_image(image: std::sync::Arc<gpui::RenderImage>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let fitted = gpui::ObjectFit::Cover.get_bounds(bounds, image.size(0));
            let _ = window.paint_image(fitted, Corners::default(), image, 0, false);
        },
    )
    .size_full()
}

fn tier(span: f32) -> Tier {
    match span {
        s if s >= 120. => Tier::Full,
        s if s >= 48. => Tier::NoChip,
        s if s >= 24. => Tier::NoLabel,
        s if s >= 10. => Tier::SpineTrace,
        _ => Tier::Sliver,
    }
}

/// The lane stack's own height budget, split evenly across up to
/// [`LANES_COMPRESS`] rows before the column scrolls instead (DESIGN §7).
fn row_h(lanes: usize, box_h: f32) -> f32 {
    let n = lanes.clamp(1, LANES_COMPRESS) as f32;
    ((box_h - (n - 1.) * ROW_GAP) / n).clamp(LANE_MIN_H, LANE_FULL_H)
}

/// A clip's ink spine + body, degrading by [`Tier`] (DESIGN §5, §7). A single
/// placeholder ink stands in for every source until real extraction lands.
///
/// hook: §12 step 5 -- per-source film ink attaches here, replacing
/// `INK2()` with the source's own quantized hue.
fn clip_box(
    player: &Player,
    lane: Lane,
    idx: usize,
    clip: &Clip,
    scale: Scale,
    picks: &[(Lane, usize)],
    pick_links: &[Option<u32>],
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let (start, len) = (
        f64::from(clip.start) / player.fps,
        f64::from(clip.frames()) / player.fps,
    );
    let span = scale.width_px(len);
    let width = clip_width(span);
    let left = scale.px_at(start);
    let on = marked((lane, idx), clip.link, picks, pick_links);
    let t = tier(span);
    let source = player.sources().get(clip.source);
    let label = source.map(|s| file_name(&s.path));
    let audio = lane.kind == LaneKind::Audio;
    let wave = source.and_then(|s| player.waves.get(&(s.path.clone(), s.audio_stream))).cloned();
    // hook: §12 step 5 -- per-source film ink attaches here, replacing
    // `source_tint` (library_meta.rs's own placeholder wheel, index-keyed and
    // already different per source) with the source's own quantized hue.
    let ink = source_tint(clip.source);
    let thumb = (!audio)
        .then(|| source.and_then(|s| player.thumbs.get(&s.path)))
        .flatten()
        .cloned();
    let has_trace = t != Tier::Sliver;
    let has_spine = t != Tier::Sliver;
    let has_label = matches!(t, Tier::Full | Tier::NoChip);
    let has_chip = t == Tier::Full && !clip.speed.is_normal();
    let (in_frame, out_frame) = (f64::from(clip.in_frame) / player.fps, f64::from(clip.out_frame) / player.fps);
    let speed = clip.speed;
    // Right-aligned readout: the trim delta off the source's full length when
    // this clip is shorter than the file it was cut from, else the plain
    // duration (DESIGN §5). corner-cut: reads only against the *source's* full
    // length, not the subject cut's own out/in marks the time band's readout
    // means (§12's cut object has no per-edge history at this layer) --
    // ceiling is wiring this to the same cut state once the time band exposes
    // it, rather than a second reading of "trim" here.
    let full_frames = source.map(|s| player.session.as_ref().map_or(0, |sess| sess.file_frames(&s.path)));
    let readout = match full_frames {
        Some(full) if full > clip.frames() => {
            let delta = f64::from(full - clip.frames()) / player.fps;
            format!("−{}", mmss(delta))
        }
        _ => format!("{len:.1}s"),
    };
    div()
        .id(("bench-clip", lane.ord * 1000 + lane.kind as usize * 100 + idx))
        .absolute()
        .top_0()
        .h_full()
        .left(px(left))
        .w(px(width))
        .overflow_hidden()
        .rounded(px(0.))
        .bg(rgb(DARK_PANEL()))
        // Focus/selection ring, 1px `ink1`, lamp-adjacent and never coloured
        // (DESIGN §4).
        .when(on, |d| d.border_1().border_color(rgb(INK1())))
        .cursor_pointer()
        // Selection: reuses `Player::pick`, the same call the legacy
        // timeline's clip box makes.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.pick((lane, idx), event.modifiers.control, cx);
            }),
        )
        // Ink spine: 3px left edge, the source's own ink (hook above).
        .when(has_spine, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .h_full()
                    .w(px(3.))
                    .bg(rgb(ink)),
            )
        })
        // Trace: real waveform for audio, a real decoded thumbnail for video
        // (falling back to a flat placeholder body while it loads or if the
        // file never yields one).
        .when(has_trace && audio, |d| {
            d.children(wave.and_then(|w| match w {
                Wave::Peaks(peaks) => Some(
                    div()
                        .absolute()
                        .left(px(3.))
                        .right_0()
                        .top_0()
                        .bottom_0()
                        .child(waveform_ink(peaks, in_frame, out_frame, ink)),
                ),
                _ => None,
            }))
        })
        .when(has_trace && !audio, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(3.))
                    .right_0()
                    .top(px(2.))
                    .bottom(px(2.))
                    .overflow_hidden()
                    .bg(rgb(INK4()))
                    .when_some(thumb, |d, thumb| match thumb {
                        Thumb::Ready(image) => d.child(cover_image(image)),
                        _ => d,
                    }),
            )
        })
        // Name plate + trim/duration readout: one strip, DESIGN §4's plate
        // (canvas-on-panel), sat at the clip's top-left/top-right per the mock
        // rather than the name alone hanging below the box.
        .when(has_label, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(3.))
                    .right_0()
                    .top_0()
                    .h(px(13.))
                    .px(px(3.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(4.))
                    .bg(rgba(DARK_SEAM()))
                    .when_some(label, |d, label| {
                        d.child(
                            div()
                                .min_w(px(0.))
                                .truncate()
                                .type_style(type_scale::mono(
                                    type_scale::CHORD_METADATA_MIN_PX,
                                    gpui::FontWeight::MEDIUM,
                                ))
                                .text_color(rgb(INK1()))
                                .child(label),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .type_style(type_scale::mono(9., gpui::FontWeight::MEDIUM))
                            .text_color(rgb(INK3()))
                            .child(readout),
                    ),
            )
        })
        // Speed chip: dropped first in the ladder, tucked in the bottom-right
        // corner so it never fights the name/readout strip above.
        .when(has_chip, |d| {
            d.child(
                div()
                    .absolute()
                    .bottom_0()
                    .right_0()
                    .px(px(3.))
                    .bg(rgb(DARK_RAISED()))
                    .type_style(type_scale::mono(9., gpui::FontWeight::MEDIUM))
                    .text_color(rgb(INK2()))
                    .child(format!("{speed}")),
            )
        })
        // Splice gap: the lamp-white sliver at every clip's trailing edge --
        // DESIGN §1 law 3's other legal white, present at every tier (a
        // sliver at film scale is spine gone too, so the splice is what is
        // left to read).
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .h_full()
                .w(px(1.))
                .bg(rgb(LAMP_WHITE())),
        )
}

/// One lane row: pinned head + bed of clips, DESIGN §5's "lanes V/A/S", drawn
/// at the compressed height [`row_h`] gives it.
fn lane_row(
    player: &Player,
    lane: Lane,
    h: f32,
    scale: Scale,
    picks: &[(Lane, usize)],
    pick_links: &[Option<u32>],
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let clips: Vec<Clip> = player
        .session
        .as_ref()
        .map_or(&[][..], |s| s.lane_clips(lane))
        .to_vec();
    let boxes: Vec<_> = clips
        .iter()
        .enumerate()
        .map(|(idx, clip)| clip_box(player, lane, idx, clip, scale, picks, pick_links, cx))
        .collect();
    // DESIGN §5's lane heads: "a small ink dot under each head, coloured by
    // the lane's source ink" -- the first clip's source stands for the lane,
    // since a lane with several sources still needs one dot, not a legend.
    let dot = clips
        .first()
        .map_or_else(INK4, |clip| source_tint(clip.source));
    div()
        .id(("bench-lane", lane.ord * 10 + lane.kind as usize))
        .flex_none()
        .h(px(h))
        .flex()
        .child(
            // Pinned track head.
            div()
                .flex_none()
                .w(px(HEAD_W))
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(2.))
                .bg(rgb(DARK_PANEL()))
                // MOCK-SPEC.md "Bench": "V1, A1 in mono".
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK2()))
                .child(lane.label())
                .child(div().flex_none().w(px(4.)).h(px(4.)).rounded(px(2.)).bg(rgb(dot))),
        )
        .child(
            // The bed: a drop target for the Sources tab (`AssetDrag`,
            // reused from `ui/timeline.rs`) and every clip on it.
            div()
                .id(("bench-bed", lane.ord * 10 + lane.kind as usize))
                .relative()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .bg(rgb(DARK_CANVAS()))
                .border_l_1()
                .border_color(rgba(DARK_SEAM()))
                .drag_over::<AssetDrag>(|d, _, _, _| d.bg(rgb(DARK_RAISED())))
                .on_drop(cx.listener(move |this, drag: &AssetDrag, window, cx| {
                    let at = this.place_frame(window.mouse_position().x).0;
                    this.insert_source(&drag.0.clone(), drag.1, Some(lane), Some(at), cx)
                }))
                .children(boxes),
        )
}

/// The bench's content: pinned ruler, pinned heads, lane stack -- compressed
/// evenly to [`LANES_COMPRESS`] rows and then scrolling (DESIGN §7).
pub(crate) fn render(player: &mut Player, box_h: f32, cx: &mut Context<Player>) -> impl IntoElement {
    let lanes = player
        .session
        .as_ref()
        .map_or_else(|| vec![Lane::V1, Lane::A1], PlaybackSession::lanes);
    let position = player.active_session().map_or(0., PlaybackSession::now);
    let scale = player.scale;
    let bed_w = f32::from(player.ruler.get().size.width).max(1.);
    let filled = scale.px_at(position).clamp(0., bed_w);
    let (picks, pick_links) = player.marks();
    let h = row_h(lanes.len(), (box_h - RULER_H - ROW_GAP).max(LANE_MIN_H));
    let mut rows = Vec::new();
    for &lane in &lanes {
        rows.push(lane_row(player, lane, h, scale, &picks, &pick_links, cx));
    }
    // Ruler ticks (DESIGN §5): the interval is the smallest [`TICK_STOPS`]
    // entry whose pixel spacing still clears [`TICK_MIN_PX`], walked from the
    // first tick at or after the bed's left edge to the bed's right edge.
    // Guarded on `pps > 0` -- a zero scale (no session yet) has no interval
    // that ever advances past the bed's own width, which would loop forever.
    let mut ticks = Vec::new();
    if scale.pps > 0. {
        let interval = tick_interval(scale.pps);
        let mut t = (scale.start / interval).ceil() * interval;
        while scale.px_at(t) <= bed_w {
            ticks.push((scale.px_at(t).max(0.), tick_mmss(t)));
            t += interval;
        }
    }
    let playhead_tc = crate::viewport::timecode(position, player.fps);
    div()
        .id("bench-content")
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(ROW_GAP))
        .child(
            // Pinned ruler: click/drag to seek (reuses `Player::scrub_to`,
            // the same call the legacy ruler makes), tick marks with mono
            // `ink3` timecodes (DESIGN §5), and the playhead's own lamp-white
            // line (DESIGN §1 law 3 -- the only other legal use of pure
            // white besides the splice).
            div()
                .id("bench-ruler")
                .flex_none()
                .h(px(RULER_H))
                .ml(px(HEAD_W))
                .relative()
                .rounded(px(2.))
                .bg(rgb(DARK_PANEL()))
                .cursor_pointer()
                .child(bounds_probe(player.ruler.clone()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        this.scrubbing = true;
                        this.scrub_to(event.position.x, true, cx);
                    }),
                )
                .children(ticks.into_iter().map(|(x, label)| {
                    div()
                        .absolute()
                        .top_0()
                        .left(px(x))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_start()
                                .child(div().w(px(1.)).h(px(4.)).bg(rgb(INK3())))
                                .child(
                                    div()
                                        .type_style(type_scale::mono(
                                            type_scale::FLOOR_PX,
                                            gpui::FontWeight::MEDIUM,
                                        ))
                                        .text_color(rgb(INK3()))
                                        .child(label),
                                ),
                        )
                }))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .left(px(filled))
                        .w(px(1.))
                        .bg(rgb(LAMP_WHITE())),
                )
                .child(
                    // The pinned playhead timecode plate, DESIGN §5's own
                    // "playhead's own timecode in a plate at the left edge" --
                    // always at `left_0`, never following the scrubbed x, so
                    // it never overlaps the picture region above it.
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
                ),
        )
        .child(
            div()
                .id("bench-lanes")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .track_scroll(&player.lanes_scroll)
                .flex()
                .flex_col()
                .gap(px(ROW_GAP))
                .children(rows),
        )
}
