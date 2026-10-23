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
//! Fade wedges, dissolve joins, and the lane header's drop-line cue reuse the
//! legacy interaction state: `FadeDrag` previews the ramp until release,
//! `dissolve_selected` still owns the toggle, and `LaneDrop` answers the same
//! slot that `reorder_lane` commits.
//!
//! `nudge_cut` and friends still act on `Player::selected` directly, with no
//! element under the pointer required, and stay untouched by any of this.

pub(crate) mod clip;
pub(crate) mod clip_envelope;
pub(crate) mod lane;
mod ruler;
mod sub;
mod titles_row;
pub(crate) mod overview;

use crate::ui::hitmap;
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::*;
use crate::*;

use lane::lane_row;
use ruler::ruler;

/// The pinned ruler's own height, above the lane stack -- tall enough for a
/// tick line plus a mono label under it, and the playhead's lamp tab along
/// its top edge (DESIGN §6.5).
pub(crate) const RULER_H: f32 = crate::layout::redesign::RULER_H;
/// A mono character's width at `size_px`, from the shared
/// [`crate::layout::LIST_CHAR_W`] calibration (taken at
/// [`type_scale::CHORD_METADATA_MIN_PX`]).
pub(crate) fn char_w(size_px: f32) -> f32 {
    crate::layout::LIST_CHAR_W / type_scale::CHORD_METADATA_MIN_PX * size_px
}

/// The pinned track-head column: the source dot and the lane's name, and
/// nothing else since the head's verbs moved into its own menu (cleanse
/// round 2) -- the pixels it gave up are the bed's now.
pub(crate) const HEAD_W: f32 = 156.;
pub(crate) const ROW_GAP: f32 = 2.;
/// The height a lane is drawn at when nobody has dragged it (DESIGN §5 "Lane
/// heights", REDESIGN §3.6): by kind. An empty lane keeps its kind's height, so
/// dropping the first clip on it moves nothing.
const LANE_VIDEO_H: f32 = crate::layout::redesign::LANE_H_V;
const LANE_AUDIO_H: f32 = crate::layout::redesign::LANE_H_A;
const LANE_SUB_H: f32 = 28.;
/// The range a dragged lane may be held to. Past the top the lane stops
/// growing and the column scrolls instead.
pub(crate) const LANE_MAX_H: f32 = 120.;
/// The gap `lane_row`'s pinned head column puts between the lane label and
/// its status dot (its own `.gap(px(...))`), named so [`LANE_MIN_H`]'s
/// derivation below doesn't repeat the literal blind.
pub(crate) const LANE_HEAD_GAP: f32 = 2.;
/// The status dot's own diameter (`lane_row`'s "Pinned track head" child,
/// its own `.w(px(...)).h(px(...))`).
pub(crate) const LANE_DOT_D: f32 = 4.;
/// The source patch box's side (its slot at the head's left edge is always this wide, shown or not).
pub(crate) const PATCH_W: f32 = 16.;
/// The widest a lane's label may be inside the head (a long track name is ellipsised to it;
/// the head itself stays [`HEAD_W`]): the head less the patch slot, the dot, the two switches and
/// their gaps.
pub(crate) const LANE_NAME_MAX_W: f32 = 68.;
/// The least a lane row may be (DESIGN §5). The head is one row -- dot, name,
/// switches -- so its tallest piece is the 24px switch hit area ([`HIT_MIN`]),
/// not a stack of label, gap and dot; `a_lane_row_fits_what_its_own_head_draws`
/// checks that this still holds. 28 leaves the switches a little air.
pub(crate) const LANE_MIN_H: f32 = 28.;

/// Holds a lane's height to what the bench may draw: [`LANE_MIN_H`] to
/// [`LANE_MAX_H`].
pub(crate) fn clamp_lane_h(h: f32) -> f32 {
    h.clamp(LANE_MIN_H, LANE_MAX_H)
}

/// A lane's key in the view file (`view_state`): its kind's letter and its
/// ord plus one, the same number its head names it by -- "v1", "a2", "s1".
pub(crate) fn lane_key(lane: Lane) -> String {
    let kind = match lane.kind {
        LaneKind::Video => 'v',
        LaneKind::Audio => 'a',
        LaneKind::Subtitle => 's',
    };
    format!("{kind}{}", lane.ord + 1)
}

/// The height a lane has when nobody has dragged it: its kind's own, whether
/// or not it holds anything (no auto-collapse; the layout is stable); on a window
/// shorter than 800 px the short pair (REDESIGN §2.2).
pub(crate) fn default_lane_h(kind: LaneKind) -> f32 {
    let short = SHORT_WINDOW.with(Cell::get);
    match (kind, short) {
        (LaneKind::Video, false) => LANE_VIDEO_H,
        (LaneKind::Video, true) => crate::layout::redesign::LANE_H_V_SHORT,
        (LaneKind::Audio, false) => LANE_AUDIO_H,
        (LaneKind::Audio, true) => crate::layout::redesign::LANE_H_A_SHORT,
        (LaneKind::Subtitle, _) => LANE_SUB_H,
    }
}

thread_local! {
    /// Whether the window is shorter than 800 px (REDESIGN §2.2): lanes are drawn
    /// V 44 / A 32 so four of them still fit. Set by `stance::bench` each frame.
    static SHORT_WINDOW: Cell<bool> = const { Cell::new(false) };
}

/// Tells the bench the window height, so the default lane heights can follow it.
pub(crate) fn set_window_h(height: f32) {
    SHORT_WINDOW.with(|c| c.set(height < 800.));
}

/// The height `lane` is drawn at: the one a hand dragged it to and saved for
/// this project, else [`default_lane_h`]. A saved value that is not a number
/// is ignored, and the result is always held to the lane range.
pub(crate) fn lane_height(player: &Player, lane: Lane) -> f32 {
    let key = lane_key(lane);
    let saved = player
        .lane_h
        .iter()
        .find(|(k, h)| *k == key && h.is_finite())
        .map(|(_, h)| *h);
    let h = saved.unwrap_or_else(|| default_lane_h(lane.kind));
    clamp_lane_h(h)
}

/// Saves the height a lane was dragged to, replacing its earlier one.
pub(crate) fn save_lane_h(saved: &mut Vec<(String, f32)>, lane: Lane, h: f32) {
    let key = lane_key(lane);
    match saved.iter_mut().find(|(k, _)| *k == key) {
        Some(entry) => entry.1 = h,
        None => saved.push((key, h)),
    }
}

/// Forgets a lane's saved height, so it draws at its kind's default again.
pub(crate) fn forget_lane_h(saved: &mut Vec<(String, f32)>, lane: Lane) {
    let key = lane_key(lane);
    saved.retain(|(k, _)| *k != key);
}

/// The three track heights a lane can be set to, from its header's menu, the
/// Timeline menu or the keyboard (Premiere's and Resolution's "track height").
/// Small is the floor a lane may be held to, Medium is the kind's own default
/// and Large is the taller one that shows its thumbnails and waveform in full.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TrackSize {
    Small,
    Medium,
    Large,
}

impl TrackSize {
    pub(crate) const ALL: [TrackSize; 3] = [TrackSize::Small, TrackSize::Medium, TrackSize::Large];

    /// One size taller (`up`) or shorter, held at the ends.
    pub(crate) fn step(self, up: bool) -> Self {
        match (self, up) {
            (TrackSize::Small, true) | (TrackSize::Large, false) => TrackSize::Medium,
            (TrackSize::Medium, true) | (TrackSize::Large, true) => TrackSize::Large,
            (TrackSize::Medium, false) | (TrackSize::Small, false) => TrackSize::Small,
        }
    }

    pub(crate) fn word(self) -> &'static str {
        match self {
            TrackSize::Small => "Small",
            TrackSize::Medium => "Medium",
            TrackSize::Large => "Large",
        }
    }

    /// The size a track-height row sets, if `action` is one of those rows.
    pub(crate) fn of_action(action: crate::keymap::ActionId) -> Option<Self> {
        use crate::keymap::ActionId;
        match action {
            ActionId::TrackSmall => Some(TrackSize::Small),
            ActionId::TrackMedium => Some(TrackSize::Medium),
            ActionId::TrackLarge => Some(TrackSize::Large),
            _ => None,
        }
    }
}

/// Large lanes: a video lane's thumbnails and an audio lane's waveform get
/// the room they need; the subtitle lane's text simply gets more air.
const LANE_LARGE_VIDEO_H: f32 = 96.;
const LANE_LARGE_AUDIO_H: f32 = 72.;
const LANE_LARGE_SUB_H: f32 = 40.;

/// The height a lane of `kind` has at `size`. Medium follows the window
/// (`default_lane_h`), so it stays the height the lane is drawn at unless
/// somebody has set it.
pub(crate) fn size_h(kind: LaneKind, size: TrackSize) -> f32 {
    match (size, kind) {
        (TrackSize::Small, _) => LANE_MIN_H,
        (TrackSize::Medium, _) => default_lane_h(kind),
        (TrackSize::Large, LaneKind::Video) => LANE_LARGE_VIDEO_H,
        (TrackSize::Large, LaneKind::Audio) => LANE_LARGE_AUDIO_H,
        (TrackSize::Large, LaneKind::Subtitle) => LANE_LARGE_SUB_H,
    }
}

/// The size a lane is drawn at: the preset nearest its height. A tie (a
/// subtitle lane's Small and Medium are one height) is Medium, the default.
pub(crate) fn track_size(player: &Player, lane: Lane) -> TrackSize {
    let h = lane_height(player, lane);
    [TrackSize::Medium, TrackSize::Small, TrackSize::Large]
        .into_iter()
        .min_by(|a, b| (size_h(lane.kind, *a) - h).abs().total_cmp(&(size_h(lane.kind, *b) - h).abs()))
        .unwrap_or(TrackSize::Medium)
}

/// Sets a lane to a size. Medium forgets the saved height, so the lane keeps
/// following the window the way it does before anyone touches it.
pub(crate) fn set_track_size(saved: &mut Vec<(String, f32)>, lane: Lane, size: TrackSize) {
    match size {
        TrackSize::Medium => forget_lane_h(saved, lane),
        _ => save_lane_h(saved, lane, size_h(lane.kind, size)),
    }
}

/// How close a dragged lane has to come to a size's height to land on it.
pub(crate) const LANE_SNAP_PX: f32 = 4.;

/// A dragged height, held to the size it sits next to when it is within
/// [`LANE_SNAP_PX`] of one; otherwise the height as dragged.
pub(crate) fn snap_lane_h(kind: LaneKind, h: f32) -> f32 {
    TrackSize::ALL
        .into_iter()
        .map(|size| size_h(kind, size))
        .find(|p| (p - h).abs() <= LANE_SNAP_PX)
        .unwrap_or(h)
}

/// The order the bench draws its lanes top to bottom, the way every NLE stacks them: video
/// lanes climb upward, the highest (`V3`) on top and `V1` just above the audio, and audio
/// lanes descend below. Only the drawing changes; the engine's own lane order and every
/// lane's address stay as they are. Anything else (captions) keeps its engine place,
/// after the audio it sat with.
pub(crate) fn display_order(lanes: &[Lane]) -> Vec<Lane> {
    let mut video: Vec<Lane> = lanes.iter().copied().filter(|l| l.kind == LaneKind::Video).collect();
    video.sort_by_key(|l| std::cmp::Reverse(l.ord));
    let rest = lanes.iter().copied().filter(|l| l.kind != LaneKind::Video);
    video.into_iter().chain(rest).collect()
}

/// Where each lane's band lies in a column of lanes drawn one under another
/// with [`ROW_GAP`] between them, top first: `(lane, top, bottom)`. Each
/// lane takes its own height, so the bands of a mixed stack are not evenly
/// spaced.
#[cfg(test)]
pub(crate) fn lane_bands(stack: &[(Lane, f32)]) -> Vec<(Lane, f32, f32)> {
    let mut top = 0.;
    stack
        .iter()
        .map(|&(lane, h)| {
            let band = (lane, top, top + h);
            top += h + ROW_GAP;
            band
        })
        .collect()
}

/// The lane whose band holds `y`, if any; a y in the gap between two lanes
/// holds neither.
#[cfg(test)]
pub(crate) fn lane_at_y(bands: &[(Lane, f32, f32)], y: f32) -> Option<Lane> {
    bands
        .iter()
        .find(|(_, top, bottom)| y >= *top && y < *bottom)
        .map(|(lane, _, _)| *lane)
}

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
    /// Trace only -- REDESIGN §3.6's "<40px" boundary for the name.
    NoLabel,
    /// Solid ink fill + splice gaps: film scale, below the trace's floor.
    Sliver,
}

/// The narrowest a clip's spine is drawn at all (DESIGN §7): under this the
/// spine would be wider than the box it sits in.
const SPINE_MIN_W: f32 = 3.;

/// `MM:SS` for a clip's own readout -- the bench talks in the film's units,
/// not in seconds with a decimal point.
pub(crate) fn mmss(secs: f64) -> String {
    let s = secs.max(0.).round() as u64;
    match s >= 3600 {
        true => format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60),
        false => format!("{}:{:02}", s / 60, s % 60),
    }
}

/// A clip's thumbnail, filling its box. `Cover` rather than `Contain`: a clip
/// is a strip of film, so it crops rather than letterboxing inside a lane.
pub(crate) fn cover_image(image: std::sync::Arc<gpui::RenderImage>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let fitted = gpui::ObjectFit::Cover.get_bounds(bounds, image.size(0));
            let _ = window.paint_image(fitted, Corners::default(), image, 0, false);
        },
    )
    .size_full()
}

/// A clip's thumbnail as a film strip: the picture at the strip's height, whole and uncropped,
/// repeated along the clip from its left edge -- one frame stretched over a long clip was cut
/// through its middle (and its text) as the clip grew. Only the tiles in view are painted.
pub(crate) fn film_image(image: std::sync::Arc<gpui::RenderImage>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let size = image.size(0);
            let (iw, ih) = (size.width.0 as f32, size.height.0 as f32);
            let h = f32::from(bounds.size.height);
            if iw <= 0. || ih <= 0. || h <= 0. {
                return;
            }
            let tile_w = (h * iw / ih).max(1.);
            let view = window.content_mask().bounds;
            let (left, right) = (f32::from(bounds.origin.x), f32::from(bounds.size.width));
            let first = ((f32::from(view.left()) - left) / tile_w).floor().max(0.);
            let mut x = first * tile_w;
            while x < right && left + x < f32::from(view.right()) {
                let tile = Bounds { origin: point(bounds.origin.x + px(x), bounds.origin.y), size: gpui::size(px(tile_w), px(h)) };
                let _ = window.paint_image(tile, Corners::default(), image.clone(), 0, false);
                x += tile_w;
            }
        },
    )
    .size_full()
}

/// The width ladder (REDESIGN §3.6): the name strip from 40 px, the trace from 24
/// px, and the solid fill below that.
fn tier(span: f32) -> Tier {
    match span {
        s if s >= 120. => Tier::Full,
        s if s >= 40. => Tier::NoChip,
        s if s >= 24. => Tier::NoLabel,
        _ => Tier::Sliver,
    }
}

/// The bench's side gutter, spent inside [`render`]'s own content box.
const BENCH_GUTTER: f32 = 12.;

/// The bench's content: pinned ruler, pinned heads, lane stack. Each lane is
/// drawn at its own height ([`lane_height`]); the lane column takes what the
/// box leaves and scrolls behind the pinned ruler and heads when the lanes
/// add up to more (DESIGN §7). The box no longer sizes the rows, so it is not
/// read here.
pub(crate) fn render(player: &mut Player, cx: &mut Context<Player>) -> impl IntoElement {
    let lanes = display_order(
        &player
            .session
            .as_ref()
            .map_or_else(|| vec![Lane::V1, Lane::A1], PlaybackSession::lanes),
    );
    let position = player.session.as_ref().map_or(0., PlaybackSession::now);
    let scale = player.scale;
    let (picks, pick_links) = player.marks();
    // The scrollbar row is spent only while the film is longer than the
    // window (DESIGN §7), the same test `scrollbar` is mounted on below.
    let scroll = {
        let view = player.view();
        view.duration > view.span()
    };
    let empty = player.session.as_ref().is_some_and(PlaybackSession::is_empty) && player.drag_lane.is_none();
    let rows_h: f32 = lanes.first().map_or(0., |&l| lane_height(player, l));
    let mut rows = Vec::new();
    rows.extend(titles_row::titles_row(player, scale, cx).map(IntoElement::into_any_element));
    for &lane in &lanes {
        let h = lane_height(player, lane);
        rows.push(lane_row(player, lane, h, scale, &picks, &pick_links, cx).into_any_element());
    }
    div()
        .id("bench-content")
        .relative()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(ROW_GAP))
        // The wheel over the WHOLE bench region, not just a bed: the ruler,
        // the strip left of it, the gaps between rows and the space below the
        // last track all answer a notch here with the same mapping every row
        // gives (`Player::timeline_wheel`; the ruler's own former listener
        // folded into this one, so no strip answers a notch twice). A row
        // stops propagation, so a notch over a track never reaches this.
        .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
            this.timeline_wheel(event, cx);
        }))
        // The bench's side gutter, spent here rather than by the `bench` div
        // above, so this element's bounds -- and the catch-all release below
        // with them -- reach the window edge (note at `ui::stance::bench`).
        .px(px(BENCH_GUTTER))
        // The catch-all release: a clip let go of anywhere on the bench that is
        // not a lane row -- left of the tracks, in the gap between two rows, on
        // the empty bench below the last one -- lands on the lane and the x the
        // last live sample of that same drag was over (`Player::drag_lane`,
        // `Player::drag_x`), which is exactly what the shadow was drawn at. A
        // release that reaches no drop target at all is the one thing a drag
        // may never do: a hand that has aimed a clip and let go has made an
        // edit, and "nothing happened" is not one of the answers an editor
        // accepts. A row's own listener stops propagation when it answers, so
        // this only ever runs when none did -- and it would answer with the
        // same frame if it did run, since both read the one sample.
        .on_drop(cx.listener(move |this, drag: &ClipDrag, window, cx| {
            if this.pro_drag.is_some() {
                this.commit_pro(cx);
                return;
            }
            let (Some(lane), Some(idx)) = (this.drag_lane, this.dragged(drag)) else {
                return;
            };
            let x = this
                .drag_x
                .take()
                .unwrap_or_else(|| window.mouse_position().x);
            this.drag_alt = window.modifiers().alt;
            this.move_clip(drag.lane, idx, lane, x, cx);
        }))
        // The same catch-all for the other three things a hand can be holding
        // over the bench -- a library row, a caption already down, a palette
        // row. Each lands on the lane its last live sample was over, through
        // the very door that row's own release uses, so the engine words the
        // refusal when that lane cannot hold it. Without these, letting one go
        // off the rows reached nothing at all.
        // The library drag's own move sample over the bench: below the last lane's bed is the
        // empty stack, where a drop makes a track. Rows answer the same move for their own
        // room, and the two never both claim a pointer, since they split it by y.
        .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<AssetDrag>, _, cx| {
            if !event.bounds.contains(&event.event.position) {
                return;
            }
            let y = f32::from(event.event.position.y);
            let bottom = this
                .lane_beds
                .borrow()
                .iter()
                .map(|(_, bed)| f32::from(bed.bottom()))
                .fold(f32::NEG_INFINITY, f32::max);
            let below = y > bottom;
            let ctrl = event.event.modifiers.control;
            if below != this.drag_below || ctrl != this.drag_ctrl {
                this.drag_below = below;
                this.drag_ctrl = ctrl;
                cx.notify();
            }
        }))
        .on_drop(cx.listener(move |this, drag: &AssetDrag, window, cx| {
            this.end_drag_preview(cx);
            let x = this
                .drag_x
                .take()
                .unwrap_or_else(|| window.mouse_position().x);
            this.drag_alt = window.modifiers().alt;
            let at = this.place_frame(x).0;
            let insert = window.modifiers().control;
            // Let go below the last lane there is no row to aim at: the drop makes the tracks
            // the file needs at the foot of the stack, in the same step as the placement.
            match this.drag_lane {
                Some(lane) => this.drop_source(&drag.0.clone(), drag.1, Some(lane), Some(at), insert, cx),
                None => this.drop_source_below_lanes(&drag.0.clone(), drag.1, Some(at), insert, cx),
            }
        }))
        .on_drop(cx.listener(move |this, drag: &SubDrag, window, cx| {
            let Some(lane) = this.drag_lane else {
                return;
            };
            let x = this
                .drag_x
                .take()
                .unwrap_or_else(|| window.mouse_position().x);
            this.drag_alt = window.modifiers().alt;
            this.move_sub(drag, lane, x, cx);
        }))
        .on_drop(cx.listener(move |this, drag: &SubPick, window, cx| {
            let Some(lane) = this.drag_lane else {
                return;
            };
            let x = this
                .drag_x
                .take()
                .unwrap_or_else(|| window.mouse_position().x);
            this.drag_alt = window.modifiers().alt;
            this.place_sub(drag.0, lane, x, cx);
        }))
        .child(overview::overview(player, cx))
        .child(ruler(player, position, cx))
        .child(
            // The lanes and, over their foot while more lies below, the shared fade -- a lane
            // cut mid-row reads as the end of the list otherwise.
            div()
                .relative()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .child(
            div()
                .id("bench-lanes")
                // Empty room below the last lane is still the bench: the same
                // menu the ruler opens, and the same guard the bed carries --
                // a menu standing here belongs to the press that opened it.
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        if this.context_menu.is_some() {
                            return;
                        }
                        this.open_bench_menu(event.position, cx);
                    }),
                )
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .track_scroll(&player.lanes_scroll)
                .flex()
                .flex_col()
                .gap(px(ROW_GAP))
                .children(rows),
                )
                .child(lanes_fade(player.lanes_scroll.clone())),
        )
        .children(scroll.then(|| overview::scrollbar(player, cx)))
        // The empty timeline says what to do, over the bed of its lanes (no hitbox: a drop
        // still lands on the lane under it).
        .when(empty, |d| {
            d.child(
                div()
                    .absolute()
                    .top(px(overview::OVERVIEW_H + ROW_GAP + RULER_H + ROW_GAP))
                    .left(px(BENCH_GUTTER + HEAD_W))
                    .right(px(BENCH_GUTTER))
                    .h(px(rows_h))
                    .flex()
                    .items_center()
                    .justify_center()
                    .type_style(type_scale::body())
                    .text_color(rgb(INK3()))
                    .child("Drag media here to start"),
            )
        })
        .child(playhead_line(player, position, scale))
}

/// A short, gentle fade over the foot of the lane stack while lanes are hidden below it. Only
/// 18px and never past 60% of the bed's own ground, so the last partly visible lane stays
/// readable. Painted, so it reads the scroll extent of the frame it is drawn in.
fn lanes_fade(scroll: ScrollHandle) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let hidden = f32::from(scroll.max_offset().height) + f32::from(scroll.offset().y);
            if hidden < 2. {
                return;
            }
            let ground = rgb(crate::ui::theme::tokens().well);
            let clear = gpui::Rgba { a: 0., ..ground };
            let soft = gpui::Rgba { a: 0.6, ..ground };
            window.paint_quad(gpui::fill(
                bounds,
                gpui::linear_gradient(180., gpui::linear_color_stop(clear, 0.), gpui::linear_color_stop(soft, 1.)),
            ));
        },
    )
    .absolute()
    .left_0()
    .right_0()
    .bottom_0()
    .h(px(18.))
}

/// The playhead's line carried down through the lanes, over the clips: 2px
/// (3 while playing), lamp colour, a 1px black halo each side. The ruler draws
/// its own part and the pentagon head.
fn playhead_line(player: &Player, position: f64, scale: Scale) -> impl IntoElement + use<> {
    let bed_w = f32::from(player.ruler.get().size.width).max(1.);
    let x = scale.px_at(position).clamp(0., bed_w);
    let w = if player.transport().is_playing() { 3. } else { 2. };
    div()
        .absolute()
        .top(px(overview::OVERVIEW_H + ROW_GAP + RULER_H + ROW_GAP))
        .bottom_0()
        .left(px(BENCH_GUTTER + HEAD_W + x - 1.))
        .w(px(w + 2.))
        .bg(rgba(crate::ui::theme::with_alpha(0x000000, 0xb3)))
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(1.))
                .w(px(w))
                .bg(rgb(LAMP_WHITE())),
        )
}

#[cfg(test)]
mod tests {
    use super::{
        LANE_AUDIO_H, LANE_MAX_H, LANE_MIN_H, LANE_SUB_H, LANE_VIDEO_H, Tier,
        clamp_lane_h, default_lane_h, forget_lane_h, lane_key, save_lane_h, tier,
    };
    use crate::*;

    #[test]
    fn a_clips_duration_reads_m_ss_and_h_mm_ss_over_an_hour() {
        assert_eq!(super::mmss(14.), "0:14");
        assert_eq!(super::mmss(60.), "1:00");
        assert_eq!(super::mmss(3725.), "1:02:05");
    }

    #[test]
    fn a_label_is_whole_cut_with_an_ellipsis_or_left_to_nothing() {
        let at = |ch: char| if ch.is_whitespace() { 3. } else { 6.8 };
        assert_eq!(super::clip::fit_with("grad", 200., at).as_deref(), Some("grad"));
        assert_eq!(super::clip::fit_with("summer_reel_final", 68., at).as_deref(), Some("summer_re…"));
        // One letter and the ellipsis still fit; not even that does, nothing shows.
        assert_eq!(super::clip::fit_with("summer", 20., at).as_deref(), Some("s…"));
        assert_eq!(super::clip::fit_with("summer", 13., at), None);
    }

    #[test]
    fn the_width_ladder_steps_at_its_thresholds() {
        assert!(tier(120.) == Tier::Full && tier(119.9) == Tier::NoChip);
        assert!(tier(40.) == Tier::NoChip && tier(39.9) == Tier::NoLabel);
        assert!(tier(24.) == Tier::NoLabel && tier(23.9) == Tier::Sliver);
        assert!(tier(0.) == Tier::Sliver);
    }

    #[test]
    fn a_lane_without_a_drag_is_drawn_at_its_kinds_height() {
        assert_eq!(default_lane_h(LaneKind::Video), LANE_VIDEO_H);
        assert_eq!(default_lane_h(LaneKind::Audio), LANE_AUDIO_H);
        assert_eq!(default_lane_h(LaneKind::Subtitle), LANE_SUB_H);
        assert_eq!((LANE_VIDEO_H, LANE_AUDIO_H, LANE_SUB_H), (52., 40., 28.));
    }

    #[test]
    fn a_dragged_lane_is_held_between_the_floor_and_the_ceiling() {
        assert_eq!(clamp_lane_h(5.), LANE_MIN_H);
        assert_eq!(clamp_lane_h(500.), LANE_MAX_H);
        assert_eq!(clamp_lane_h(70.), 70.);
        assert_eq!((LANE_MIN_H, LANE_MAX_H), (28., 120.));
    }

    #[test]
    fn a_reset_forgets_the_saved_height_and_a_drag_replaces_it() {
        let mut saved = Vec::new();
        save_lane_h(&mut saved, Lane::A1, 90.);
        save_lane_h(&mut saved, Lane::A1, 70.);
        assert_eq!(
            saved,
            vec![("a1".to_string(), 70.)],
            "a key is replaced, not repeated"
        );
        save_lane_h(&mut saved, Lane::V1, 100.);
        forget_lane_h(&mut saved, Lane::A1);
        assert_eq!(saved, vec![("v1".to_string(), 100.)]);
        forget_lane_h(&mut saved, Lane::S1);
        assert_eq!(saved.len(), 1, "a lane never saved forgets nothing");
    }

    #[test]
    fn a_lane_is_keyed_by_its_kind_letter_and_its_ord_plus_one() {
        assert_eq!(lane_key(Lane::V1), "v1");
        assert_eq!(lane_key(Lane::A1), "a1");
        assert_eq!(lane_key(Lane::S1), "s1");
        assert_eq!(lane_key(Lane::new(LaneKind::Audio, 1)), "a2");
    }
}

#[cfg(test)]
mod display_order_tests {
    use super::*;

    #[test]
    fn video_lanes_climb_above_v1_and_audio_descends_below() {
        let v = |ord| Lane::new(LaneKind::Video, ord);
        let a = |ord| Lane::new(LaneKind::Audio, ord);
        let engine = [v(0), v(1), v(2), a(0), a(1)];
        assert_eq!(
            display_order(&engine),
            vec![v(2), v(1), v(0), a(0), a(1)],
            "V3 on top, then V2, V1, then the audio stack"
        );
    }
}
