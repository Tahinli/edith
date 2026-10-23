//! What a pointer is doing: the drags, the ghosts, the trims and the key repeat.

use crate::*;
use engine::project::TrimWalls;

/// A library row being dragged: the file and which of its audio streams that
/// row is, which is the whole of what a row names. Where it lands does not
/// change what is inserted.
pub(crate) struct AssetDrag(pub(crate) PathBuf, pub(crate) usize);

/// A clip already on the timeline being dragged: the lane it is on and its index
/// there, which is how every other edit names a clip. Unlike an [`AssetDrag`]
/// nothing is inserted -- the same clip changes lane and keeps the frames it
/// plays -- but where along the bed it is let go is exactly where it lands, less
/// the offset the hand grabbed it at ([`Player::grab`]).
#[derive(Clone)]
pub(crate) struct ClipDrag {
    pub(crate) lane: Lane,
    pub(crate) idx: usize,
    /// The clip that was picked up, so the drop can find it again: gpui freezes
    /// the payload for the whole gesture, and an edit made *during* one -- a
    /// stroke deletes, undoes or pastes, none of which a drag blocks -- ripples
    /// the indices under it. The index alone would then name a different take at
    /// the release, and the drag would move a clip nobody touched (see
    /// [`live_idx`]).
    pub(crate) clip: Clip,
}

/// A *palette* subtitle row being dragged onto a subtitle lane: which track of
/// [`PlaybackSession::subtitles`] it is, which is the whole of what a row names.
/// The window it lands with is the track's own, from its first microsecond to
/// its last cue -- a placement is trimmed after it is placed, like every clip
/// here.
pub(crate) struct SubPick(pub(crate) usize);

/// A subtitle already placed on a lane being dragged: [`ClipDrag`]'s twin, and
/// carrying the placement itself for that struct's reason -- an edit made during
/// the gesture moves the indices gpui froze into the payload, so the drop finds
/// its placement by value ([`Player::dragged_sub`]) rather than by an index that
/// may since have become another caption's.
#[derive(Clone, Copy)]
pub(crate) struct SubDrag {
    pub(crate) lane: Lane,
    pub(crate) idx: usize,
    pub(crate) sub: SubClip,
}

/// A track *header* being dragged: which track the hand took hold of, to be
/// let go over the header of the one whose place it is to take
/// ([`Player::reorder_lane`]). The lane alone -- a track carries its own clips
/// wherever it goes, so unlike a [`ClipDrag`] there is nothing else to name.
#[derive(Clone, Copy)]
pub(crate) struct LaneDrag(pub(crate) Lane);

/// Where a header drag in flight would leave the track in the hand: the lane
/// whose slot it is about to take, and whether the line is drawn at that lane's
/// top edge (it is coming up from below) or its bottom one. The drop indicator
/// every editor draws between two tracks, and the header answer to the ghost a
/// clip drag lays on a lane ([`Ghost`]) -- stale between gestures, which costs
/// nothing because it is drawn only while one is live.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct LaneDrop {
    pub(crate) lane: Lane,
    pub(crate) above: bool,
}

/// Where the drag in flight would leave what it is carrying: the lane the
/// pointer is over, the snapped head the release will commit ([`landing`]), and
/// how long the thing is. Drawn on that lane as a translucent box the size of
/// the take, so a landing is *seen* before the release rather than discovered
/// after it -- the line ([`Player::snap_cue`]) marks the frame, this shows the
/// body. `refused` is a drop the lane cannot take -- a picture over an audio
/// track, a sound over a video one -- tinted rather than silent, because the
/// refusal is coming at the release either way ([`lane_refuses`],
/// [`Project::move_clip`]).
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Ghost {
    pub(crate) lane: Lane,
    pub(crate) start: u32,
    /// Timeline frames, which a speed has already been counted into: the box is
    /// as wide as the clip is *long where it lands*. Zero for a library row of
    /// unknown length, drawn as a head marker.
    pub(crate) frames: u32,
    /// The swatch of the file being carried ([`file_tint`]), so the
    /// ghost reads as the thing in the hand.
    pub(crate) tint: u32,
    pub(crate) refused: bool,
}

/// A clip edge being dragged: which end of which clip, and the timeline frame
/// the pointer has pulled it to. The box on screen is drawn from `to` while this
/// is set and the engine hears about it once, at the release
/// ([`Player::commit_trim`]) -- one edit, one undo step for the whole gesture,
/// exactly as an equalizer drag works.
#[derive(Clone, Copy)]
pub(crate) struct Trim {
    pub(crate) lane: Lane,
    pub(crate) idx: usize,
    pub(crate) edge: Edge,
    /// Where the edge stood at the press: with `to`, the delta the whole group
    /// follows by -- each member's own edge moves this far, clamped to its own
    /// room, which is exactly what the engine commits at the release.
    pub(crate) from: u32,
    /// Already clamped by `PlaybackSession::trim_room`, so the width drawn from
    /// it is the width the release commits -- an edge stops under the pointer
    /// rather than snapping back after the fact.
    pub(crate) to: u32,
    /// The dragged placement's group, so the boxes of everything it names --
    /// clips and captions alike -- follow the edge on screen exactly as the
    /// engine will move them.
    pub(crate) link: Option<u32>,
}

/// A box select in flight: pressed on empty bed at `from`, pulled to `to`
/// (window coordinates). Under [`MARQUEE_SLOP`] it is still a click.
#[derive(Clone, Copy)]
pub(crate) struct Marquee {
    pub(crate) from: Point<Pixels>,
    pub(crate) to: Point<Pixels>,
}

/// How far the pointer must travel before a press on empty bed becomes a box.
pub(crate) const MARQUEE_SLOP: f32 = 4.;

impl Marquee {
    /// Whether the pointer has travelled far enough for this to be a box.
    pub(crate) fn live(&self) -> bool {
        let (dx, dy) = (f32::from(self.to.x - self.from.x), f32::from(self.to.y - self.from.y));
        dx.abs() >= MARQUEE_SLOP || dy.abs() >= MARQUEE_SLOP
    }

    /// Left, top, right, bottom of the box.
    pub(crate) fn edges(&self) -> (Pixels, Pixels, Pixels, Pixels) {
        (
            self.from.x.min(self.to.x),
            self.from.y.min(self.to.y),
            self.from.x.max(self.to.x),
            self.from.y.max(self.to.y),
        )
    }
}

/// Which of the three pro trims a gesture or key makes (roll, slip, slide).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ProKind {
    /// The cut after `idx` moves; both clips at it change, the length does not.
    Roll,
    /// The clip shows another part of its file, in the same slot.
    Slip,
    /// The clip moves between its neighbours, which give and take the frames.
    Slide,
}

/// A shift-drag in flight on a clip edge (roll) or body (slip, slide). Like
/// [`Trim`] it is tracked on the root and the engine hears about it once, at
/// the release -- one edit, one undo step.
#[derive(Clone, Copy)]
pub(crate) struct ProDrag {
    pub(crate) lane: Lane,
    /// The clip the engine call is made on: for a roll, the clip *before* the
    /// cut being dragged (the edge strip's own clip, or the one ahead of it).
    pub(crate) idx: usize,
    pub(crate) kind: ProKind,
    /// The timeline frame under the pointer at the press.
    pub(crate) from: u32,
    /// Timeline frames moved so far, already clamped to the room the engine
    /// answers (slip is clamped by the engine itself at the release).
    pub(crate) by: i64,
}

/// How long a trim edge's handle flashes warn when the pointer runs past a
/// media wall (the source has no more frames that way).
pub(crate) const TRIM_FLASH: std::time::Duration = std::time::Duration::from_millis(120);

/// The media-wall stop an edge is pinned against, live: the edge is kept at a
/// bound while it is pulled through it, and the warn flash is for that stop
/// alone. `since` is when it began, `wall` the frame it is pinned to, so a
/// pointer that stays past the same wall is one stop, not a flash per sample.
#[derive(Clone, Copy)]
pub(crate) struct TrimFlash {
    pub(crate) lane: Lane,
    pub(crate) idx: usize,
    pub(crate) edge: Edge,
    pub(crate) wall: u32,
    pub(crate) since: Instant,
}

impl TrimFlash {
    /// Whether this edge's handle is still flashing.
    pub(crate) fn lit(&self, lane: Lane, idx: usize, edge: Edge) -> bool {
        (self.lane, self.idx, self.edge) == (lane, idx, edge) && self.since.elapsed() < TRIM_FLASH
    }
}

/// The media wall a trim stopped against, if the pointer wanted past it: `at`
/// is where the pointer asked the edge to be, `to` where the engine let it
/// stand, `walls` the room the edge has. `None` when nothing was refused, or
/// when the bound that held is a neighbour or the timeline's own start, which
/// carry no flash -- only the source's end does.
pub(crate) fn media_wall_stop(at: u32, to: u32, walls: TrimWalls) -> Option<u32> {
    if at < walls.lo && to == walls.lo && walls.lo_media {
        return Some(walls.lo);
    }
    if at > walls.hi && to == walls.hi && walls.hi_media {
        return Some(walls.hi);
    }
    None
}

/// The handle width [`trims`] is built on: a box below three of these has no
/// trim zones at all. The zones themselves are [`TRIM_REACH`] either side of
/// each edge, which is wider than this strip, so this is the floor, not the
/// hit size.
pub(crate) const EDGE_W: f32 = 6.;

/// How far a trim's hit zone reaches on *each* side of a clip edge: 24 px
/// centred on the edge, so the hand finds it by aim alone (WCAG 2.5.8,
/// [`HIT_MIN`]). The visible handle of a selected clip is [`TRIM_HANDLE`]
/// wide; the rest of the zone is a target, not a drawing.
pub(crate) const TRIM_REACH: f32 = HIT_MIN / 2.;

/// The drawn trim handle a selected clip shows inside each edge.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const TRIM_HANDLE: f32 = 12.;

/// How far one edge's trim zone reaches, as `(inside, outside)` px from the
/// edge. `room` is the gap to the neighbour on that side (`f32::INFINITY`
/// where there is none): the outside reach stops at the middle of that gap, so
/// where two clips meet the clip under the pointer wins its own half of the
/// join.
///
/// The inside reach shrinks on a clip under 48 px to a quarter of its width,
/// which keeps the middle half of it a move target -- the middle third is
/// what the rule guarantees, and a quarter leaves it room to breathe.
pub(crate) fn trim_zone(width: f32, room: f32) -> (f32, f32) {
    (
        TRIM_REACH.min(width / 4.),
        TRIM_REACH.min(room.max(0.) / 2.),
    )
}

/// A fade handle's own hit zone, at each *top* corner of an audio clip's box --
/// never the trim strip's height, which is [`EDGE_W`] wide down the whole box
/// ([`trims`]) and would swallow a fade press if the two overlapped. Sitting
/// just inside the trim strip rather than on top of it, so one gesture is
/// still exactly one thing: the outer [`EDGE_W`] column lengthens or shortens
/// the clip, this small square at its top corner shapes the fade instead.
pub(crate) const FADE_HANDLE_W: f32 = 10.;
/// Only the label row's own height tall -- a fade handle reaching down into
/// the waveform would sit on top of the body drag and the waveform's own
/// tooltip both.
pub(crate) const FADE_HANDLE_H: f32 = 10.;

/// A fade handle being dragged: which clip, which end (`is_in` for the head's
/// ramp-up, the tail's ramp-down otherwise), and the gesture's own state --
/// `press_x` to measure the pixel delta from, `start` the fade length the
/// press found, `to` what the drag is showing right now, `cap` the clip's own
/// length in frames ([`Project::set_fade_in`]'s own clamp, so the drag never
/// draws past what a release would refuse). One clip, no group: unlike a trim
/// a fade never drags a neighbour, and unlike a caption it never wants a
/// track's walls.
#[derive(Clone, Copy)]
pub(crate) struct FadeDrag {
    pub(crate) lane: Lane,
    pub(crate) idx: usize,
    pub(crate) is_in: bool,
    pub(crate) press_x: Pixels,
    pub(crate) start: u32,
    pub(crate) to: u32,
    pub(crate) cap: u32,
}

/// What a volume-envelope drag holds: one key, or the line between key `i` and key `i + 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GainGrab {
    Key(usize),
    Segment(usize),
}

/// A volume-envelope drag on one audio clip, tracked on the root like a fade is. A key moves in
/// time (between its neighbours and the clip's own source range) and in dB; a segment moves both
/// of its keys up or down by the same dB. `keys` is the envelope at the press, which the release
/// compares against; `shown` is what the drag is showing and what the release commits, in one
/// undo step. Positions are window pixels, and `origin` and `size` are the clip's own box.
#[derive(Clone, Debug)]
pub(crate) struct GainEdit {
    pub(crate) lane: Lane,
    pub(crate) idx: usize,
    pub(crate) grab: GainGrab,
    pub(crate) press: Point<Pixels>,
    pub(crate) origin: Point<Pixels>,
    pub(crate) size: Size<Pixels>,
    pub(crate) keys: Vec<engine::GainKey>,
    pub(crate) shown: Vec<engine::GainKey>,
    /// The clip's own gain, the source range a key may move in, its speed and the timeline's
    /// pixels per frame, read at the press.
    pub(crate) clip_gain: f32,
    pub(crate) in_frame: u32,
    pub(crate) out_frame: u32,
    pub(crate) speed: f64,
    pub(crate) px_per_frame: f32,
    /// For a key: the display dB the press was over, less the key's own. For a segment: the dB
    /// under the press. A move adds the pointer's change to it.
    pub(crate) grab_db: f32,
}

/// Whether a clip box this wide gets its two trim strips at all. Below three
/// handles wide the pair would occlude the whole box: every press on it would
/// trim, and the clip could not be selected, dragged to another lane or picked
/// up by its middle -- which is exactly what a jumpcut leaves behind
/// ([`Player::cut_silences`] manufactures a great many short clips). Above it,
/// what is left between the two strips is a handle's width of body in its own
/// right. A clip too short for its handles is trimmed by zooming in first: the
/// bed is a magnifier, and the strip grows with the box.
pub(crate) fn trims(width: f32) -> bool {
    width >= 3. * EDGE_W
}

/// How wide a clip's box is *drawn*, given the width its own length is worth
/// (`span`). Never under [`HIT_MIN`], even where that is wider than the clip is
/// long: zoomed far out a short take is worth a fraction of a pixel, and a box
/// nobody can put a pointer on is a clip that cannot be selected, dragged, given
/// a menu or reached at all -- which is strictly worse than one drawn a few
/// pixels too wide. The same call [`cue_box`] makes for a mark, and what every
/// editor draws.
///
/// A drawing only: [`Scale::time_at`] still reads the bed, so a press inside the
/// padding names the frame it points at, and the box's head is the clip's own.
pub(crate) fn clip_width(span: f32) -> f32 {
    span.max(HIT_MIN)
}

/// The sheet a card or a menu is painted on: the whole window, and the mouse
/// stops at it. Occluding is what tells gpui that nothing under this sheet is
/// hovered any more (`Hitbox::is_hovered`) -- without it the window carries on
/// hovering behind an open menu and pops *its* tooltip over the menu's items,
/// which is a card being painted over by the thing it covers.
///
/// Every card and every menu takes its sheet from here, so no surface can be
/// drawn over the top of one by having been given a plain scrim.
pub(crate) fn scrim() -> Div {
    div().absolute().inset_0().occlude()
}

/// The body every hanging menu is drawn on: the clip menu, the library row
/// menu and the open choice list, one shape for all three.
///
/// It exists because the three drifted: each carried its own copy of the
/// plate chrome, so styling one (the picker, 2026-08-21) left the other two
/// painting the legacy raised grey -- the user's "right click menu in library
/// is old too". A plate is `panel` with a seam around it and a 6px radius
/// (DESIGN §2/§4); the legacy tree keeps its own raised sheet. One function,
/// so the next change reaches all three or none.
pub(crate) fn menu_plate(id: &'static str, x: f32, y: f32) -> Stateful<Div> {
    // The one popup look (REDESIGN §1.6): every menu starts from `kit::plate`.
    crate::ui::kit::plate(6.)
        .id(id)
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(crate::layout::MENU_W))
        .flex()
        .flex_col()
        .p(px(crate::layout::MENU_PAD))
}

/// The sheet a card with a *slider* in it takes instead: the same scrim, with
/// the window's own drag listeners on it as well.
///
/// A scrim occludes, and occluding is where gpui's hit test stops
/// (`Hitbox::is_hovered`, window.rs:788) -- so while a card is up the root is
/// not hovered anywhere behind it and its `on_mouse_move`/`on_mouse_up` hear
/// nothing at all. Every drag in this window is tracked from the root, because
/// each of them starts on a strip a few pixels wide that the pointer leaves at
/// once ([`Player::drag_move`]), so a card's handles were set by the press and
/// then frozen: the value never followed the hand and the release never wrote.
/// The scrim is the one surface above the occluder that covers the whole card,
/// so the same two listeners go here, and a drag that leaves the card is picked
/// up by the root's copy of them without a seam.
pub(crate) fn drag_scrim(cx: &Context<Player>) -> Div {
    scrim()
        .on_mouse_move(cx.listener(Player::drag_move))
        .on_mouse_up(MouseButton::Left, cx.listener(Player::drag_release))
}

/// The seam between two panels, and the handle that resizes them: a strip
/// [`SPLIT_W`] wide with the resize cursor of its axis on it, which is the whole
/// of how a person finds out the layout moves at all.
///
/// The press is all this element does. The gesture itself is the root's, like
/// every other drag in this window ([`Player::drag_move`]): a 6 px strip is not
/// where the pointer stays, and a divider whose own listeners tracked it would
/// stop following the hand on the first move. `stop_propagation` so the press
/// never reaches the region under it -- a seam over the timeline must not scrub.
///
/// Drawn to the ghost grammar (DESIGN §11.2): nothing at rest -- a room whose
/// every seam is inked is the "crowded" the user named -- then one hairline of
/// dim ink under the pointer, and the same line one step brighter while the
/// hand is actually holding it. The band below is the *hit* area throughout;
/// only one 1 px line inside it is ever painted. `held` is the seam this window is
/// dragging right now ([`Player::split_drag`]), which the pointer leaves on the
/// first move -- so the lit line has to come from the model, not from `hover`.
///
/// What the *hand* aims at is wider than what the eye is given: the press and
/// the resize cursor live on a [`GRAB_W`] band centred on the *line the eye
/// sees* -- one row above the strip, so it reaches past the panel edge the
/// strip stands in front of rather than being centred on the strip -- mounted
/// absolutely so it overlays its neighbours instead of pushing them (the strip
/// keeps its [`SPLIT_W`] of layout, and `layout::split_drag_size` keeps
/// answering in strip coordinates). A 6 px target missed by 5 px is a resize
/// the hand cannot find, which is the same as no resize at all (user
/// 2026-09-09: "panes should be enlargable and shrinkable" -- they already
/// were). The hairline is painted *by the band*, not by the strip, so the
/// whole band lights it: a line raised only by the pixels that already grab
/// the seam tells a hand nothing. The bench's band ([`BENCH_GRAB_H`]) is every
/// other seam's reach: the section head under it is gone (cleanse round 2) and
/// the ruler that starts the bench now answers presses itself, so a deeper
/// band would eat scrubs at the film's own top edge.
///
/// A double press resets the seam to the window's own share
/// ([`Splits::clear`]): the cheap way back from a layout dragged somewhere
/// unusable, and the one every editor's dividers answer to.
pub(crate) fn divider(split: Split, held: bool, cx: &mut Context<Player>) -> Div {
    let across = matches!(split, Split::Timeline | Split::Bench);
    // Named per seam anyway ([`BENCH_GRAB_H`] is [`GRAB_W`] since the bench
    // lost its section head): every neighbour's first row carries content the
    // hand may want instead -- the bench's ruler scrubs, the dock's rows start
    // at its very edge, the legacy timeline's are the edit toolbar's buttons.
    let grab = match split {
        Split::Bench => BENCH_GRAB_H,
        _ => GRAB_W,
    };
    // Named per seam: `group_hover` resolves by name through one global map,
    // so two dividers sharing a name would light each other.
    let group = format!("seam-{split:?}");
    div()
        .flex_none()
        .when(across, |d| d.h(px(SPLIT_W)).w_full())
        .when(!across, |d| d.w(px(SPLIT_W)).h_full())
        .relative()
        .child(
            div()
                .absolute()
                .group(group.clone())
                .when(across, |d| {
                    d.left_0()
                        .right_0()
                        .top(px(-1.))
                        .h(px(grab))
                        .cursor_row_resize()
                })
                .when(!across, |d| {
                    d.top_0()
                        .bottom_0()
                        .left(px(-1.))
                        .w(px(grab))
                        .cursor_col_resize()
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        match event.click_count >= 2 {
                            // The reset is written where a release would have written
                            // the drag, the same small round trip: a seam put back by
                            // hand must not come back on the next launch.
                            true => {
                                this.split_drag = None;
                                this.splits.clear(split);
                                crate::layout::save_stance_splits(&this.splits);
                            }
                            false => {
                                this.split_drag = Some(split);
                                this.split_press = Some((event.position, false));
                            }
                        }
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                // The hairline the eye is given, painted *inside* the band and
                // one pixel thick, so the whole band -- not the 6 px strip it
                // hangs on -- is what lights it. A line only the pixels that
                // already hit the seam can raise tells the hand nothing it did
                // not already know.
                .child(
                    div()
                        .absolute()
                        .when(across, |d| {
                            d.left_0().right_0().top(px(1.)).h(px(1.)).border_t_1()
                        })
                        .when(!across, |d| {
                            d.top_0().bottom_0().left(px(1.)).w(px(1.)).border_l_1()
                        })
                        .border_color(match held {
                            true => gpui::Hsla::from(rgb(INK3())),
                            false => gpui::transparent_black(),
                        })
                        // Lit under the pointer: the second half of "this can be
                        // dragged", said before the button goes down rather than
                        // after.
                        .when(!held, |d| {
                            d.group_hover(group, |s| s.border_color(rgb(STROKE_DIVIDER())))
                        }),
                ),
        )
}

/// The grab on a lane's bottom edge (DESIGN §5 "Lane heights"): 2px inside the
/// lane's edge and 12px past it -- the 2px [`ROW_GAP`] plus 10px of the lane
/// below. It starts at the head's right edge, so the bed carries it and a head
/// keeps its own bottom pixels for its reorder grab and switches. It stays out
/// of the lane's bottom 2px and so does not cover the key marks drawn on a
/// clip's foot (`ui/bench_stance/lane.rs`), which must take their own presses.
/// A press starts the drag (`Player::lane_resize`, read by `drag_move`); a
/// double-click puts the lane back to its kind's default.
pub(crate) fn lane_resize_band(
    lane: Lane,
    h: f32,
    left: f32,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    const ABOVE_H: f32 = 2.;
    const BELOW_H: f32 = 12.;
    let key = crate::ui::bench_stance::lane_key(lane);
    // Named per lane: `group_hover` resolves by name through one global map.
    let group = format!("lane-resize-{key}");
    div()
        .id(SharedString::from(format!("bench-lane-resize-{key}")))
        .absolute()
        .group(group.clone())
        .left(px(left))
        .right_0()
        .top(px(h - ABOVE_H))
        .h(px(ABOVE_H + BELOW_H))
        .cursor_row_resize()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                match event.click_count >= 2 {
                    true => this.reset_lane_h(lane, cx),
                    // The grab is the pointer's y less the height the lane
                    // has now, so a drag sets the height from the pointer
                    // alone (`Player::drag_lane_resize`) and never jumps.
                    false => {
                        this.lane_resize = Some((lane, f32::from(event.position.y) - h));
                        cx.notify();
                    }
                }
                cx.stop_propagation();
            }),
        )
        // The hairline at the lane's edge, lit under the pointer -- the same
        // "this can be dragged" cue a seam gives (`divider`).
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(ABOVE_H - 1.))
                .h(px(1.))
                .group_hover(group, |s| s.bg(rgb(STROKE_DIVIDER()))),
        )
}

/// A press that stops here. What every card's body hands its scrim: the scrim
/// closes the card on a press, and the card is painted after it, so this listener
/// runs first (gpui dispatches topmost-first, window.rs:3705) and a press meant
/// for a button never closes the card out from under its own click -- the rule
/// the menus already follow ([`Player::library_card`]).
pub(crate) fn swallow(_: &MouseDownEvent, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

/// How close to an edge a dragged clip has to be let go for it to land *on* it:
/// the snap every timeline has, in pixels rather than frames so that it feels
/// the same at every zoom. Narrower than [`EDGE_W`] -- a hand aiming between two
/// takes must still be able to leave a gap of a few frames there.
pub(crate) const SNAP_PX: f64 = 5.;

/// The same magnet, wider, for a drag's own *release* -- a library row or a
/// clip let go of, rather than an edge nudged one frame at a time. The user's
/// own complaint ("to drop at 0.0 you must be exactly at 0.0") is a 5px
/// window read at zoom: past a couple of minutes on screen a pixel is worth
/// several frames, and 5px of aim is not forgiving enough to feel like a
/// magnet at all. Twice [`SNAP_PX`], [`Scale::drop_snap_frames`].
pub(crate) const DROP_SNAP_PX: f64 = 10.;

/// The scrollbar's thumb never narrows past this, however long the timeline:
/// a thumb a pixel wide is a thumb no hand can hold. A quarter of the ruler's
/// own seek strip, which is the idiom it sits under.
pub(crate) const SCROLL_THUMB_MIN: f32 = 24.;

/// The thickness of a scrollbar bar as drawn, in the one dimension the strip
/// spans: the height of the horizontal one under the lanes, the width of the
/// vertical one on the beds' right edge. This is what the layout spends.
pub(crate) const SCROLL_BAR_H: f32 = 6.;

/// The hit area of a scrollbar strip, in the same dimension: a hand must be
/// able to find it without aiming (WCAG 2.5.8, [`HIT_MIN`]). The strip's row
/// is [`SCROLL_ROW_H`] tall; the band reaches [`SCROLL_HIT_UP`] above it into
/// the bottom of the lanes and [`SCROLL_HIT_DOWN`] below it, so the band is
/// at least [`HIT_MIN`] and the lanes give up no more than 8px for it.
pub(crate) const SCROLL_HIT: f32 = HIT_MIN;

/// Where a held key would land, which is the whole of what auto-repeat has to
/// know. [`Player::repeat_scope`] answers it from the same state the handler
/// walks below itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Repeat {
    /// A card with values in it owns the keyboard: equalizer, colour, speed or
    /// silence. Its arrows are sliders.
    Card,
    /// Nobody does, so the keymap answers -- and only one pair of its actions
    /// is a value being moved rather than a thing being done once.
    Keymap,
    /// A stroke is being captured, an export is running, or an overlay with no
    /// value in it is up. Nothing there is worth a repeat.
    Nothing,
}

/// Whether a *held* stroke means it again. One press is always one action; a
/// hold is only ever a value running, so this is what tells the two apart.
///
/// The cards' arrows, because that is what every one of them moves a slider
/// with -- and only the arrows, so the equalizer's `r` cannot flatten five
/// bands forty times a second and the silence card's `enter` cannot cut forty
/// places again on the next tick. Outside a card the volume pair and nothing
/// else: play, cut, delete, save, export and every other binding is a one-shot,
/// exactly as it was when the handler filtered every held key alike.
pub(crate) fn repeats(scope: Repeat, key: &str, action: Option<ActionId>) -> bool {
    match scope {
        Repeat::Card => matches!(key, "up" | "down" | "left" | "right"),
        Repeat::Keymap => matches!(
            action,
            Some(
                ActionId::VolumeUp
                    | ActionId::VolumeDown
                    // A zoom is a value being moved as much as a level is:
                    // held, it runs from the whole timeline down to a handful
                    // of frames and stops there. The fit is one press.
                    | ActionId::ZoomIn
                    | ActionId::ZoomOut
            )
        ),
        Repeat::Nothing => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walls(lo_media: bool, hi_media: bool) -> TrimWalls {
        TrimWalls {
            lo: 1,
            hi: 9,
            lo_media,
            hi_media,
        }
    }

    #[test]
    fn a_stop_flashes_only_at_a_media_wall_the_pointer_wanted_past() {
        // Past the source's end: flash. Past a neighbour, or inside the room:
        // nothing to say.
        assert_eq!(media_wall_stop(12, 9, walls(false, true)), Some(9));
        assert_eq!(media_wall_stop(12, 9, walls(false, false)), None);
        assert_eq!(media_wall_stop(0, 1, walls(true, false)), Some(1));
        assert_eq!(media_wall_stop(0, 1, walls(false, true)), None);
        assert_eq!(media_wall_stop(5, 5, walls(true, true)), None);
        // A clamp that did not clamp the pointer's ask is not a stop.
        assert_eq!(media_wall_stop(9, 9, walls(true, true)), None);
    }

    #[test]
    fn a_flash_lights_its_own_edge_for_its_window_only() {
        let lane = Lane::V1;
        let fresh = TrimFlash {
            lane,
            idx: 2,
            edge: Edge::End,
            wall: 9,
            since: Instant::now(),
        };
        assert!(fresh.lit(lane, 2, Edge::End));
        assert!(!fresh.lit(lane, 2, Edge::Start));
        assert!(!fresh.lit(lane, 3, Edge::End));
        let spent = TrimFlash {
            since: Instant::now() - TRIM_FLASH - Duration::from_millis(1),
            ..fresh
        };
        assert!(!spent.lit(lane, 2, Edge::End));
    }
}
