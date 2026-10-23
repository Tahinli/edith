//! One lane row: the pinned head and the bed of clips (DESIGN §5, §7).

use super::clip::{ClipSlot, clip_box};
use super::sub::sub_box;
use super::*;
use crate::ui::icons::Icon;
use crate::ui::theme::tokens;
use crate::interact::lane_resize_band;
use crate::player::timeline_edit::Switch;
use crate::player::keyframes::merge_mark_px;

/// The opacity a hidden or muted lane's clips are drawn at (DESIGN §7): dim
/// enough to read as off, present enough to find again.
const DIMMED_OPACITY: f32 = 0.4;

/// One lane row: pinned head + bed of clips, DESIGN §5's "lanes V/A/S", drawn
/// at the height [`lane_height`] gives it.
pub(super) fn lane_row(
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
        .map(|(idx, clip)| {
            clip_box(
                player,
                ClipSlot {
                    lane,
                    idx,
                    clip: clip.clone(),
                    prev: idx.checked_sub(1).and_then(|p| clips.get(p).cloned()),
                    next: clips.get(idx + 1).cloned(),
                    picks,
                    pick_links,
                },
                scale,
                cx,
            )
        })
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
        .map(|(idx, sub)| sub_box(player, lane, idx, sub, scale, (picks, pick_links), cx))
        .collect();
    // DESIGN §5's lane heads: "a small ink dot under each head, coloured by
    // the lane's source ink" -- the first clip's source stands for the lane,
    // since a lane with several sources still needs one dot, not a legend.
    let dot = clips
        .first()
        .map_or_else(INK3, |clip| source_tint(clip.source));
    // The holes between placements, as `PlaybackSession::gap_at` names them:
    // the stretch before the first placement counts too, since closing it is
    // a gap like any other. The open tail after the last one does not -- nothing
    // would ripple back into it -- so the walk simply stops at the last end.
    let placed: Vec<(u32, u32)> = match lane.kind {
        LaneKind::Subtitle => subs.iter().map(|s| (s.start, s.end())).collect(),
        LaneKind::Video | LaneKind::Audio => clips.iter().map(|c| (c.start, c.end())).collect(),
    };
    let mut holes = Vec::new();
    let mut next = 0;
    for (start, end) in placed {
        // Only a hole between two placements is a gap: the stretch before the first one is not.
        if start > next && next > 0 {
            holes.push((next, start - next));
        }
        next = next.max(end);
    }
    let picked = player.gap_pick();
    let gap_boxes: Vec<_> = holes
        .into_iter()
        .map(|(start, frames)| {
            let selected = picked
                == Some(GapPick {
                    lane,
                    start,
                    frames,
                });
            gap_box(player, lane, start, frames, selected, scale, cx)
        })
        .collect();
    // A hidden or muted lane's clips sit at 40% (DESIGN §7). The lock dims
    // nothing: a locked clip is still on the picture and still plays.
    let dim = if player.lane_switch(lane, Switch::Hidden) || player.lane_switch(lane, Switch::Muted)
    {
        DIMMED_OPACITY
    } else {
        1.0
    };
    let drop = player
        .lane_drop
        .filter(|drop| drop.lane == lane && cx.has_active_drag());
    // The keys on each clip, drawn as diamonds along its foot, over the clip body.
    let key_marks: Vec<gpui::AnyElement> = clips
        .iter()
        .enumerate()
        .flat_map(|(idx, _)| {
            key_marks(player, lane, idx, picks.contains(&(lane, idx)), scale, h, cx)
        })
        .collect();
    div()
        .id(("bench-lane", lane.ord * 10 + lane.kind as usize))
        .flex_none()
        .h(px(h))
        .flex()
        .relative()
        // The wheel, matched to the legacy timeline's own mapping: ctrl+wheel
        // zooms about the pointer, a bare wheel scrolls the bed along the film
        // (`Player::timeline_wheel`). On the whole row -- head column and bed
        // alike -- not on the bed alone: a hand rolls the wheel wherever it
        // left the pointer, and aiming at a clip first is a tax. Stopped here
        // so gpui's own overflow scroll on the lane column (`bench-lanes`) and
        // `bench-content`'s catch-all below never answer the same notch again.
        .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
            cx.stop_propagation();
            this.timeline_wheel(event, cx);
        }))
        // Dragged, the whole track moves in the stack (`LaneDrag`,
        // `Player::reorder_lane`) -- let go anywhere along the row, not just
        // over the head column, since a slot is what is being aimed at.
        .drag_over::<LaneDrag>(|d, _, _, _| d.bg(rgba(tokens().accent_wash)))
        .on_drop(cx.listener(move |this, drag: &LaneDrag, _, cx| {
            this.reorder_lane(drag.0, lane, cx);
        }))
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<LaneDrag>, _, cx| {
                if !event.bounds.contains(&event.event.position) {
                    return this.forget_lane_drop(lane, cx);
                }
                this.preview_lane_drop(event.drag(cx).0, lane, cx);
            }),
        )
        .drag_over::<AssetDrag>(|d, _, _, _| d.bg(rgba(tokens().accent_wash)))
        .on_drop(cx.listener(move |this, drag: &AssetDrag, window, cx| {
            // The shadow goes whatever the drop does: placed, or refused with its words.
            this.end_drag_preview(cx);
            this.drag_alt = window.modifiers().alt;
            let x = this
                .drag_x
                .take()
                .unwrap_or_else(|| window.mouse_position().x);
            let at = this.place_frame(x).0;
            this.drop_source(&drag.0.clone(), drag.1, Some(lane), Some(at), window.modifiers().control, cx)
        }))
        // The landing shadow (`Player::preview_ghost_asset`, the same
        // setter `ui/timeline.rs`'s own bed calls) -- guarded on the
        // pointer actually being inside this *row's* bounds, since
        // `on_drag_move` fires on every painted element of the drag's
        // type, not just the one under the pointer. The row and not the
        // bed: the head column is `HEAD_W` px of this lane too, and a clip
        // dragged out over it is aiming at frame 0 -- the drop it used to
        // fall through (nothing under the pointer answered `ClipDrag`) is
        // the "I have to be pixel perfect to snap to 0" complaint.
        // `Player::frame_under` clamps an x left of the bed to the head of
        // the view (`px_along`), so the head column reads as frame 0.
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<AssetDrag>, _, cx| {
                if !event.bounds.contains(&event.event.position) {
                    // The pointer has left the row this shadow was drawn on: the shadow goes with
                    // it, as the clip drag's does, rather than being left at the last lane.
                    if this.drag_lane == Some(lane) {
                        this.end_drag_preview(cx);
                    }
                    return;
                }
                let path = event.drag(cx).0.clone();
                this.drag_x = Some(event.event.position.x);
                // The row the catch-all release below aims at. A file's own
                // kind is not known until the engine has placed it, so the
                // last row hovered is the answer and `Player::insert_source`
                // words the refusal if it cannot hold this file.
                this.drag_lane = Some(lane);
                this.drag_alt = event.event.modifiers.alt;
                this.drag_ctrl = event.event.modifiers.control;
                this.drag_below = false;
                this.preview_ghost_asset(&path, lane, event.event.position.x, cx);
                // The shadow is the length the drop will have, not the file's
                // whole count: a still lands at its five seconds, whatever length
                // it may be trimmed out to (`Player::preview_ghost_asset` sizes it
                // by the count).
                let frames = asset_drop_frames(this.session.as_ref(), &path, this.fps);
                for ghost in &mut this.ghost {
                    ghost.frames = frames;
                }
            }),
        )
        .drag_over::<ClipDrag>(|d, _, _, _| d.bg(rgba(tokens().accent_wash)))
        .on_drop(cx.listener(move |this, drag: &ClipDrag, window, cx| {
            // A slip, slide or roll that began on the same press ends here, not as a move.
            if this.pro_drag.is_some() {
                this.commit_pro(cx);
                return;
            }
            this.drag_alt = window.modifiers().alt;
            let Some(idx) = this.dragged(drag) else {
                return;
            };
            let x = this
                .drag_x
                .take()
                .unwrap_or_else(|| window.mouse_position().x);
            this.move_clip(drag.lane, idx, lane, x, cx)
        }))
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<ClipDrag>, _, cx| {
                if !event.bounds.contains(&event.event.position) || this.pro_drag.is_some() {
                    return;
                }
                let drag = event.drag(cx).clone();
                this.drag_x = Some(event.event.position.x);
                // The last lane this clip could actually land on: a picture
                // clip dragged down across A1 on its way to the empty bench
                // below is still aiming at V1, and the catch-all release must
                // not hand it the audio lane that refused it on the way past.
                this.drag_lane = held_lane(this.drag_lane, lane, drag.lane);
                this.drag_alt = event.event.modifiers.alt;
                this.preview_ghost(&drag, lane, event.event.position.x, cx);
            }),
        )
        // A caption already on a lane moves along it, or onto another
        // subtitle track, exactly as a clip does (`Player::move_sub`,
        // the same call `ui/timeline.rs`'s own bed makes).
        .drag_over::<SubDrag>(|d, _, _, _| d.bg(rgba(tokens().accent_wash)))
        .on_drop(cx.listener(move |this, drag: &SubDrag, window, cx| {
            this.drag_alt = window.modifiers().alt;
            let x = this
                .drag_x
                .take()
                .unwrap_or_else(|| window.mouse_position().x);
            this.move_sub(drag, lane, x, cx);
        }))
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<SubDrag>, _, cx| {
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                let drag = *event.drag(cx);
                this.drag_x = Some(event.event.position.x);
                this.drag_lane = held_lane(this.drag_lane, lane, drag.lane);
                this.drag_alt = event.event.modifiers.alt;
                this.preview_ghost_sub(&drag, lane, event.event.position.x, cx);
            }),
        )
        .drag_over::<SubPick>(|d, _, _, _| d.bg(rgba(tokens().accent_wash)))
        .on_drop(cx.listener(move |this, drag: &SubPick, window, cx| {
            this.drag_alt = window.modifiers().alt;
            let x = this
                .drag_x
                .take()
                .unwrap_or_else(|| window.mouse_position().x);
            this.place_sub(drag.0, lane, x, cx);
        }))
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<SubPick>, _, cx| {
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                let track = event.drag(cx).0;
                this.drag_x = Some(event.event.position.x);
                this.drag_lane = Some(lane);
                this.drag_alt = event.event.modifiers.alt;
                this.preview_ghost_pick(track, lane, event.event.position.x, cx);
            }),
        )
        .child(lane_head(player, lane, dot, picks.iter().any(|(l, _)| *l == lane), cx))
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
                .opacity(dim)
                // DESIGN Â§7: the lanes scroll BEHIND the pinned heads. A
                // clip whose start is left of the view sits at a negative
                // `left`, and without a clip mask gpui paints it -- and its
                // hitbox -- straight over the `HEAD_W` px head column, so V1/A1 and
                // their verbs vanish under it. The mask cuts painting and
                // hit-testing at the bed's own edges together, so the visible
                // half of a half-scrolled clip still drags and trims and the
                // hidden half answers nothing. Menus, drag previews and
                // tooltips are unaffected: they render at the window root
                // (`ui/overlays.rs`, gpui's `active_drag`), not in here.
                .overflow_hidden()
                .bg(rgb(tokens().well))
                // Media (or a caption, or a placed clip) being dragged over this lane: the bed
                // itself lights up -- the row's own wash is hidden under the bed's fill -- so
                // the user sees which lane takes the drop (REDESIGN §3.11).
                .drag_over::<AssetDrag>(|d, _, _, _| d.bg(rgb(crate::ui::theme::mix(tokens().well, tokens().accent, 0.22))))
                .drag_over::<ClipDrag>(|d, _, _, _| d.bg(rgb(crate::ui::theme::mix(tokens().well, tokens().accent, 0.22))))
                // 1px `line` under the lane (REDESIGN §3.6).
                .border_b_1()
                .border_color(rgba(tokens().line))
                // The seam is drawn as an absolute overlay rather than a
                // layout `border_l_1`: a border eats a pixel out of the
                // content box it decorates, so frame 0 -- drawn by every
                // ghost and clip at `left(px(0.))` relative to *this* box --
                // would sit one pixel right of what `Player::frame_under`
                // computes through the ruler's own (unbordered) bounds
                // (bench-ruler, below). One shared origin, so a drop lands
                // exactly on the frame its ghost showed.
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .w(px(1.))
                        .h_full()
                        .bg(rgba(DARK_SEAM())),
                )
                // Left press on the bed picks the hole under the pointer, or
                // drops a gap pick that a press elsewhere leaves behind. A
                // clip's own press runs its own pick (which clears the gap),
                // and over a clip `gap_at` answers `None` here, so nothing
                // stale is kept by this one.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        let frame = this.frame_under(event.position.x);
                        this.begin_marquee(lane, frame, event.position);
                        let hole = this.session.as_ref().and_then(|s| s.gap_at(lane, frame));
                        let hole = crate::timeline_math::between_clips(hole);
                        match hole {
                            Some((start, frames)) => this.select_gap(lane, start, frames, cx),
                            None => this.clear_gap(cx),
                        }
                    }),
                )
                // Right-click on empty bed space: the "Close Gap" door. A
                // clip's own right-click handler (`clip_box`/`sub_box`) never
                // stops propagation, so this fires on that press too --
                // harmlessly, since `gap_at` answers `None` for a frame a
                // clip already covers and this listener does nothing then,
                // leaving the clip's own menu exactly as it set it.
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        // A clip's (or a caption's) own handler has already run
                        // by the time this bubbles up and has set its menu: the
                        // scrim swallows every press while a menu is open, so a
                        // menu standing here can only be the one this very
                        // press just opened, and the bed must not replace it.
                        if this.context_menu.is_some() {
                            return;
                        }
                        let Some(session) = this.session.as_ref() else {
                            return;
                        };
                        let frame = this.frame_under(event.position.x);
                        match crate::timeline_math::between_clips(session.gap_at(lane, frame)) {
                            Some((start, frames)) => {
                                this.open_gap_menu(lane, start, frames, event.position, cx);
                            }
                            // Empty bed past the end of the lane's contents: no
                            // hole to close there, so what the pointer is over
                            // is the bench itself (DESIGN §9).
                            None => this.open_empty_menu(lane, event.position, cx),
                        }
                    }),
                )
                .children(hitmap::dynamic(
                    move || {
                        (
                            format!("lane.{}.bed", lane.label()),
                            format!("{} timeline bed", lane.label()),
                        )
                    },
                    true,
                ))
                .child({
                    let beds = player.lane_beds.clone();
                    canvas(
                        move |bounds, _, _| {
                            let mut beds = beds.borrow_mut();
                            match beds.iter_mut().find(|(l, _)| *l == lane) {
                                Some(entry) => entry.1 = bounds,
                                None => beds.push((lane, bounds)),
                            }
                        },
                        |_, _, _, _| (),
                    )
                    .absolute()
                    .size_full()
                })
                .children(boxes)
                .children(key_marks)
                .children(sub_boxes)
                .children(gap_boxes)
                // The In-Out range, tinted across the lane over the clips: subtle enough to
                // read every clip through, with a hairline at each mark.
                .children(super::ruler::range_band(player.range, player.fps, scale, f32::from(player.ruler.get().size.width).max(1.)).map(|(x, w)| {
                    let line = rgba(crate::ui::theme::with_alpha(tokens().accent, 0x80));
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .left(px(x))
                        .w(px(w))
                        .bg(rgba(tokens().range_tint))
                        .child(div().absolute().top_0().left_0().w(px(1.)).h_full().bg(line))
                        .child(div().absolute().top_0().right_0().w(px(1.)).h_full().bg(line))
                }))
                // A locked lane: a faint diagonal hatch over its bed, so it reads as
                // closed to edits at a glance.
                .when(player.lane_switch(lane, Switch::Locked), |d| d.child(locked_hatch()))
                // The box select in flight, clipped to this bed: the part of the
                // pointer rectangle that crosses this lane.
                .children(player.marquee.filter(Marquee::live).and_then(|m| {
                    let beds = player.lane_beds.borrow();
                    let bed = beds.iter().find(|(l, _)| *l == lane)?.1;
                    let (l, t, r, b) = m.edges();
                    let (top, bottom) = (t.max(bed.top()), b.min(bed.bottom()));
                    (bottom > top).then(|| {
                        div()
                            .absolute()
                            .left(l - bed.left())
                            .top(top - bed.top())
                            .w(r - l)
                            .h(bottom - top)
                            .border_1()
                            .border_color(rgb(INK1()))
                            .bg(rgba((INK1() << 8) | GHOST_ALPHA))
                    })
                }))
                // The shadow of the row in flight (`Player::ghost`, set by
                // the `on_drag_move` handlers above) -- the same box
                // `ui/timeline.rs`'s own bed draws, only while a drag of its
                // kind is actually live (`App::has_active_drag`: gpui drops a
                // drag without telling anyone) and only on the one lane the
                // pointer is over.
                .children(
                    player
                        .ghost
                        .iter()
                        .copied()
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
                                .border_2()
                                .border_color(rgb(if g.refused { DROP_REFUSE() } else { tokens().accent }))
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
                                .bg(rgb(tokens().accent))
                        }),
                ),
        )
        // The drop-line is inside its target row so it survives the lane
        // column's clipping at the first row; it marks exactly the slot the
        // release will reorder into, rather than a generic drag highlight.
        .children(drop.map(|drop| {
            let line = div().absolute().left_0().w_full().h(px(2.)).bg(rgb(INK1()));
            if drop.above {
                line.top_0()
            } else {
                line.bottom_0()
            }
        }))
        // The resize band is the header's bottom edge and nothing else: only a press on the
        // head column resizes a track, so a clip dragged vertically along the bed never grows
        // its lane (`interact::lane_resize_band` says what it covers, here the head's width).
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .w(px(HEAD_W))
                .h(px(h))
                .child(lane_resize_band(lane, h, 0., cx)),
        )
}

/// A key diamond's box, in px.
const KEY_MARK_SIZE: f32 = 9.;
/// How far a key diamond's foot sits above the lane's bottom: the clip body's own
/// inset, so the diamonds stand on the body's bottom edge.
const KEY_MARK_FOOT: f32 = 3.;
/// Keys closer than this many px share one diamond.
const KEY_MARK_MERGE_PX: f32 = 4.;
/// A clip narrower than this draws no key diamonds: its box is a sliver.
const KEY_MARK_MIN_W: f32 = 24.;

/// The key diamonds of one clip (`Player::clip_key_frames`), along its foot. The
/// selected clip's are accent, the rest light ink, each edged in the dark. A click
/// on one seeks the playhead to its key and takes the press, so the clip under it
/// does not also select (the lane's resize band stays clear of them).
fn key_marks(
    player: &Player,
    lane: Lane,
    idx: usize,
    selected: bool,
    scale: Scale,
    h: f32,
    cx: &mut Context<Player>,
) -> Vec<gpui::AnyElement> {
    let Some(clip) = player.session.as_ref().and_then(|s| s.lane_clips(lane).get(idx)) else {
        return Vec::new();
    };
    let width = scale.width_px(f64::from(clip.frames()) / player.fps);
    if width < KEY_MARK_MIN_W {
        return Vec::new();
    }
    let left = scale.px_at(f64::from(clip.start) / player.fps);
    let frames = player.clip_key_frames(lane, idx);
    let xs: Vec<f32> = frames
        .iter()
        .map(|&frame| scale.px_at(f64::from(frame) / player.fps))
        .collect();
    let inside: Vec<usize> = (0..xs.len())
        .filter(|&n| xs[n] >= left && xs[n] < left + width)
        .collect();
    let inside_px: Vec<f32> = inside.iter().map(|&n| xs[n]).collect();
    let colour = match selected {
        true => crate::ui::theme::with_alpha(tokens().accent, 0xff),
        false => crate::ui::theme::with_alpha(tokens().ink1, 0xff),
    };
    let fps = player.fps;
    let top = h - KEY_MARK_FOOT - KEY_MARK_SIZE;
    merge_mark_px(&inside_px, KEY_MARK_MERGE_PX)
        .into_iter()
        .map(|kept| {
            let n = inside[kept];
            let frame = frames[n];
            div()
                .id(("bench-key", ((lane.ord * 10 + lane.kind as usize) * 100_000 + idx) * 100_000 + n))
                .absolute()
                .top(px(top))
                .left(px(xs[n] - KEY_MARK_SIZE / 2.))
                .size(px(KEY_MARK_SIZE))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.seek(f64::from(frame) / fps, cx);
                    }),
                )
                .child(key_diamond(KEY_MARK_SIZE, colour))
                .into_any_element()
        })
        .collect()
}

/// A solid diamond of `size` px in `colour`, with a 1 px outline in the dark (`e0`
/// at 70%) so it reads over any picture.
fn key_diamond(size: f32, colour: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let (cx_, cy_) = (o.x + s.width / 2., o.y + s.height / 2.);
            let mut paint = |half: f32, colour: u32| {
                let half = px(half);
                let mut path = gpui::PathBuilder::fill();
                path.move_to(gpui::point(cx_, cy_ - half));
                path.line_to(gpui::point(cx_ + half, cy_));
                path.line_to(gpui::point(cx_, cy_ + half));
                path.line_to(gpui::point(cx_ - half, cy_));
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgba(colour));
                }
            };
            paint(size / 2., crate::ui::theme::with_alpha(tokens().e0, 0xb3));
            paint(size / 2. - 1., colour);
        },
    )
    .size(px(size))
}

/// The inline `Close gap` button's width. A gap narrower than this shows no
/// button: a label that spills over its own hole reads as the next clip's.
const GAP_BTN_W: f32 = 96.;
/// One hole on the bed (DESIGN §7): a wash and outline when picked, carrying the `Close gap` button while the pointer is on it. The button is
/// centred and its hit area is a full [`HIT_MIN`] tall.
fn gap_box(
    player: &Player,
    lane: Lane,
    start: u32,
    frames: u32,
    selected: bool,
    scale: Scale,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let left = scale.px_at(f64::from(start) / player.fps);
    let width = scale.width_px(f64::from(frames) / player.fps);
    let group = SharedString::from(format!("bench-gap-{}-{}-{start}", lane.ord, lane.kind as usize));
    div()
        .id(group.clone())
        .group(group.clone())
        .absolute()
        .top_0()
        .h_full()
        .left(px(left))
        .w(px(width.max(1.)))
        // What a hole is for, on hover: the mouse's own verb, in words.
        .tooltip(crate::ui::widgets::tip_hover("Gap: right-click to close", "", None))
        // No hatch: a picked hole is a quiet wash with an outline.
        .when(selected, |d| {
            d.bg(rgba(tokens().accent_wash))
                .border_1()
                .border_color(rgb(tokens().accent))
        })
        .flex()
        .items_center()
        .justify_center()
        .children((width >= GAP_BTN_W).then(|| {
            // Refused while it cannot close (an export, a locked track): dimmed, with the reason
            // as its tooltip, and the click is not taken.
            let why = player.close_gap_refusal(lane);
            div()
                .id(("bench-gap-close", lane.ord * 10 + lane.kind as usize))
                .flex_none()
                .w(px(GAP_BTN_W))
                .h(px(HIT_MIN))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .type_style(type_scale::caption_strong())
                .text_color(rgb(INK1()))
                .when(why.is_none(), |d| d.cursor_pointer().hover(|s| s.bg(rgb(DARK_RAISED()))))
                .when(why.is_some(), |d| d.opacity(0.4).cursor_not_allowed())
                .when_some(why, |d, why| d.tooltip(crate::ui::widgets::tip_hover(why, "", None)))
                // Shown only while the pointer is on the hole: a label that sat
                // in every picked gap read as a button in the timeline's middle.
                .invisible()
                .group_hover(group, |s| s.visible())
                // Stopped here so the bed's own press does not re-pick the
                // hole the button is about to close.
                .when(why.is_none(), |d| {
                    d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.select_gap(lane, start, frames, cx);
                        this.close_selected_gap(cx);
                    }))
                })
                .child("Close gap")
        }))
}

/// The pinned track head (DESIGN §5, §7): the source dot and the lane's name,
/// then its switches. ◉ hides a video or subtitle lane, ♪ mutes an audio lane,
/// ▣ locks any lane; each is INK3 at rest and, once on, its glyph is struck
/// through in INK1. A subtitle lane's name is also its show toggle (`show_sub_lane`),
/// since the head has no room for a word beside three glyphs. Dragging the head
/// reorders the track (`LaneDrag`), its right-click names the track.
fn lane_head(
    player: &Player,
    lane: Lane,
    dot: u32,
    target: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let head_ghost: SharedString = player.lane_title(lane).into();
    let subtitle = lane.kind == LaneKind::Subtitle;
    let shown = player.sub_lane_on(lane);
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(5.))
        // The source patch box leads the head, so the source monitor's pick sits where a
        // Premiere user looks for it. Its slot is reserved even when no box shows.
        .child(patch_slot(player, lane, cx))
        // The head is the source dot, the name and the switches, and nothing
        // else (DESIGN §5, cleanse round 2): the verbs it used to wear -- mix,
        // remove -- are rows in its own right-click menu (`oracle::lane_items`)
        // and keep their chords there.
        .child(
            div()
                .flex_none()
                .w(px(LANE_DOT_D))
                .h(px(LANE_DOT_D))
                .rounded(px(LANE_DOT_D / 2.))
                .bg(rgb(dot)),
        )
        .child(lane_name(player, lane, subtitle, shown, cx));
    if lane.kind != LaneKind::Audio {
        row = row.child(lane_switch(
            lane,
            Switch::Hidden,
            player.lane_switch(lane, Switch::Hidden),
            cx,
        ));
    } else {
        row = row.child(lane_switch(
            lane,
            Switch::Muted,
            player.lane_switch(lane, Switch::Muted),
            cx,
        ));
    }
    row = row.child(lane_switch(
        lane,
        Switch::Locked,
        player.lane_switch(lane, Switch::Locked),
        cx,
    ));
    div()
        .id(("bench-lane-head", lane.ord * 10 + lane.kind as usize))
        .flex_none()
        .w(px(HEAD_W))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(LANE_HEAD_GAP))
        .relative()
        // `e1` with a 1px `e0` seam right and bottom; the lane holding the
        // pick wears a 2px accent bar on the left.
        .bg(rgb(tokens().e1))
        .border_r_1()
        .border_b_1()
        .border_color(rgb(tokens().e0))
        .cursor(CursorStyle::OpenHand)
        .on_drag(LaneDrag(lane), move |_, _, _, cx| {
            cx.new(|_| Tip(head_ghost.clone()))
        })
        .tooltip(crate::ui::widgets::tip_hover("Reorder lane", "", Some("Drag to move the lane up or down")))
        // The head's own verbs, never the bed's: a right-click here
        // names the *track* (DESIGN §9), so it opens the lane menu and
        // not the clip menu the bed beside it opens.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.open_head_menu(lane, event.position, cx);
            }),
        )
        .children(hitmap::dynamic(
            move || {
                (
                    format!("lane.{}.reorder", lane.label()),
                    format!("Reorder {}", lane.label()),
                )
            },
            true,
        ))
        .child(row)
        .when(target, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(rgb(tokens().accent)),
            )
        })
}

/// The source patch box on a lane head (Premiere's V and A patch): a video lane wears V and an
/// audio lane A. The box is lit on the lane the source monitor's picture or sound goes to, and a
/// click moves the patch there. It shows only while the source monitor has an item loaded; the
/// slot is always the same width, so nothing beside it moves. A subtitle lane has no patch.
fn patch_slot(player: &Player, lane: Lane, cx: &mut Context<Player>) -> AnyElement {
    let slot = div().flex_none().w(px(PATCH_W)).h(px(PATCH_W));
    if lane.kind == LaneKind::Subtitle || !player.source_patch_shown() {
        return slot.into_any_element();
    }
    let on = player.source_patch_on(lane);
    let letter = match lane.kind {
        LaneKind::Audio => "A",
        _ => "V",
    };
    let note = match on {
        true => "The source monitor's picture or sound goes to this track on Insert and Overwrite.",
        false => "Click to send the source monitor's picture or sound to this track on Insert and Overwrite.",
    };
    let hit = format!("lane.{}.patch", lane.label());
    let name = format!("Patch {}", lane.label());
    slot.id(SharedString::from(format!("bench-lane-{}-patch", lane.label())))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .text_size(px(12.))
        .cursor_pointer()
        .when(on, |d| d.bg(rgba(tokens().accent_wash)))
        .text_color(rgb(if on { tokens().accent } else { tokens().ink3 }))
        .hover(|d| d.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
        .tooltip(crate::ui::widgets::tip_hover(&name, "", Some(note)))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.set_source_patch(lane, cx);
        }))
        .children(hitmap::dynamic(move || (hit.clone(), name.clone()), true))
        .child(letter)
        .into_any_element()
}

/// A lane's name. On a subtitle lane it is the show toggle: the one lane whose
/// captions are over the picture wears the lit plate (`Player::sub_lane_on`).
fn lane_name(
    player: &Player,
    lane: Lane,
    subtitle: bool,
    shown: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let title = player.lane_title(lane);
    let editing = player.lane_edit.as_ref().filter(|(l, _)| *l == lane).map(|(_, t)| t.clone());
    let selected = editing.as_ref().is_some_and(|t| t.all && !t.text.is_empty());
    // While it is retyped the label is the end of the text and a caret: the box is a fixed
    // width, so the head never changes size under the hand.
    let fit = // Inter 12/600 averages about 6.4 px a glyph (ten fill the 68 px box).
    (LANE_NAME_MAX_W / 6.4).floor().max(2.) as usize;
    let shown_text: SharedString = match &editing {
        Some(field) => {
            let tail = crate::player::lane_names::edit_tail(&field.text, fit);
            if field.all { tail.into() } else { format!("{tail}▏").into() }
        }
        None => crate::player::lane_names::ellipsise(&title, fit).into(),
    };
    let named = title != lane.label();
    let name = div()
        .id(("bench-lane-name", lane.ord * 10 + lane.kind as usize))
        .flex()
        .items_center()
        // One width for every label, named or not, typed in or not: the switches beside it
        // never move.
        .flex_none()
        .w(px(LANE_NAME_MAX_W))
        .when(editing.is_some(), |d| d.bg(rgb(DARK_RAISED())))
        // Inter caption, 600 (REDESIGN §3.6).
        .type_style(type_scale::section())
        .text_color(rgb(INK1()))
        .when(selected, |d| d.bg(rgb(INK3())).text_color(rgb(DARK_PANEL())))
        .child(div().w_full().min_w(px(0.)).truncate().child(shown_text))
        .when(named && editing.is_none(), |d| {
            d.tooltip(crate::ui::widgets::tip_hover(&title, "double-click to rename", Some(&lane.label())))
        })
        .when(!named && editing.is_none() && !subtitle, |d| {
            d.tooltip(crate::ui::widgets::tip_hover(&lane.label(), "double-click to rename", None))
        })
        .children(hitmap::dynamic(
            move || (format!("lane.{}.name", lane.label()), format!("Name of {}", lane.label())),
            true,
        ));
    if !subtitle {
        return name
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if event.click_count() >= 2 && this.lane_edit.is_none() {
                    this.begin_lane_edit(lane, cx);
                }
            }))
            .into_any_element();
    }
    name.id(("bench-show-sub-lane", lane.ord))
        .h(px(HIT_MIN))
        .px(px(4.))
        .rounded(px(3.))
        .cursor_pointer()
        .when(shown, |s| s.bg(rgb(DARK_RAISED())))
        .hover(|s| s.bg(rgb(DARK_RAISED())))
        .tooltip(crate::ui::widgets::tip_hover(
            &format!("{} subtitles", lane.label()),
            "",
            shown.then_some("on the picture"),
        ))
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            if event.click_count() >= 2 && this.lane_edit.is_none() {
                this.begin_lane_edit(lane, cx);
            } else {
                this.show_sub_lane(lane, cx);
            }
        }))
        .into_any_element()
}

/// One head switch (DESIGN §7): a 24px hit area around a text glyph. The
/// click flips the switch the glyph names; the tooltip says what a click does
/// and, once it is on, why it is.
fn lane_switch(
    lane: Lane,
    which: Switch,
    on: bool,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let (glyph, tag, name) = match which {
        Switch::Hidden => (
            if on { Icon::EyeOff } else { Icon::Eye },
            "eye",
            if on { "Show" } else { "Hide" },
        ),
        Switch::Muted => (
            if on { Icon::VolumeX } else { Icon::Volume2 },
            "mute",
            if on { "Unmute" } else { "Mute" },
        ),
        Switch::Locked => (
            if on { Icon::Lock } else { Icon::LockOpen },
            "lock",
            if on { "Unlock" } else { "Lock" },
        ),
    };
    let tip = format!("{name} {}", lane.label());
    // What the switch's state is, in a whole sentence: the tooltip says it either way.
    let note = match (which, on) {
        (Switch::Hidden, false) => "This track is shown on the picture and in the export.",
        (Switch::Hidden, true) => "This track is hidden. It is left off the picture and the export.",
        (Switch::Muted, false) => "This track is audible in the mix.",
        (Switch::Muted, true) => "This track is muted. It is silent in the mix.",
        (Switch::Locked, false) => "This track is unlocked. Its clips can be edited.",
        (Switch::Locked, true) => "This track is locked. Its clips refuse edits.",
    };
    // The aim ids the pointer harness drives, written out per switch so each
    // one is named where it is made.
    let hit_id = match which {
        Switch::Hidden => "lane.{}.eye",
        Switch::Muted => "lane.{}.mute",
        Switch::Locked => "lane.{}.lock",
    };
    div()
        .id(SharedString::from(format!(
            "bench-lane-{}-{tag}",
            lane.label()
        )))
        .flex_none()
        .w(px(HIT_MIN))
        .h(px(HIT_MIN))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .cursor_pointer()
        // Engaged (hidden, muted, locked): an accent glyph on a wash, so the state reads
        // from across the room; at rest the glyph is quiet ink.
        .when(on, |d| d.bg(rgba(tokens().accent_wash)))
        .hover(|d| d.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
        .tooltip(crate::ui::widgets::tip_hover(&tip, "", Some(note)))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.set_lane_switch(lane, which, !on, cx);
        }))
        .children(hitmap::dynamic(
            move || {
                (
                    hit_id.replace("{}", &lane.label()),
                    format!("{name} {}", lane.label()),
                )
            },
            true,
        ))
        .child(glyph.el_px(px(14.), if on { tokens().accent } else { tokens().ink3 }))
}

/// The hatch over a locked lane's bed: 1px diagonal lines every 9px in faint ink.
fn locked_hatch() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |bounds, _, window, _| {
            let (o, w, h) = (bounds.origin, f32::from(bounds.size.width), f32::from(bounds.size.height));
            let mut x = -h;
            while x < w {
                let mut path = gpui::PathBuilder::stroke(px(1.));
                path.move_to(point(o.x + px(x), o.y + px(h)));
                path.line_to(point(o.x + px(x + h), o.y));
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgba(crate::ui::theme::with_alpha(tokens().ink1, 0x12)));
                }
                x += 9.;
            }
        },
    )
    .absolute()
    .size_full()
}

/// The frames an asset dropped now would land with: a still its project's still length (the
/// default when no project is open), never past the count the engine holds for it, and a video or
/// a song its own length.
fn asset_drop_frames(session: Option<&PlaybackSession>, path: &std::path::Path, fps: f64) -> u32 {
    let total = session.map_or(0, |s| s.file_frames(path));
    match engine::is_image(path) {
        true => {
            let secs = session.map_or(engine::project::STILL_DEFAULT_SECS, PlaybackSession::still_secs);
            ((secs * fps).ceil() as u32).clamp(1, total.max(1))
        }
        false => total,
    }
}

/// What an empty lane's bed says in its own corner, quietly: where media
/// goes. Nothing once the lane holds anything.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn empty_lane_hint(kind: LaneKind) -> &'static str {
    match kind {
        LaneKind::Video | LaneKind::Audio => "Drop media here",
        LaneKind::Subtitle => "Drop captions here",
    }
}

/// Whether a waveform is known to hold no sound: the file has no audio track,
/// or every peak is flat. Loading and failed are not "silent" -- those are
/// not answers.
pub(crate) fn wave_is_silent(wave: &Wave) -> bool {
    match wave {
        Wave::Silent => true,
        Wave::Peaks(peaks) => peaks
            .iter()
            .all(|&(lo, hi)| lo.abs() < 1e-4 && hi.abs() < 1e-4),
        _ => false,
    }
}

/// A quiet marker for a silent audio clip: a flat centre line and the word,
/// in the lowest ink, so it reads as "no sound" and not as a failure.
pub(crate) fn silent_marker(ink: u32) -> impl IntoElement {
    div()
        .absolute()
        .left(px(3.))
        .right_0()
        .top_0()
        .bottom_0()
        .flex()
        .items_center()
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .h(px(1.))
                .bg(rgba((ink << 8) | 0x66)),
        )
        .child(
            div()
                .relative()
                .ml(px(6.))
                .px(px(4.))
                .bg(rgba(DARK_SEAM()))
                .type_style(type_scale::caption())
                .text_color(rgb(INK3()))
                .child("silent"),
        )
}
