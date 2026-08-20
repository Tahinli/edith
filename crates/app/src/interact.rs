//! What a pointer is doing: the drags, the ghosts, the trims and the key repeat.

use crate::*;

/// A library row being dragged: the file and which of its audio streams that
/// row is, which is the whole of what a row names. Where it lands does not
/// change what is inserted.
pub(crate) struct AssetDrag(pub(crate) PathBuf, pub(crate) usize);

/// A clip already on the timeline being dragged: the lane it is on and its index
/// there, which is how every other edit names a clip. Unlike an [`AssetDrag`]
/// nothing is inserted -- the same clip changes lane and keeps the frames it
/// plays -- but where along the bed it is let go is exactly where it lands, less
/// the offset the hand grabbed it at ([`Player::grab`]).
#[derive(Clone, Copy)]
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

/// How wide a clip's edge is as a *target*: the strip at each end where a press
/// means "make this longer or shorter" instead of "move this to another lane".
/// Wide enough to hit, narrow enough that the middle of even a small box is
/// still the body.
pub(crate) const EDGE_W: f32 = 6.;

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
pub(crate) fn drag_scrim(cx: &mut Context<Player>) -> Div {
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
pub(crate) fn divider(split: Split, cx: &mut Context<Player>) -> Div {
    let across = matches!(split, Split::Timeline | Split::Bench);
    div()
        .flex_none()
        .when(across, |d| d.h(px(SPLIT_W)).w_full().cursor_row_resize())
        .when(!across, |d| d.w(px(SPLIT_W)).h_full().cursor_col_resize())
        .bg(rgb(STROKE_DIVIDER()))
        // Lit under the pointer: the second half of "this can be dragged", said
        // before the button goes down rather than after.
        .hover(|s| s.bg(rgb(ACCENT_PRIMARY())))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                this.split_drag = Some(split);
                cx.stop_propagation();
            }),
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

/// The scrollbar's thumb never narrows past this, however long the timeline:
/// a thumb a pixel wide is a thumb no hand can hold. A quarter of the ruler's
/// own seek strip, which is the idiom it sits under.
pub(crate) const SCROLL_THUMB_MIN: f32 = 24.;

/// The thickness of a scrollbar strip's hit area, in the one dimension the
/// strip spans: the height of the horizontal one under the lanes, the width
/// of the vertical one on the beds' right edge. The bar inside it looks 6 px,
/// and the strip that takes the press is thicker for the ruler strip's
/// reason -- a hand must be able to find it without aiming (WCAG 2.5.8).
pub(crate) const SCROLL_HIT: f32 = 14.;

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

/// The keys that are only ever half a chord. gpui delivers a lone modifier
/// press as a keystroke of its own, and taking one as a binding would leave an
/// action that fires the moment the user reaches for any chord that uses it --
/// so a capture waits through them instead.
pub(crate) fn is_bare_modifier(key: &str) -> bool {
    matches!(
        key,
        "control" | "shift" | "alt" | "super" | "platform" | "function" | "fn" | "meta" | "command"
    )
}
