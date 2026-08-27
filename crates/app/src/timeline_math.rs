//! The timeline's arithmetic: takes, trims, snapping and envelopes.

use crate::*;

/// Whether this clip is a whole take, i.e. whether deleting it may close the
/// hole under it: a take is what the first pair of lanes carries between them,
/// `V1`'s picture and the sound grouped with it, and dropping one moves the
/// frames after it on every lane. A caption a hand grouped with the clip
/// counts as its partner too: the group is what pairs, not the lane's kind --
/// and a group answers `true` from *whichever* member's lane the question is
/// asked from, added row or not, because a grouped delete
/// ([`PlaybackSession::delete_clip`] -> `Project::delete_members`) closes
/// every member's own lane by its own span already, not just the first pair's.
///
/// Everything else is a half or a layer, and is *lifted* instead: a half whose
/// picture was lifted (what a lift leaves behind) has no take to ripple, and an
/// *ungrouped* clip on a further lane is laid over the timeline rather than
/// part of it -- closing a hole under it would drag the take beneath out of
/// step with it.
pub(crate) fn whole_take(session: &PlaybackSession, lane: Lane, idx: usize) -> bool {
    let Some(clip) = session.lane_clips(lane).get(idx) else {
        return false;
    };
    let paired = || {
        session
            .lanes()
            .into_iter()
            .filter(|&other| other != lane)
            .flat_map(|other| {
                let clips = session.lane_clips(other).iter().map(|c| c.link);
                let subs = session.sub_lane(other).iter().map(|s| s.link);
                clips.chain(subs)
            })
            .any(|link| link.is_some() && link == clip.link)
    };
    match (lane.kind, lane.ord) {
        // A clip a hand grouped closes its own lane wherever it sits:
        // `Project::delete_members` already cuts every member's own span out
        // of its own lane, whichever lanes those are -- so a caption or clip
        // grouped onto an added row closes its hole exactly as `V1`/`A1`'s
        // first pair does, not just the pair itself.
        _ if paired() => true,
        (_, 1..) => false,
        // The picture of a take -- unless the take has been taken apart: a
        // detached picture (a group id no other lane carries, which is also what
        // a lift of the sound leaves) is a half like the sound is, and a ripple
        // under it would drag away the very half it was detached from. A clip in
        // no group at all is not a half but a placement, and on `V1` a placement
        // is the take there is.
        (LaneKind::Video, _) => clip.link.is_none(),
        // The sound of a take, only while the take is still there: its group is
        // carried by a clip on some other lane. Reached only when `paired()` is
        // already false, so this is always false -- kept as its own arm for the
        // reason a lone `A1` clip is not a take, not because it does the work.
        (LaneKind::Audio, _) => false,
        // A subtitle lane carries no `Clip` at all, so the lookup above already
        // returned: never a take, and never rippled as one.
        (LaneKind::Subtitle, _) => false,
    }
}

/// The clip a Group would pair this one with: the first clip on another track,
/// in the order the lanes are drawn, covering exactly the same frames and not in
/// this clip's group already. Exactly the same frames because that is all a
/// group id can mean (engine `links_are_consistent`), which is what leaves
/// nothing for a second click to choose. `None` when no track has one, and the
/// notice says so.
pub(crate) fn span_partner(
    session: &PlaybackSession,
    lane: Lane,
    idx: usize,
) -> Option<(Lane, usize)> {
    let clip = *session.lane_clips(lane).get(idx)?;
    let matches = |other: Lane| {
        let i = session.lane_clips(other).iter().position(|c| {
            (c.start, c.end()) == (clip.start, clip.end())
                && !(c.link.is_some() && c.link == clip.link)
        })?;
        Some((other, i))
    };
    // Sound before picture (and picture before sound): "group this" means the
    // other half of the take, and a project whose audio lane was added after a
    // second video one has that half *after* the layer in storage order -- which
    // is the order the lanes come in. A same-kind lane is still groupable (V1
    // and V2 may be one take), but only where no opposite one covers the span.
    let (opposite, same): (Vec<Lane>, Vec<Lane>) = session
        .lanes()
        .into_iter()
        .filter(|&other| other != lane)
        .partition(|other| other.kind != lane.kind);
    opposite.into_iter().chain(same).find_map(matches)
}

/// What the timeline has picked: every clip and caption a hand ctrl-clicked
/// into the selection, in click order and with no two the same. The last of
/// them is the [`anchor`](Selection::anchor) -- the one every action that is
/// about *one* thing (delete, lift, copy, the equalizer) acts on -- and the
/// whole list is what a manual Group names to the engine.
///
/// Indices move under every edit, exactly as the single mark this replaces
/// did, so every edit that renumbers a lane clears the picks it cannot keep
/// honest. Pure and engine-free, so a test can build one without a window.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Selection {
    /// `(lane, index into that lane's own list)` -- a subtitle lane's index
    /// space for a caption, exactly the pair a click was told about.
    picks: Vec<(Lane, usize)>,
}

impl Selection {
    /// Nothing picked: what the timeline starts as and what an edit that moved
    /// indices leaves behind.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The one a plain click and every selection key leave: the whole of the
    /// selection, one pick. A click names a thing, and the things it had been
    /// naming along with it are no longer named.
    pub(crate) fn set_one(&mut self, pick: (Lane, usize)) {
        self.picks.clear();
        self.picks.push(pick);
    }

    /// A ctrl-click: the pick joins the selection if it was not in it and
    /// leaves if it was -- the toggle that assembles a group by hand. The
    /// order is the order they were picked in, and the anchor rides on it.
    pub(crate) fn toggle(&mut self, pick: (Lane, usize)) {
        match self.picks.iter().position(|&p| p == pick) {
            Some(at) => {
                self.picks.remove(at);
            }
            None => self.picks.push(pick),
        }
    }

    /// The last pick: what "the selected clip" is to every action that acts on
    /// one thing. The click order puts it under the hand, which is where the
    /// eye is.
    pub(crate) fn anchor(&self) -> Option<(Lane, usize)> {
        self.picks.last().copied()
    }

    /// Whether `pick` is one of the selection's -- the question a right-click
    /// asks before it decides whether the menu is about the selection or
    /// about the clip under it.
    pub(crate) fn contains(&self, pick: (Lane, usize)) -> bool {
        self.picks.contains(&pick)
    }

    /// Every pick, in click order: what a manual Group names to the engine.
    pub(crate) fn picks(&self) -> &[(Lane, usize)] {
        &self.picks
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.picks.is_empty()
    }

    /// A pick that joins without disturbing the rest -- Select All's builder,
    /// which names every placement there is and cannot be a `set_one`.
    pub(crate) fn add(&mut self, pick: (Lane, usize)) {
        if !self.picks.contains(&pick) {
            self.picks.push(pick);
        }
    }
}

/// The lane stack's thumb over a track `h` px tall: `(y, height)` of the box
/// standing for the visible share of a stack `content` px tall in a box
/// `box_h` px tall, taken down by `scrolled` px. Full track when the whole
/// stack is on screen, never shorter than
/// [`SCROLL_THUMB_MIN`] when it is not, and never off the track either way.
pub(crate) fn lanes_thumb(track: f32, content: f32, box_h: f32, scrolled: f32) -> (f32, f32) {
    if track <= 0. || content <= 0. || box_h >= content {
        return (0., track.max(0.));
    }
    let height = (track * box_h / content).clamp(SCROLL_THUMB_MIN, track);
    let y = (scrolled / (content - box_h) * (track - height)).clamp(0., track - height);
    (y, height)
}

impl Selection {
    pub(crate) fn len(&self) -> usize {
        self.picks.len()
    }

    pub(crate) fn clear(&mut self) {
        self.picks.clear();
    }
}

/// Whether a click marks this box: any pick on it always, and any box sharing
/// a group id with a pick -- the clip the anchor was ctrl-clicked beside, and
/// the caption a hand pinned to a clip. A group whose only member is picked
/// marks that member alone, which is what makes a detached half separately
/// deletable.
///
/// `pick_links` are the group ids of the picks themselves, read by the caller
/// off the session in the same order as `picks` -- the one fact this pure
/// question cannot know.
pub(crate) fn marked(
    here: (Lane, usize),
    link: Option<u32>,
    picks: &[(Lane, usize)],
    pick_links: &[Option<u32>],
) -> bool {
    picks.contains(&here) || (link.is_some() && pick_links.contains(&link))
}

/// The clip a trim is *showing*, worked out the way `Project::trim` will write
/// it: the timeline room the edge leaves is turned into source frames by the one
/// conversion that exists for it ([`Speed::fit`]), and the box is drawn from
/// that. The preview and the commit are then the same arithmetic -- a box let go
/// of stays where the hand left it at every rate. Assigning the timeline count
/// straight to the source field, as this used to, drew a speeded clip's tail
/// moving at the wrong rate (it snapped on release) and drew a *head* trim
/// moving the clip's other edge, since the length it implied was not the length
/// the release would commit.
///
/// A still grows forward from source frame 0 instead: every frame of it is the
/// same picture, so there is no earlier one for an in-point to walk back to.
///
/// Room too narrow to hold one source frame is the edit the engine refuses, and
/// the box is drawn unchanged rather than as something that will not be
/// committed.
pub(crate) fn trimmed_clip(clip: Clip, edge: Edge, to: u32, still: bool) -> Clip {
    // An edge that has not moved is not an edit, and `Project::trim` refuses it
    // as one: the press that starts a drag must draw the clip it pressed, not a
    // clip a rounding narrower.
    if to
        == match edge {
            Edge::Start => clip.start,
            Edge::End => clip.end(),
        }
    {
        return clip;
    }
    match edge {
        Edge::Start => {
            // What survives is measured from the *end* -- the frames that stay
            // play what they always played, which is what makes this a trim.
            let Some(keep) = clip.speed.fit(clip.end().saturating_sub(to)) else {
                return clip;
            };
            match still {
                true => Clip {
                    fade_in: 0,
                    fade_out: 0,
                    in_frame: 0,
                    out_frame: keep,
                    start: to,
                    ..clip
                },
                false => Clip {
                    fade_in: 0,
                    fade_out: 0,
                    in_frame: clip.out_frame - keep.min(clip.out_frame),
                    start: to,
                    ..clip
                },
            }
        }
        Edge::End => match clip.speed.fit(to.saturating_sub(clip.start)) {
            Some(keep) => Clip {
                fade_in: 0,
                fade_out: 0,
                out_frame: clip.in_frame + keep,
                ..clip
            },
            None => clip,
        },
    }
}

/// The cut odometer's own arithmetic (DESIGN.md §6): where `,` `.` land from
/// `idx`, `stride` cuts forward or back among `len` of them. Clamped rather
/// than wrapping -- an odometer that wraps at the last cut reads as landing
/// back at the first, which is not "next" -- so a walk already at either end
/// simply holds there, the way [`Player::step`]'s own ends do.
pub(crate) fn walk_cut(idx: usize, len: usize, forward: bool, stride: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let delta = stride.min(len - 1) as isize;
    let at = if forward {
        idx as isize + delta
    } else {
        idx as isize - delta
    };
    at.clamp(0, len as isize - 1) as usize
}

/// The no-aim trim's own arithmetic (DESIGN.md §6): `current` moved `dir`
/// frames (usually ±1, a held key's ratchet) and clamped into the room
/// [`engine::PlaybackSession::trim_room`] already answered -- the same wall a
/// pointer drag is clamped to, so a keyboard trim can never draw an edge the
/// engine would refuse.
pub(crate) fn nudge_edge(current: u32, dir: i32, lo: u32, hi: u32) -> u32 {
    (i64::from(current) + i64::from(dir)).clamp(i64::from(lo), i64::from(hi)) as u32
}

/// Whether a loop-trim window ([`ActionId::LoopTrim`]) has been played out to
/// its far edge and wants restarting at its near one: pure so the pump's own
/// tick ([`Player::pump`]) is one `if` over a fact this decides. `None` --
/// loop-trim off -- never restarts anything.
pub(crate) fn should_loop_restart(frame: u32, window: Option<(u32, u32)>) -> bool {
    window.is_some_and(|(_, hi)| frame >= hi)
}

/// Which window is actually looping, in priority order: the i/o range
/// ([`Player::range`]) beats the subject-cut trim ([`Player::loop_trim`])
/// beats the whole timeline -- but only once *something* is already armed
/// (`loop_on` or `loop_trim`). A bare in/out mark with neither on is an
/// export mark first, not a loop request, so it answers `None` here and
/// never restarts anything on its own.
pub(crate) fn active_loop_window(
    loop_on: bool,
    range: Option<(u32, u32)>,
    loop_trim: Option<(u32, u32)>,
) -> Option<(u32, u32)> {
    if !loop_on && loop_trim.is_none() {
        return None;
    }
    range.or(loop_trim)
}

/// Where a play resumes from on crossing into [`Transport::Ended`]
/// (`Player::pump`): the active window's own near edge
/// ([`active_loop_window`]) when one is armed, *even when its far edge is
/// the film's own last frame* -- half of all cuts in a two-clip film sit on
/// the final clip (DESIGN.md §6), so that coincidence is not an edge case
/// and must not read as "reached the end, stop". Falls back to the
/// whole-timeline loop's top, and `None` -- nothing armed -- restarts
/// nothing, which is the halt this crossing otherwise means.
pub(crate) fn loop_restart_frame(
    loop_on: bool,
    range: Option<(u32, u32)>,
    loop_trim: Option<(u32, u32)>,
) -> Option<u32> {
    match active_loop_window(loop_on, range, loop_trim) {
        Some((lo, _)) => Some(lo),
        None => loop_on.then_some(0),
    }
}

/// How many timeline frames a stretch of a subtitle track is worth: the one
/// conversion between the two clocks a caption has -- microseconds for its
/// words, frames for where it sits -- and the app-side twin of the engine's own
/// (`Project::trim_sub_room`). Never zero: a placement of no frames is the one
/// [`Project::place_sub`] refuses as empty, and a track shorter than a frame is
/// still a caption somebody dragged.
/// A fade handle's drag, turned into timeline frames: `dx` pixels at `pps`
/// pixels per second, at `fps` frames per second -- the one conversion a
/// fade-in or fade-out handle needs, and the app-side twin of every other
/// pixel-to-frame door on this bed ([`Scale::time_at`]). Zero for a
/// degenerate scale (nothing drawn to drag against), never negative on its
/// own -- the caller adds this to the fade's length before clamping to the
/// clip's own room, so a delta can still shrink a fade by coming back signed.
pub(crate) fn fade_delta_frames(dx: f32, pps: f64, fps: f64) -> i64 {
    if pps <= 0. {
        return 0;
    }
    ((f64::from(dx) / pps) * fps).round() as i64
}

pub(crate) fn frames_of_us(us: i64, fps: f64) -> u32 {
    match fps.is_finite() && fps > 0. {
        true => ((us as f64) * fps / 1e6)
            .round()
            .clamp(1., f64::from(u32::MAX)) as u32,
        false => 1,
    }
}

/// The *placement* a trim is showing, the same way for a caption:
/// [`Project::trim_sub`] moves the head or the tail and the window follows it,
/// so the box on screen is the frames alone -- the words it will keep are the
/// engine's arithmetic at the release, and nothing here draws them.
///
/// An edge that has not moved draws the placement it pressed, and one dragged
/// past its other end keeps the one frame the engine's own walls always leave.
pub(crate) fn trimmed_sub(sub: SubClip, edge: Edge, to: u32) -> SubClip {
    match edge {
        Edge::Start => {
            let to = to.min(sub.end() - 1);
            SubClip {
                start: to,
                frames: sub.end() - to,
                ..sub
            }
        }
        Edge::End => SubClip {
            frames: to.max(sub.start + 1) - sub.start,
            ..sub
        },
    }
}

/// Which index on its own lane a dragged clip is at *now*: the one it was picked
/// up at while nothing has moved, and wherever the clip itself has slid to when
/// an edit during the drag rippled the lane's indices -- a delete, an undo or a
/// paste from a stroke, none of which gpui's frozen drag payload hears about.
/// `None` when the clip is gone altogether, and then the drop is not an edit at
/// all: moving whatever slid into its place is the one thing the hand did not
/// ask for. A lane's clips are disjoint and sorted, so at most one of them can
/// be the clip that was picked up. Generic over what a lane holds, because a
/// [`SubClip`] is dragged under exactly the same rule.
pub(crate) fn live_idx<T: Copy + PartialEq>(clips: &[T], idx: usize, clip: T) -> Option<usize> {
    match clips.get(idx) {
        Some(&at) if at == clip => Some(idx),
        _ => clips.iter().position(|&c| c == clip),
    }
}

/// Which index a lane's mark is on after that lane was renumbered: the caption
/// that starts at `start`, and `None` when it is not there any more.
///
/// The mark on a caption is an index like a clip's ([`Player::selected`]), and a
/// subtitle lane both *inserts in start order* ([`engine::Project::place_sub`])
/// and removes, so any placement or lift renumbers every caption after it and an
/// index left as it was names a neighbour -- the caption the next Delete would
/// take. So the mark is read off the caption's start frame before such an edit
/// and found again by it after: captions on a lane are disjoint and in order, so
/// a start frame names exactly one of them and is the identity an index is not.
pub(crate) fn sub_mark(subs: &[SubClip], start: u32) -> Option<usize> {
    subs.iter().position(|s| s.start == start)
}


/// Scales an envelope to its own loudest sample, so a quietly mastered source
/// still draws as a shape. The fixtures peak around an eighth of full scale, and
/// an eighth of a 30 px lane is a flat line -- which says "silent" about a file
/// that is not.
pub(crate) fn normalise(mut peaks: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    let loudest = peaks.iter().fold(0f32, |m, &(lo, hi)| m.max(-lo).max(hi));
    if loudest > 0. {
        for (lo, hi) in &mut peaks {
            *lo /= loudest;
            *hi /= loudest;
        }
    }
    peaks
}

/// The min/max envelope of `peaks` over the source seconds `from..to`, as
/// `(x, top, bottom)` columns of a `w` x `h` box. Every point is inside that box
/// -- a clip's waveform cannot paint over its neighbour -- and every column is
/// at least a pixel tall, so silence reads as a line through the middle rather
/// than as a polygon with no area.
pub(crate) fn envelope(
    peaks: &[(f32, f32)],
    from: f64,
    to: f64,
    w: f32,
    h: f32,
) -> Vec<(f32, f32, f32)> {
    if peaks.is_empty() || w <= 0. || h <= 0. {
        return Vec::new();
    }
    let cols = ((w / WAVE_COL).ceil().max(1.) as usize).min(WAVE_COLS_MAX);
    let mid = h / 2.;
    (0..=cols)
        .map(|col| {
            let along = col as f64 / cols as f64;
            let at = from + (to - from) * along;
            // Casting a float to an integer saturates in Rust, so a source
            // second past the end of the peaks clamps rather than wrapping.
            let bucket = ((at * f64::from(WAVE_BPS)) as usize).min(peaks.len() - 1);
            let (lo, hi) = peaks[bucket];
            let top = (mid - hi.clamp(0., 1.) * mid).min(mid - 0.5);
            let bottom = (mid - lo.clamp(-1., 0.) * mid).max(mid + 0.5);
            (w * along as f32, top.max(0.), bottom.min(h))
        })
        .collect()
}

/// Records its parent's laid-out box: gpui hands a mouse listener the window
/// position only, and the ruler sits behind the panel's padding. Paints
/// nothing and takes no hitbox of its own, so the click still reaches the bar.
pub(crate) fn bounds_probe(into: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(move |bounds, _, _| into.set(bounds), |_, _, _, _| ())
        .absolute()
        .size_full()
}

/// [`bounds_probe`] for a box something *else* is laid out against: it also asks
/// for one more frame whenever the height changed.
///
/// A measurement is read by the frame after the one that took it, and a notice
/// arriving over a paused picture repaints exactly once -- so without this the
/// cue would sit under that notice until something else happened to draw, which
/// on a paused window can be never.
pub(crate) fn height_probe(into: Rc<Cell<Pixels>>) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            // Only on a change: an unconditional request is a repaint loop.
            if into.replace(bounds.size.height) != bounds.size.height {
                window.request_animation_frame();
            }
        },
        |_, _, _, _| (),
    )
    .absolute()
    .size_full()
}

/// Where along an element a click landed, 0..1. An element that was never
/// painted has no width, and reads as its start rather than as NaN.
pub(crate) fn frac_along(x: Pixels, bounds: Bounds<Pixels>) -> f32 {
    if bounds.size.width <= px(0.) {
        return 0.;
    }
    ((x - bounds.left()) / bounds.size.width).clamp(0., 1.)
}

/// Where along an element a click landed, in pixels from its own left edge:
/// [`frac_along`] in the units the timeline is drawn in, since a [`Scale`]
/// measures in pixels and not in shares of a bed. Clamped to the element -- a
/// drag that slid off the end names its end -- and an element that was never
/// painted reads as its start.
pub(crate) fn px_along(x: Pixels, bounds: Bounds<Pixels>) -> f32 {
    f32::from(x - bounds.left()).clamp(0., f32::from(bounds.size.width).max(0.))
}

/// [`px_along`] turned through a right angle, for the lane stack's scrollbar:
/// how far down its track a press landed.
pub(crate) fn px_down(y: Pixels, bounds: Bounds<Pixels>) -> f32 {
    f32::from(y - bounds.top()).clamp(0., f32::from(bounds.size.height).max(0.))
}

/// One turn of the wheel as a single number, positive for a turn *up*, in
/// whatever units the device sends (a mouse counts lines, a touchpad pixels).
/// A tilt wheel's own axis counts as the same gesture: it is the sideways
/// scroll a mouse that has one sends, and the controls this drives have one
/// direction each, so the two axes are one answer here.
pub(crate) fn wheel_delta(event: &ScrollWheelEvent) -> f32 {
    let (dx, dy) = match event.delta {
        ScrollDelta::Lines(d) => (d.x, d.y),
        ScrollDelta::Pixels(d) => (f32::from(d.x), f32::from(d.y)),
    };
    match dy == 0. {
        true => dx,
        false => dy,
    }
}

/// The frame a dropped clip's head lands on: `raw`, unless one of `marks` is
/// within `tol` frames of where its head -- or its tail, `len` frames along --
/// would come to rest, in which case that edge wins. The snap every timeline
/// has: clips meet exactly instead of a frame apart, and the hand does not have
/// to aim. The nearest mark wins, and a head landing on one beats a tail at the
/// same distance -- what was dragged is the head.
pub(crate) fn snapped(raw: u32, len: u32, tol: u32, marks: &[u32]) -> u32 {
    let mut best: Option<(u32, u32)> = None;
    for &mark in marks {
        // Head on the mark, then tail on it -- a clip dragged up against the
        // take in front of it snaps by whichever end reaches first.
        for start in [Some(mark), mark.checked_sub(len)] {
            let Some(start) = start else { continue };
            let d = start.abs_diff(raw);
            if d <= tol && best.is_none_or(|(near, _)| d < near) {
                best = Some((d, start));
            }
        }
    }
    best.map_or(raw, |(_, start)| start)
}

/// The edges worth landing on, off *every* lane: both ends of every clip on the
/// timeline, less `skip` -- the clip being dragged, which does not snap to where
/// it already is -- and less `skip_link`, the group id of whatever is being
/// dragged: its clips travel with it, and so does any caption the id names.
/// `skip` is a lane's place in `lanes` and an index into it; `skip_link` is
/// read off a caption by the caller, whose drag has no clip for `skip` to
/// name. The playhead and the head of the timeline go on the end: a clip meets
/// the cursor and the start of the show as readily as it meets another take.
///
/// All lanes rather than the one being dropped on, because a cut is made across
/// the timeline: a title on V2 lines up with the shot under it, and a sound
/// effect lines up with the frame it belongs to.
pub(crate) fn snap_marks(
    lanes: &[&[Clip]],
    skip: Option<(usize, usize)>,
    skip_link: Option<u32>,
    playhead: u32,
) -> Vec<u32> {
    let mut marks: Vec<u32> = lanes
        .iter()
        .enumerate()
        .flat_map(|(lane, clips)| {
            clips
                .iter()
                .enumerate()
                .filter(move |&(idx, clip)| {
                    Some((lane, idx)) != skip && !(skip_link.is_some() && clip.link == skip_link)
                })
                .flat_map(|(_, clip)| [clip.start, clip.end()])
        })
        .collect();
    marks.push(playhead);
    marks.push(0);
    marks
}

/// [`snapped`], and the mark that pulled it there -- the line the bed draws
/// while the hand is still moving. `None` when nothing was near enough: a line
/// standing over open bed would promise a landing that is not going to happen.
/// The head is read before the tail, exactly as [`snapped`] prefers it.
///
/// `on` is the switch ([`ActionId::ToggleSnap`]): off, the gesture lands raw and
/// draws no line at all, which is the whole point of being able to turn it off.
pub(crate) fn snap_cue(
    on: bool,
    raw: u32,
    len: u32,
    tol: u32,
    marks: &[u32],
) -> (u32, Option<u32>) {
    if !on {
        return (raw, None);
    }
    let start = snapped(raw, len, tol, marks);
    let mark = [start, start.saturating_add(len)]
        .into_iter()
        .find(|mark| marks.contains(mark));
    (start, mark)
}

/// Where a drag lands and the mark that pulled it there: the frame under the
/// pointer, less however far into the box the hand grabbed it (so a clip travels
/// with the pointer rather than jumping its head under it), snapped by
/// [`snap_cue`]. One answer, asked by the shadow drawn in flight
/// ([`Player::preview_ghost`]), by the line ([`Player::preview_drop`]) and by
/// the drop that commits ([`Player::move_clip`]) -- which is what makes the
/// promise and the landing the same frame.
pub(crate) fn landing(
    under: u32,
    grab: u32,
    len: u32,
    on: bool,
    tol: u32,
    marks: &[u32],
) -> (u32, Option<u32>) {
    snap_cue(on, under.saturating_sub(grab), len, tol, marks)
}

/// Why this file may not go on that lane, in the words the refusal is told in --
/// `None` when it may. A file with no picture belongs on an audio lane and
/// nowhere else, and a still is silent, so an audio lane is the one place it
/// cannot go. Asked twice: by the ghost tinting itself as refused on the way
/// down, and by the insert that commits ([`Player::insert_source`]), so what is
/// shown as impossible is exactly what is refused.
///
/// `has_video` is [`Player::has_video`]'s answer for `path`: the probed truth
/// where the header has been read, and an extension's guess otherwise -- never
/// `engine::is_audio` alone, which a song muxed into an `.mp4` lies to.
pub(crate) fn lane_refuses(path: &Path, lane: Lane, has_video: bool) -> Option<String> {
    let name = file_name(path);
    let label = lane.label();
    match lane.kind {
        LaneKind::Video if !has_video => Some(format!(
            "NOT ON {label} — {name} has no picture; drop it on an audio lane"
        )),
        LaneKind::Audio if engine::is_image(path) => Some(format!(
            "NOT ON {label} — {name} is a still image; drop it on a video lane"
        )),
        // A subtitle lane holds words and no media at all: the refusal is here
        // rather than at the engine's door so the shadow is tinted red on the
        // way down, like every other lane a file cannot go on.
        LaneKind::Subtitle => Some(format!(
            "NOT ON {label} — {name} is a file; a subtitle track takes captions, dragged from the \
             Subtitles list"
        )),
        _ => None,
    }
}

/// Where down an element a pointer sits, 0..1 from the top: the vertical twin
/// of [`frac_along`], for the equalizer, whose gain axis is the y one. An
/// element that was never painted reads as its middle -- flat, the one answer
/// that changes nothing -- rather than as a full boost.
pub(crate) fn frac_down(y: Pixels, bounds: Bounds<Pixels>) -> f32 {
    if bounds.size.height <= px(0.) {
        return 0.5;
    }
    ((y - bounds.top()) / bounds.size.height).clamp(0., 1.)
}

#[cfg(test)]
mod drop_landing_tests {
    use super::*;

    /// A release a few pixels' worth of frames right of an empty timeline's
    /// head still lands on frame 0: [`snap_marks`] always offers 0
    /// (`marks.push(0)`, above) even with no clips on the timeline yet, so
    /// the "you must be exactly at 0.0" complaint is the tolerance, not a
    /// missing mark.
    #[test]
    fn a_pull_near_the_head_of_an_empty_timeline_snaps_to_frame_0() {
        let marks = snap_marks(&[&[]], None, None, 0);
        assert!(marks.contains(&0), "an empty timeline still offers frame 0");
        assert_eq!(snapped(4, 0, 10, &marks), 0);
        // Outside the tolerance, the raw ask stands: this is a magnet, not a
        // clamp to 0.
        assert_eq!(snapped(11, 0, 10, &marks), 11);
    }

    /// A clip already on the lane offers both of its own edges as marks
    /// ([`snap_marks`]'s `flat_map(|clip| [clip.start, clip.end()])`), and a
    /// release close to either -- within the drop tolerance -- lands on it
    /// rather than the raw pixel.
    #[test]
    fn a_pull_near_a_clip_edge_snaps_onto_it() {
        let marks = [0, 100, 200];
        assert_eq!(snapped(205, 0, 10, &marks), 200);
    }

    /// The playhead is a mark too, so a clip let go near it lines up with
    /// where the timeline is parked.
    #[test]
    fn a_pull_near_the_playhead_snaps_onto_it() {
        let marks = snap_marks(&[&[]], None, None, 500);
        assert!(marks.contains(&500));
        assert_eq!(snapped(493, 0, 10, &marks), 500);
    }
}
