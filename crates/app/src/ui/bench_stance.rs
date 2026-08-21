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
//! Now also reused: `ClipDrag` (moving a placed clip along its lane and onto
//! another, `Player::dragged`/`Player::move_clip`), the mouse edge-trim
//! strips (`Player::start_trim`/`trims(span)`, the same minimum-width rule
//! the legacy strips use), `Player::timeline_wheel` (ctrl+wheel zooms about
//! the pointer, a bare wheel scrolls the bed -- the same mapping on the
//! ruler and on every bed) and `LaneDrag`/`Player::reorder_lane` (drag a
//! track head onto another to swap their order). `Player::drag_move`/
//! `Player::drag_release`, wired once on the window's own root
//! (`render.rs`), already carry a trim or a scroll off the 6px strip that
//! started it, for the darkroom tree exactly as for the legacy one -- no
//! root wiring lives here.
//!
//! Still deferred: fades/dissolve glyphs, and the lane header's own
//! drop-line cue (the legacy `LaneDrop` preview) -- a plain drag_over
//! highlight stands in for it here, since the cue itself is display-only and
//! `reorder_lane` at the release does not need it.
//!
//! `nudge_cut` and friends still act on `Player::selected` directly, with no
//! element under the pointer required, and stay untouched by any of this.

use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::*;
use crate::*;

/// The pinned ruler's own height, above the lane stack -- tall enough for a
/// tick line plus a mono `MM:SS` label under it (DESIGN §5's "tick marks with
/// mono ink3 timecodes").
pub(crate) const RULER_H: f32 = 22.;
/// The stops a tick interval is picked from (DESIGN §5, the previous
/// builder's own note): the smallest one whose pixel width at the current
/// zoom still clears [`TICK_MIN_PX`].
const TICK_STOPS: [f64; 13] = [
    0.5, 1., 2., 5., 10., 15., 30., 60., 120., 300., 600., 1800., 3600.,
];
/// The floor a tick's pixel spacing must clear before its label is legible
/// mono text at 10px.
const TICK_MIN_PX: f64 = 64.;
/// The playhead timecode plate's own width: eleven mono characters at the
/// 14px readout role plus its horizontal padding and a 1px rounding guard,
/// derived from the shared list-character calibration so the plate grows with
/// its type scale.
const PLATE_W: f32 = 11. * crate::layout::LIST_CHAR_W / type_scale::CHORD_METADATA_MIN_PX
    * type_scale::CHORD_METADATA_MAX_PX
    + 9.;
/// The pinned track-head column: wide enough for its lane-specific ghost
/// verbs and their compact chords without shrinking their pointer targets.
const HEAD_W: f32 = 72.;
pub(crate) const ROW_GAP: f32 = 2.;
/// DESIGN §7: lanes compress evenly up to this many rows before the column
/// scrolls behind the pinned ruler and heads instead of compressing further.
const LANES_COMPRESS: usize = 5;
const LANE_FULL_H: f32 = 40.;
/// The gap `lane_row`'s pinned head column puts between the lane label and
/// its status dot (its own `.gap(px(...))`), named so [`LANE_MIN_H`]'s
/// derivation below doesn't repeat the literal blind.
pub(crate) const LANE_HEAD_GAP: f32 = 2.;
/// The status dot's own diameter (`lane_row`'s "Pinned track head" child,
/// its own `.w(px(...)).h(px(...))`).
pub(crate) const LANE_DOT_D: f32 = 4.;
/// The lane label's own line box at [`type_scale::CHORD_METADATA_MIN_PX`]:
/// gpui's default `TextStyle::line_height` is not 1x the font size but the
/// golden ratio (`gpui::phi()` == `relative(1.618034)`), and `lane_row`
/// never calls `.line_height()` on the head label to override it -- so this
/// is what the label really occupies, not its glyph height.
/// `round(13. * 1.618034) = 21`.
const LANE_LABEL_LINE_H: f32 = 21.;
/// The least a lane row may be: what its own head actually draws, not a
/// number copied from nowhere. The old `18.` fit only the label's line box
/// and let every lane's status dot overflow into the row beneath it --
/// invisible everywhere but the last lane, which has no next row to spill
/// into and clipped straight into the ledger instead (adversarial pass on
/// the `BENCH_MIN_H = 82` floor).
pub(crate) const LANE_MIN_H: f32 = LANE_LABEL_LINE_H + LANE_HEAD_GAP + LANE_DOT_D;

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
pub(crate) fn row_h(lanes: usize, box_h: f32) -> f32 {
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
    // The placed clip, for the drag payload -- and the clip as an in-flight
    // edge trim is showing it, for the box (`Player::trimmed`, the same
    // split `ui/timeline.rs`'s own clip box makes).
    let placed = *clip;
    let clip = &player.trimmed(lane, idx, placed);
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
    let wave = source
        .and_then(|s| player.waves.get(&(s.path.clone(), s.audio_stream)))
        .cloned();
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
    let (in_frame, out_frame) = (
        f64::from(clip.in_frame) / player.fps,
        f64::from(clip.out_frame) / player.fps,
    );
    let speed = clip.speed;
    // Right-aligned readout: the trim delta off the source's full length when
    // this clip is shorter than the file it was cut from, else the plain
    // duration (DESIGN §5). corner-cut: reads only against the *source's* full
    // length, not the subject cut's own out/in marks the time band's readout
    // means (§12's cut object has no per-edge history at this layer) --
    // ceiling is wiring this to the same cut state once the time band exposes
    // it, rather than a second reading of "trim" here.
    let full_frames = source.map(|s| {
        player
            .session
            .as_ref()
            .map_or(0, |sess| sess.file_frames(&s.path))
    });
    let readout = match full_frames {
        Some(full) if full > clip.frames() => {
            let delta = f64::from(full - clip.frames()) / player.fps;
            format!("−{}", mmss(delta))
        }
        _ => format!("{len:.1}s"),
    };
    // For the drag ghost and the fallback label both -- kept before `label`
    // is consumed by the name plate below.
    let ghost: SharedString = label.clone().unwrap_or_else(|| lane.label()).into();
    div()
        .id((
            "bench-clip",
            lane.ord * 1000 + lane.kind as usize * 100 + idx,
        ))
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
        // The right button selects exactly as the left one does -- the menu
        // acts on the clip it names -- then hangs the clip menu at the
        // pointer (DESIGN §9's "verbs of the thing under the cursor"). Same
        // call and same surface the legacy timeline's clip box opens
        // (`ui/timeline.rs`'s `Player::open_menu`; the card itself is
        // `context_card`, already mounted at `stance.rs:695`).
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.open_menu(lane, idx, event.position, cx);
            }),
        )
        // Dragged, it moves: to the frame and the lane it is let go over
        // (`Player::move_clip`, `ClipDrag`) -- the same payload and the same
        // ghost tooltip `ui/timeline.rs`'s own clip box drags.
        .on_drag(
            ClipDrag {
                lane,
                idx,
                clip: placed,
            },
            {
                let ghost = ghost.clone();
                move |_, _, _, cx| cx.new(|_| Tip(ghost.clone()))
            },
        )
        // The two edge strips a drag lengthens or shortens the clip by
        // (`Player::start_trim`), gated on the same `trims(span)` floor the
        // legacy strips use -- a clip too narrow to aim at keeps its whole
        // padded box as a body to select and drag by instead (the `[`/`]`
        // chords still trim it either way).
        .children(
            [Edge::Start, Edge::End]
                .into_iter()
                .filter(|_| trims(span))
                .map(|edge| {
                    let mut zone = div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(px(EDGE_W))
                        .occlude()
                        .cursor(CursorStyle::ResizeLeftRight)
                        .hover(|s| s.bg(rgb(INK1())))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.start_trim(lane, idx, edge, event.modifiers.control, cx);
                            }),
                        )
                        // Occluded, so the box's own right-button listener
                        // never fires here: the same menu, opened by the same
                        // call, exactly as the legacy clip's edge strip does.
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.open_menu(lane, idx, event.position, cx);
                            }),
                        );
                    zone = match edge {
                        Edge::Start => zone.left_0(),
                        Edge::End => zone.right_0(),
                    };
                    zone
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
            d.children(wave.and_then(|w| {
                match w {
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
                }
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
        // rather than the name alone hanging below the box. Its 21px height is
        // the metadata role's default phi() line box, not its 13px glyph size.
        .when(has_label, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(3.))
                    .right_0()
                    .top_0()
                    .h(px(LANE_LABEL_LINE_H))
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
                            .type_style(type_scale::mono(
                                type_scale::FLOOR_PX,
                                gpui::FontWeight::MEDIUM,
                            ))
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
                    .type_style(type_scale::mono(
                        type_scale::FLOOR_PX,
                        gpui::FontWeight::MEDIUM,
                    ))
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

/// A placed caption's box, on a subtitle lane: [`clip_box`]'s twin, minus the
/// thumbnail/waveform trace a caption has none of (DESIGN §6 -- "the subtitle
/// cue edge IS a cut", so the same selection ring, drag, edge-trim and
/// right-click menu a clip gets apply here too). Reused rather than
/// reimplemented: `Player::trimmed_sub`/`SubDrag`/`sub_pick_name`/`cue_box`,
/// the same plumbing `ui/timeline.rs`'s own caption box already drives.
fn sub_box(
    player: &Player,
    lane: Lane,
    idx: usize,
    sub: &SubClip,
    scale: Scale,
    picks: &[(Lane, usize)],
    pick_links: &[Option<u32>],
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let placed = *sub;
    let shown = player.trimmed_sub(lane, idx, placed);
    let (start, len) = (
        f64::from(shown.start) / player.fps,
        f64::from(shown.frames) / player.fps,
    );
    let span = scale.width_px(len);
    let width = clip_width(span);
    let left = scale.px_at(start);
    let on = marked((lane, idx), placed.link, picks, pick_links);
    // Where in this window the track's own cues actually fall
    // (`PlaybackSession::sub_lane_cues`, the same map the export and the
    // picture's own plate go through).
    let cues: Vec<(f32, f32)> = player.session.as_ref().map_or_else(Vec::new, |s| {
        s.sub_lane_cues(lane)
            .iter()
            .map(|cue| cue_box(scale, cue))
            .collect()
    });
    let label = player
        .session
        .as_ref()
        .and_then(|s| sub_pick_name(s.subtitles(), placed.track));
    let ghost: SharedString = label.unwrap_or_else(|| lane.label()).into();
    div()
        .id(("bench-sub", lane.ord * 1000 + idx))
        .absolute()
        .top_0()
        .h_full()
        .left(px(left))
        .w(px(width))
        .overflow_hidden()
        .rounded(px(0.))
        .bg(rgb(DARK_PANEL()))
        .when(on, |d| d.border_1().border_color(rgb(INK1())))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.pick((lane, idx), event.modifiers.control, cx);
            }),
        )
        // Same door a clip's box opens its menu by (DESIGN §9).
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.open_menu(lane, idx, event.position, cx);
            }),
        )
        .on_drag(
            SubDrag {
                lane,
                idx,
                sub: placed,
            },
            {
                let ghost = ghost.clone();
                move |_, _, _, cx| cx.new(|_| Tip(ghost.clone()))
            },
        )
        // The same edge-trim strips a clip gets, on the same [`trims`] floor.
        .children(
            [Edge::Start, Edge::End]
                .into_iter()
                .filter(|_| trims(span))
                .map(|edge| {
                    let mut zone = div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(px(EDGE_W))
                        .occlude()
                        .cursor(CursorStyle::ResizeLeftRight)
                        .hover(|s| s.bg(rgb(INK1())))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.start_trim(lane, idx, edge, event.modifiers.control, cx);
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.open_menu(lane, idx, event.position, cx);
                            }),
                        );
                    zone = match edge {
                        Edge::Start => zone.left_0(),
                        Edge::End => zone.right_0(),
                    };
                    zone
                }),
        )
        // The cues themselves: where in this window the track's words fall,
        // each its own full-height block over the letterbox ground -- a
        // placement is a window of a track, not full of speech.
        .children(cues.into_iter().map(|(cl, cw)| {
            div()
                .absolute()
                .top(px(1.))
                .bottom(px(1.))
                .left(px(cl))
                .w(px(cw))
                .rounded(px(0.))
                .bg(rgb(CLIP_TEXT()))
        }))
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
    // The bed's other kind of box: a subtitle lane holds no `Clip` at all
    // (`LaneKind::Subtitle`'s own doc), so this is empty everywhere but S1..
    // and `boxes` above is empty on a subtitle lane -- one row draws whichever
    // list it was given.
    let subs: Vec<SubClip> = player
        .session
        .as_ref()
        .map_or(&[][..], |s| s.sub_lane(lane))
        .to_vec();
    let sub_boxes: Vec<_> = subs
        .iter()
        .enumerate()
        .map(|(idx, sub)| sub_box(player, lane, idx, sub, scale, picks, pick_links, cx))
        .collect();
    // DESIGN §5's lane heads: "a small ink dot under each head, coloured by
    // the lane's source ink" -- the first clip's source stands for the lane,
    // since a lane with several sources still needs one dot, not a legend.
    let dot = clips
        .first()
        .map_or_else(INK4, |clip| source_tint(clip.source));
    let head_ghost: SharedString = lane.label().into();
    let gain_db = player
        .session
        .as_ref()
        .map_or(0., |session| session.lane_gain_db(lane));
    let shown = player.sub_lane_on(lane);
    let chord_style = type_scale::mono(
        type_scale::CHORD_METADATA_MIN_PX,
        gpui::FontWeight::MEDIUM,
    );
    div()
        .id(("bench-lane", lane.ord * 10 + lane.kind as usize))
        .flex_none()
        .h(px(h))
        .flex()
        // Dragged, the whole track moves in the stack (`LaneDrag`,
        // `Player::reorder_lane`) -- let go anywhere along the row, not just
        // over the head column, since a slot is what is being aimed at.
        // corner-cut: no drop-line cue (module doc) -- a plain highlight
        // while the pointer is over the row stands in for it.
        .drag_over::<LaneDrag>(|d, _, _, _| d.bg(rgb(DARK_RAISED())))
        .on_drop(cx.listener(move |this, drag: &LaneDrag, _, cx| {
            this.reorder_lane(drag.0, lane, cx);
        }))
        .child(
            // Pinned track head: its drag handle remains the lane label; verbs
            // stay separate pointer targets so a mix/remove click never starts
            // a reorder drag.
            div()
                .id(("bench-lane-head", lane.ord * 10 + lane.kind as usize))
                .flex_none()
                .w(px(HEAD_W))
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(LANE_HEAD_GAP))
                .bg(rgb(DARK_PANEL()))
                .cursor(CursorStyle::OpenHand)
                .on_drag(LaneDrag(lane), move |_, _, _, cx| {
                    cx.new(|_| Tip(head_ghost.clone()))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        // MOCK-SPEC.md "Bench": "V1, A1 in mono".
                        .type_style(type_scale::mono(
                            type_scale::CHORD_METADATA_MIN_PX,
                            gpui::FontWeight::MEDIUM,
                        ))
                        .text_color(rgb(INK2()))
                        .child(lane.label())
                        .when(lane.kind == LaneKind::Audio, |d| {
                            d.child(
                                div()
                                    .id(("bench-mix-lane", lane.ord))
                                    .flex()
                                    .items_center()
                                    .gap(px(1.))
                                    .rounded(px(3.))
                                    .px(px(2.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                                    .tooltip(move |_, cx| {
                                        cx.new(|_| {
                                            Tip(
                                                format!(
                                                    "{} {gain_db:+.0} dB — mix this lane",
                                                    lane.label()
                                                )
                                                .into(),
                                            )
                                        })
                                        .into()
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.act_lane(ActionId::Mix, lane, cx);
                                    }))
                                    .child("≋")
                                    .child(
                                        div()
                                            .type_style(chord_style.clone())
                                            .text_color(rgb(INK3()))
                                            .child(player.keymap.chord(ActionId::Mix)),
                                    ),
                            )
                        })
                        .when(lane.kind == LaneKind::Subtitle, |d| {
                            d.child(
                                div()
                                    .id(("bench-show-sub-lane", lane.ord))
                                    .rounded(px(3.))
                                    .px(px(2.))
                                    .cursor_pointer()
                                    .text_color(rgb(if shown { INK1() } else { INK3() }))
                                    .when(shown, |s| s.bg(rgb(DARK_RAISED())))
                                    .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                                    .tooltip(move |_, cx| {
                                        cx.new(|_| {
                                            Tip(
                                                format!(
                                                    "{} — click to show this subtitle lane",
                                                    lane.label()
                                                )
                                                .into(),
                                            )
                                        })
                                        .into()
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.show_sub_lane(lane, cx);
                                    }))
                                    .child("◉"),
                            )
                        })
                        .when(lane.kind != LaneKind::Subtitle, |d| {
                            let action = match lane.kind {
                                LaneKind::Video => ActionId::RemoveVideoLane,
                                LaneKind::Audio => ActionId::RemoveAudioLane,
                                LaneKind::Subtitle => unreachable!(),
                            };
                            d.child(
                                div()
                                    .id(("bench-remove-lane", lane.ord * 10 + lane.kind as usize))
                                    .flex()
                                    .items_center()
                                    .gap(px(1.))
                                    .rounded(px(3.))
                                    .px(px(2.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                                    .tooltip(move |_, cx| {
                                        cx.new(|_| {
                                            Tip(
                                                format!(
                                                    "Remove {} — it must be empty first",
                                                    lane.label()
                                                )
                                                .into(),
                                            )
                                        })
                                        .into()
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.act_lane(action, lane, cx);
                                    }))
                                    .child("×")
                                    .child(
                                        div()
                                            .type_style(chord_style)
                                            .text_color(rgb(INK3()))
                                            .child(player.keymap.chord(action)),
                                    ),
                            )
                        }),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(LANE_DOT_D))
                        .h(px(LANE_DOT_D))
                        .rounded(px(LANE_DOT_D / 2.))
                        .bg(rgb(dot)),
                ),
        )
        .child(
            // The bed: a drop target for the Sources tab (`AssetDrag`,
            // reused from `ui/timeline.rs`), a placed clip (`ClipDrag`) and
            // every clip on it.
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
                // Right-click on empty bed space: the "Close Gap" door. A
                // clip's own right-click handler (`clip_box`/`sub_box`) never
                // stops propagation, so this fires on that press too --
                // harmlessly, since `gap_at` answers `None` for a frame a
                // clip already covers and this listener does nothing then,
                // leaving the clip's own menu exactly as it set it.
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        let Some(session) = this.session.as_ref() else {
                            return;
                        };
                        let frame = this.frame_under(event.position.x);
                        if let Some((start, frames)) = session.gap_at(lane, frame) {
                            this.open_gap_menu(lane, start, frames, event.position, cx);
                        }
                    }),
                )
                // The landing shadow (`Player::preview_ghost_asset`, the same
                // setter `ui/timeline.rs`'s own bed calls) -- guarded on the
                // pointer actually being inside this bed's own bounds, since
                // `on_drag_move` fires on every painted element of the drag's
                // type, not just the one under the pointer.
                .on_drag_move(
                    cx.listener(move |this, event: &DragMoveEvent<AssetDrag>, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            return;
                        }
                        let path = event.drag(cx).0.clone();
                        this.preview_ghost_asset(&path, lane, event.event.position.x, cx);
                    }),
                )
                .drag_over::<ClipDrag>(|d, _, _, _| d.bg(rgb(DARK_RAISED())))
                .on_drop(cx.listener(move |this, drag: &ClipDrag, window, cx| {
                    let Some(idx) = this.dragged(drag) else {
                        return;
                    };
                    this.move_clip(drag.lane, idx, lane, window.mouse_position().x, cx)
                }))
                .on_drag_move(
                    cx.listener(move |this, event: &DragMoveEvent<ClipDrag>, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            return;
                        }
                        let drag = *event.drag(cx);
                        this.preview_ghost(&drag, lane, event.event.position.x, cx);
                    }),
                )
                // A caption already on a lane moves along it, or onto another
                // subtitle track, exactly as a clip does (`Player::move_sub`,
                // the same call `ui/timeline.rs`'s own bed makes).
                .drag_over::<SubDrag>(|d, _, _, _| d.bg(rgb(DARK_RAISED())))
                .on_drop(cx.listener(move |this, drag: &SubDrag, window, cx| {
                    this.move_sub(drag, lane, window.mouse_position().x, cx);
                }))
                .on_drag_move(
                    cx.listener(move |this, event: &DragMoveEvent<SubDrag>, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            return;
                        }
                        let drag = *event.drag(cx);
                        this.preview_ghost_sub(&drag, lane, event.event.position.x, cx);
                    }),
                )
                .drag_over::<SubPick>(|d, _, _, _| d.bg(rgb(DARK_RAISED())))
                .on_drop(cx.listener(move |this, drag: &SubPick, window, cx| {
                    this.place_sub(drag.0, lane, window.mouse_position().x, cx);
                }))
                .on_drag_move(
                    cx.listener(move |this, event: &DragMoveEvent<SubPick>, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            return;
                        }
                        let track = event.drag(cx).0;
                        this.preview_ghost_pick(track, lane, event.event.position.x, cx);
                    }),
                )
                // The wheel, matched to the legacy timeline's own mapping:
                // ctrl+wheel zooms about the pointer, a bare wheel scrolls
                // the bed along the film (`Player::timeline_wheel`). Stopped
                // here so gpui's own overflow scroll on the lane column
                // (`bench-lanes`) never answers the same notch a second time.
                .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                    cx.stop_propagation();
                    this.timeline_wheel(event, cx);
                }))
                .children(boxes)
                .children(sub_boxes)
                // The shadow of the row in flight (`Player::ghost`, set by
                // the `on_drag_move` handlers above) -- the same box
                // `ui/timeline.rs`'s own bed draws, only while a drag of its
                // kind is actually live (`App::has_active_drag`: gpui drops a
                // drag without telling anyone) and only on the one lane the
                // pointer is over.
                .children(
                    player
                        .ghost
                        .filter(|g| g.lane == lane && cx.has_active_drag())
                        .map(|g| {
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .left(px(scale.px_at(f64::from(g.start) / player.fps)))
                                .w(px(scale
                                    .width_px(f64::from(g.frames) / player.fps)
                                    .max(GHOST_MIN)))
                                .rounded(px(3.))
                                .border_1()
                                .border_color(rgb(if g.refused { DROP_REFUSE() } else { INK1() }))
                                .bg(rgba(
                                    ((if g.refused { DROP_REFUSE() } else { g.tint }) << 8)
                                        | GHOST_ALPHA,
                                ))
                        }),
                )
                // Where it would land, drawn on every lane so a clip lining
                // up with a take one track over can be seen to line up with
                // it -- the same cue `ui/timeline.rs` draws.
                .children(
                    player
                        .snap_cue
                        .filter(|_| player.trim.is_some() || cx.has_active_drag())
                        .map(|frame| {
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .left(px(scale.px_at(f64::from(frame) / player.fps)))
                                .w(px(1.))
                                .bg(rgb(INK1()))
                        }),
                ),
        )
}

/// The bench's content: pinned ruler, pinned heads, lane stack -- compressed
/// evenly to [`LANES_COMPRESS`] rows and then scrolling (DESIGN §7).
pub(crate) fn render(
    player: &mut Player,
    box_h: f32,
    cx: &mut Context<Player>,
) -> impl IntoElement {
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
    // FAULT 4: the playhead timecode plate is pinned at `left_0`..`PLATE_W`
    // over the ruler (DESIGN §5), so a tick whose label would land under it
    // is suppressed here rather than drawn and overlapped -- the plate
    // already owns that lane.
    let mut ticks = Vec::new();
    if scale.pps > 0. {
        let interval = tick_interval(scale.pps);
        let mut t = (scale.start / interval).ceil() * interval;
        while scale.px_at(t) <= bed_w {
            let x = scale.px_at(t).max(0.);
            if x >= PLATE_W {
                ticks.push((x, tick_mmss(t)));
            }
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
                // Ctrl+wheel zooms about the pointer, a bare one scrolls the
                // bed along -- the same mapping every bed below gives, and
                // the legacy ruler's own (`ui/timeline.rs`'s
                // `on_scroll_wheel` at its ruler strip).
                .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                    this.timeline_wheel(event, cx);
                }))
                .children(ticks.into_iter().map(|(x, label)| {
                    div().absolute().top_0().left(px(x)).child(
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
