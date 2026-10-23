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
    let clip = session.lane_clips(lane).get(idx)?.clone();
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

/// A hole the bed may offer as a gap: one with a placement on both sides of it. The stretch
/// before a lane's first placement starts at frame 0 and is not a gap a hand made by moving
/// clips apart, so it is never drawn, picked or closed -- an empty track reads as empty.
/// `gap_at` still answers it for the engine; the bed asks this before it offers one.
pub(crate) fn between_clips(hole: Option<(u32, u32)>) -> Option<(u32, u32)> {
    hole.filter(|&(start, _)| start > 0)
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
            let Some(keep) = clip.fit_source(clip.end().saturating_sub(to)) else {
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
        Edge::End => match clip.fit_source(to.saturating_sub(clip.start)) {
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

/// Where a pro trim's anchor (a cut's frame, a clip's start) lands when moved
/// `by` frames, held inside the room `(lo, hi)` the engine answered
/// ([`engine::PlaybackSession::roll_room`], [`engine::PlaybackSession::slide_room`]).
pub(crate) fn pro_target(base: u32, by: i64, lo: u32, hi: u32) -> u32 {
    (i64::from(base) + by).clamp(i64::from(lo), i64::from(hi)) as u32
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

/// How far a Play Around ([`ActionId::PlayAround`]) runs before the playhead and
/// after it, in seconds.
pub(crate) const PLAY_AROUND_PAD: f64 = 2.0;

/// A Play Around in flight: where the playhead was when it began, and the second
/// the run stops on. Kept on the [`Player`] until the run ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlayAround {
    pub(crate) return_to: f64,
    pub(crate) stop_at: f64,
}

/// The run a Play Around from `playhead` covers on a timeline `duration` seconds
/// long: [`PLAY_AROUND_PAD`] before the playhead to the same after it, clamped to
/// the timeline. `(start, stop)` in seconds.
pub(crate) fn play_around_span(playhead: f64, duration: f64) -> (f64, f64) {
    let start = (playhead - PLAY_AROUND_PAD).max(0.);
    let stop = (playhead + PLAY_AROUND_PAD).min(duration.max(0.));
    (start, stop)
}

/// How close to its stop a run must read before it counts as having reached it.
/// The clock can stop a hair short of the timeline's end when play runs out.
pub(crate) const PLAY_AROUND_SLACK: f64 = 0.05;

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

/// The spacing a ruler tick is drawn at, and so how its label reads
/// (DESIGN §6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RulerStep {
    /// Whole frames: a tick on a whole second reads as that second (`1s`),
    /// the ones between read as frames past it (`+10f`).
    Frames(u32),
    /// Whole seconds, read as `2s`, `30s`, then `1:00`, `1:30` past a minute.
    Seconds(u32),
}

/// Frame steps, in frames, for zoomed-in views where a whole second is still
/// too wide to clear the pitch. Only the ones that divide the frame rate are
/// used (`1, 5` at 25fps; `1, 2, 4, 6, 12` at 24fps; `1, 2, 5, 6, 10, 15` at
/// 30fps), so every whole second is itself a tick and the `+Nf` labels count
/// up to it and start over -- ten-frame ticks at 25fps would read `+20f`,
/// `+5f`, `+15f` with the second never landing on one.
const RULER_FRAME_STEPS: [u32; 10] = [1, 2, 4, 5, 6, 10, 12, 15, 20, 30];
/// Second steps: the human ones -- 1 s, 2 s, 5 s, 10 s, 30 s, a minute, then
/// 2, 5, 10 and 30 minutes and an hour. Never 15 s or 20 s, which a reader
/// cannot count along.
const RULER_SECOND_STEPS: [u32; 11] = [1, 2, 5, 10, 30, 60, 120, 300, 600, 1800, 3600];

impl RulerStep {
    /// The spacing in seconds at the project's frame rate.
    pub(crate) fn secs(self, fps: f64) -> f64 {
        match self {
            RulerStep::Frames(n) => f64::from(n) / fps.max(1.),
            RulerStep::Seconds(s) => f64::from(s),
        }
    }
}

/// The finest step whose pixel width at `pps` clears `min_px`, walking frame
/// steps first, then the second steps up to an hour. Frame steps only appear
/// once a whole second is wider than the pitch allows, so the far zoom never
/// shows frame labels and the near zoom never shows minutes.
pub(crate) fn ruler_step(pps: f64, fps: f64, min_px: f64) -> RulerStep {
    let per_sec = (fps.ceil() as u32).max(1);
    RULER_FRAME_STEPS
        .iter()
        .filter(|&&n| per_sec.is_multiple_of(n))
        .map(|&n| RulerStep::Frames(n))
        .chain(RULER_SECOND_STEPS.iter().map(|&s| RulerStep::Seconds(s)))
        .find(|step| step.secs(fps) * pps >= min_px)
        .unwrap_or(RulerStep::Seconds(3600))
}

/// A whole-second ruler label that cannot be misread as a timecode: `0s`,
/// `2s`, `30s` under a minute, `1:00`, `1:30` past it, `1:00:00` past an
/// hour. The transport's own `HH:MM:SS:FF` readout stays where it is; this
/// is the ruler's shorter voice.
pub(crate) fn ruler_seconds_label(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}:{:02}", s / 60, s % 60),
        s => format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60),
    }
}

/// The label of the `index`th tick at `step`: a second step reads the whole
/// second, a frame step reads the whole second when it lands on one and
/// `+Nf` frames past the last whole second otherwise. Indexed rather than
/// timed so a frame tick never drifts onto the wrong frame over a long bed.
pub(crate) fn ruler_label(step: RulerStep, index: u64, fps: f64) -> String {
    match step {
        RulerStep::Seconds(s) => ruler_seconds_label(index * u64::from(s)),
        RulerStep::Frames(n) => {
            let per_sec = (fps.ceil() as u64).max(1);
            let frames = index * u64::from(n);
            match frames % per_sec {
                0 => ruler_seconds_label(frames / per_sec),
                f => format!("+{f}f"),
            }
        }
    }
}

/// Only the app's tests call this; the GUI path no longer does.
#[cfg(test)]
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
pub(crate) fn live_idx<T: PartialEq>(clips: &[T], idx: usize, clip: &T) -> Option<usize> {
    match clips.get(idx) {
        Some(at) if at == clip => Some(idx),
        _ => clips.iter().position(|c| c == clip),
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
/// `markers` are the comment markers' edges and the chapter starts, which the
/// caller reads off the project ([`Player::snap_targets`]).
///
/// All lanes rather than the one being dropped on, because a cut is made across
/// the timeline: a title on V2 lines up with the shot under it, and a sound
/// effect lines up with the frame it belongs to.
pub(crate) fn snap_marks(
    lanes: &[&[Clip]],
    skip: Option<(usize, usize)>,
    skip_link: Option<u32>,
    playhead: u32,
    range: Option<(u32, u32)>,
    markers: &[u32],
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
    // The in and out points ([`Player::range`]): a hand that has marked a
    // stretch is working to it, and a clip meets its brackets as readily as it
    // meets a cut.
    if let Some((in_, out)) = range {
        marks.push(in_);
        marks.push(out);
    }
    // The comment markers' starts and ends and the chapter starts: a title lines
    // up with the note it answers, and a chapter with a cut.
    marks.extend_from_slice(markers);
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

/// Whether the magnet is on for the gesture in flight: the switch, unless
/// `alt` is held -- the temporary override Resolve, Premiere and Final Cut all
/// put on that key, off for this one drag and leaving the switch where the hand
/// left it. [`Player::snap_live`]'s whole rule, here so it can be read without
/// a window.
pub(crate) fn snap_live(on: bool, alt: bool) -> bool {
    on && !alt
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

/// Where a clip `len` frames long, let go over *another* lane with its head
/// asked at `at`, really comes to rest -- `None` when the engine refuses the
/// drop outright. A mirror of [`Project::move_room`]: the release is refused
/// only when the asked head sits *inside* a take already there (a clamp has no
/// answer for that), or when the gap it names is narrower than the clip;
/// otherwise the head is clamped into that gap exactly as a same-lane move is
/// ([`gap_clamp`]). `others` is `(start, end)` of every clip already on the
/// destination lane, less the ones travelling with this one.
///
/// The shadow reads this, so a landing the release *will* make is never drawn
/// as refused -- a whole-span overlap test refuses far more than the engine
/// does, and a shadow that lies about a refusal reads as a broken timeline.
pub(crate) fn cross_room(at: u32, len: u32, others: &[(u32, u32)]) -> Option<u32> {
    let (mut lo, mut hi) = (0, u32::MAX);
    for &(other_start, other_end) in others {
        if other_start <= at && at < other_end {
            return None;
        }
        match other_end <= at {
            true => lo = lo.max(other_end),
            false => hi = hi.min(other_start),
        }
    }
    match hi - lo < len {
        true => None,
        false => Some(at.clamp(lo, hi - len)),
    }
}

/// Where a clip staying on its own lane really comes to rest: `want`, clamped
/// into the gap it is sitting in -- the take behind it and the take in front of
/// it are walls ([`Project::move_selection`] clamps the delta by exactly these,
/// it does not refuse), so a clip dragged hard left past its neighbour lands
/// flush against that neighbour's tail rather than nowhere. `now` is its head
/// today, which is what names the gap. The shadow reads this, so the box drawn
/// in flight is the box the release leaves behind.
pub(crate) fn gap_clamp(now: u32, len: u32, want: u32, others: &[(u32, u32)]) -> u32 {
    let lo = others
        .iter()
        .filter(|&&(_, end)| end <= now)
        .map(|&(_, end)| end)
        .max()
        .unwrap_or(0);
    let hi = others
        .iter()
        .filter(|&&(start, _)| start >= now.saturating_add(len))
        .map(|&(start, _)| start)
        .min()
        .unwrap_or(u32::MAX);
    want.clamp(lo, hi.saturating_sub(len).max(lo))
}

/// The lane a release lands on when the pointer is off the rows themselves:
/// the row under it while its kind can hold what is in the hand, and otherwise
/// the last one that could -- a picture clip carried down across A1 on its way
/// to the empty bench below is still aiming at V1. [`Player::drag_lane`]'s
/// whole rule.
pub(crate) fn held_lane(prev: Option<Lane>, over: Lane, from: Lane) -> Option<Lane> {
    match over.kind == from.kind {
        true => Some(over),
        false => prev,
    }
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
        LaneKind::Video if !has_video => Some(format!("Not on {label} — {name} has no picture")),
        LaneKind::Audio if engine::is_image(path) => {
            Some(format!("Not on {label} — {name} is a still image"))
        }
        // A subtitle lane holds words and no media at all: the refusal is here
        // rather than at the engine's door so the shadow is tinted red on the
        // way down, like every other lane a file cannot go on.
        LaneKind::Subtitle => Some(format!(
            "Not on {label} — {name} is a file; a subtitle track takes captions, dragged from the \
             Subtitles list"
        )),
        _ => None,
    }
}

/// What a three-point edit needs to know, gathered from the window so the plan
/// itself ([`plan_source_edit`]) is arithmetic and can be tested without one.
pub(crate) struct SourceEditAsk<'a> {
    pub(crate) fps: f64,
    /// Every media lane, with whether it is locked.
    pub(crate) lanes: &'a [(Lane, bool)],
    pub(crate) playhead: u32,
    /// The timeline's in/out marks, in frames.
    pub(crate) range: Option<(u32, u32)>,
    /// The item's own marks, in seconds of the file.
    pub(crate) marks: (Option<f64>, Option<f64>),
    /// The source's index in the project, and its length in timeline frames.
    pub(crate) source: usize,
    pub(crate) total: u32,
    pub(crate) picture: bool,
    pub(crate) sound: bool,
    pub(crate) still: bool,
    /// The source patch: the video lane the picture goes to and the audio lane the sound goes
    /// to. `None` falls to the first unlocked lane of the kind, as it always did.
    pub(crate) video_patch: Option<Lane>,
    pub(crate) audio_patch: Option<Lane>,
}

/// The three-point edit the keys ask for: where it lands (`at`, a frame) and
/// the `(lane, clip)` halves of the take.
///
/// Where: the timeline's in mark when one is set, else the playhead. How long:
/// the source's marked in..out when it has an out mark; else the timeline's
/// in..out length when that is set (the classic three-point: source in plus a
/// timeline range); else to the end of the file. A still takes no marks and
/// shows for the timeline range, else five seconds. Lanes: the first unlocked
/// video lane for the picture and the first unlocked audio lane for the sound,
/// whichever halves the source has.
pub(crate) fn plan_source_edit(ask: &SourceEditAsk) -> Result<(u32, Vec<(Lane, Clip)>), String> {
    let free = |kind: LaneKind, patch: Option<Lane>| match patch.filter(|p| ask.lanes.iter().any(|(l, _)| l == p)) {
        Some(lane) => Some(lane),
        None => ask.lanes.iter().find(|(l, locked)| l.kind == kind && !locked).map(|(l, _)| *l),
    };
    let frame = |t: f64| (t.max(0.) * ask.fps).round() as u32;
    let (at, tl_len) = match ask.range {
        Some((a, b)) if b > a => (a, Some(b - a)),
        _ => (ask.playhead, None),
    };
    let (in_f, out_f) = if ask.still {
        (0, tl_len.unwrap_or_else(|| (5. * ask.fps).ceil() as u32))
    } else {
        let in_f = ask.marks.0.map_or(0, frame);
        let out_f = match (ask.marks.1, tl_len) {
            (Some(t), _) => frame(t),
            (None, Some(len)) => in_f.saturating_add(len),
            (None, None) => ask.total,
        };
        (in_f, out_f.min(ask.total))
    };
    if out_f <= in_f {
        return Err("The marked range is empty".into());
    }
    let mut items = Vec::new();
    if ask.picture {
        let lane = free(LaneKind::Video, ask.video_patch).ok_or("Every video track is locked")?;
        items.push((lane, Clip::source_range(ask.source, in_f, out_f)));
    }
    if ask.sound {
        let lane = free(LaneKind::Audio, ask.audio_patch).ok_or("Every audio track is locked")?;
        items.push((lane, Clip::source_range(ask.source, in_f, out_f)));
    }
    if items.is_empty() {
        return Err("That source has neither picture nor sound".into());
    }
    Ok((at, items))
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
        let marks = snap_marks(&[&[]], None, None, 0, None, &[]);
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
        let marks = snap_marks(&[&[]], None, None, 500, None, &[]);
        assert!(marks.contains(&500));
        assert_eq!(snapped(493, 0, 10, &marks), 500);
    }

    /// The user's own repro: a clip dragged left through the lane-head column
    /// -- window x *left of the bed* -- lands on frame 0 rather than nowhere.
    /// Two clamps do it, and both are on the path the bench now takes for a
    /// drop over the head ([`Player::frame_under`]): [`px_along`] pins an x
    /// before the bed's left edge to 0 px, [`Scale::time_at`] pins the moment
    /// to 0 s, and the grab offset comes off with a saturating subtraction, so
    /// even a clip held by its tail cannot ask for a negative frame.
    #[test]
    fn a_drag_out_over_the_lane_head_lands_on_frame_0() {
        let bed = Bounds {
            origin: point(px(100.), px(0.)),
            size: size(px(900.), px(60.)),
        };
        let scale = Scale::default();
        // 40 px into the head column, and off the window entirely.
        for x in [px(60.), px(-400.)] {
            assert_eq!(px_along(x, bed), 0.);
            let under = frame_at(scale.time_at(px_along(x, bed)), 25.);
            assert_eq!(under, 0);
            // Grabbed 30 frames into the box: still frame 0, never a refusal.
            assert_eq!(landing(under, 30, 50, true, 10, &[0, 300]), (0, Some(0)));
        }
    }

    /// Both edges of the clip in the hand are candidates and the nearest one
    /// wins: a tail 2 frames short of a neighbour's head beats a head 8 frames
    /// from the timeline's start.
    #[test]
    fn the_nearer_of_the_two_edges_wins() {
        let marks = [0, 300, 500];
        // len 292: the tail wants 300 (2 away), the head 0 (8 away).
        // The tail already sits exactly on 300, so the head is left alone.
        assert_eq!(snapped(8, 292, 10, &marks), 8);
        assert_eq!(snapped(6, 292, 10, &marks), 8, "tail meets 300, head stays");
    }

    /// The threshold is a distance on *screen*: the same 10 px of aim at two
    /// zooms is a different number of frames, which is what makes the magnet
    /// feel identical zoomed in and zoomed out ([`Scale::drop_snap_frames`]).
    /// Outside it the drag follows the pointer exactly.
    #[test]
    fn the_threshold_is_ten_pixels_at_any_zoom() {
        let fps = 25.;
        for pps in [50., 400.] {
            let scale = Scale { pps, start: 0. };
            let tol = scale.drop_snap_frames(fps);
            // 10 px worth of frames, and one frame more than that is not near.
            let near = (DROP_SNAP_PX / pps * fps) as u32;
            assert_eq!(tol, near);
            assert_eq!(snapped(500 + tol, 0, tol, &[500]), 500);
            assert_eq!(
                snapped(500 + tol + 1, 0, tol, &[500]),
                500 + tol + 1,
                "past the window the pointer is obeyed exactly"
            );
        }
    }

    /// `alt` held turns the magnet off for the gesture and nothing else: the
    /// same raw ask, no line, and [`Player::snap`] itself untouched.
    #[test]
    fn alt_turns_the_magnet_off_for_this_drag_only() {
        let on = true;
        assert!(snap_live(on, false));
        assert!(!snap_live(on, true));
        let marks = [0, 300];
        assert_eq!(
            snap_cue(snap_live(on, true), 297, 0, 10, &marks),
            (297, None)
        );
        assert_eq!(
            snap_cue(snap_live(on, false), 297, 0, 10, &marks),
            (300, Some(300))
        );
        // The switch off stays off however the modifier is held.
        assert!(!snap_live(false, false));
    }

    /// Every target an NLE offers is on the list: frame 0, the playhead, both
    /// edges of every clip on *every* lane, and the in/out brackets.
    #[test]
    fn the_targets_are_zero_the_playhead_the_edges_and_the_marks() {
        let clip = |start: u32, frames: u32| Clip {
            speed_keys: Vec::new(),
            reversed: false,
            stabilise: None, deinterlace: Default::default(), mark: Default::default(),
            angle: Default::default(),
            fade_in: 0,
            fade_out: 0,
            transition_out: 0,
            transition_kind: Default::default(),
            visualizer: 0,
            viz_paint: 0,
            gain_db: 0.0,
            gain_keys: Vec::new(),
            pan: 0,
            pitch: 0,
            transform_keys: Vec::new(),
            opacity_keys: Vec::new(),
            color_keys: Vec::new(),
            effects: Vec::new(),
            start,
            in_frame: 0,
            out_frame: frames,
            source: 0,
            link: None,
            eq: None,
            color: None,
            transform: None,
            fit: Default::default(),
            speed: Default::default(),
        };
        let v1 = [clip(100, 50), clip(400, 50)];
        let a1 = [clip(700, 20)];
        let marks = snap_marks(&[&v1, &a1], None, None, 900, Some((250, 600)), &[]);
        for want in [0, 900, 100, 150, 400, 450, 700, 720, 250, 600] {
            assert!(marks.contains(&want), "{want} is not a snap target");
        }
        // And each of them actually pulls a drag onto itself.
        for target in [0, 250, 600, 900] {
            assert_eq!(snapped(target + 4, 0, 10, &marks), target);
        }
    }

    /// A release on the empty bench below the last row, or left of the heads,
    /// lands on the lane the last live sample was over -- and a lane of the
    /// wrong kind crossed on the way down never steals it.
    #[test]
    fn the_last_lane_that_could_hold_it_keeps_the_drag() {
        let v1 = Lane::V1;
        let v2 = Lane::new(LaneKind::Video, 1);
        let a1 = Lane::A1;
        // The head column is part of the row, so the row over it is the answer.
        assert_eq!(held_lane(None, v1, v1), Some(v1));
        assert_eq!(held_lane(Some(v1), v2, v1), Some(v2));
        assert_eq!(
            held_lane(Some(v2), a1, v1),
            Some(v2),
            "audio cannot hold it"
        );
        // Nothing promised yet and nothing that can hold it: no landing.
        assert_eq!(held_lane(None, a1, v1), None);
    }

    /// A clip dragged hard left past the take in front of it comes to rest
    /// flush against that take -- the walls of its own gap, which is what the
    /// engine clamps the drag by -- and one dragged off the head of the
    /// timeline rests on frame 0.
    #[test]
    fn a_drag_past_a_neighbour_rests_flush_against_it() {
        let others = [(0, 375), (900, 1000)];
        assert_eq!(gap_clamp(375, 500, 0, &others), 375, "flush with its tail");
        assert_eq!(gap_clamp(375, 500, 380, &others), 380, "room to spare");
        assert_eq!(
            gap_clamp(375, 500, 800, &others),
            400,
            "flush with the next"
        );
        // An empty lane clamps at the head of the timeline and nowhere else.
        assert_eq!(gap_clamp(500, 100, 0, &[]), 0);
        assert_eq!(gap_clamp(500, 100, 9_000, &[]), 9_000);
    }

    /// The cross-lane shadow's rule, mirroring the engine: refused only when
    /// the asked head is inside a take or the gap is too narrow, and otherwise
    /// clamped into the gap rather than refused. The middle case is the
    /// verifier's repro -- an obstacle at (360, 420), a 90-frame clip asked at
    /// 315, which the engine lands at 270.
    #[test]
    fn a_cross_lane_landing_clamps_into_the_gap_the_engine_leaves() {
        let others = [(360, 420)];
        assert_eq!(
            cross_room(315, 90, &others),
            Some(270),
            "clamped, not refused"
        );
        assert_eq!(cross_room(380, 90, &others), None, "head inside the take");
        assert_eq!(cross_room(100, 90, &others), Some(100), "room to spare");
        let tight = [(0, 100), (150, 300)];
        assert_eq!(
            cross_room(120, 90, &tight),
            None,
            "the gap is 50 frames wide"
        );
        assert_eq!(
            cross_room(120, 50, &tight),
            Some(100),
            "and holds a 50-frame clip"
        );
        assert_eq!(
            cross_room(500, 90, &[]),
            Some(500),
            "an empty lane takes it as asked"
        );
    }
}

#[cfg(test)]
mod ruler_label_tests {
    use super::*;

    /// The pitch a ruler label needs, the ruler's own constant.
    const MIN: f64 = 80.;

    /// The pitch a step gives at `pps`, in pixels.
    fn pitch(step: RulerStep, pps: f64, fps: f64) -> f64 {
        step.secs(fps) * pps
    }

    #[test]
    fn the_steps_are_human_at_every_zoom_and_rate() {
        // 24, 25, 30 and 60 fps at the default 60px/s: a second is 60px and
        // misses, so two seconds (120px) is the step at every rate.
        for fps in [24., 25., 30., 60.] {
            assert_eq!(ruler_step(60., fps, MIN), RulerStep::Seconds(2), "{fps}");
        }
        // 300px/s: at 30fps ten frames is 100px; at 24fps ten does not divide
        // the rate, so twelve (150px); at 25fps five frames is 60px and the
        // next divisor is the second itself (300px); at 60fps ten frames is
        // only 50px, so twenty (a third of a second, 100px).
        assert_eq!(ruler_step(300., 30., MIN), RulerStep::Frames(10));
        assert_eq!(ruler_step(300., 24., MIN), RulerStep::Frames(12));
        assert_eq!(ruler_step(300., 25., MIN), RulerStep::Seconds(1));
        assert_eq!(ruler_step(300., 60., MIN), RulerStep::Frames(20));
        // 600px/s at 25fps: five frames is 120px, the audit's own case.
        assert_eq!(ruler_step(600., 25., MIN), RulerStep::Frames(5));
        // 2000px/s: one frame at 24fps is 83px and clears; at 30fps it is 67px
        // so two frames; at 60fps it is 33px so four frames (133px).
        assert_eq!(ruler_step(2000., 24., MIN), RulerStep::Frames(1));
        assert_eq!(ruler_step(2000., 30., MIN), RulerStep::Frames(2));
        assert_eq!(ruler_step(2000., 60., MIN), RulerStep::Frames(4));
        // A frame step always divides its rate, at every zoom and rate.
        for fps in [24., 25., 30., 60.] {
            for pps in [100., 200., 500., 1000., 3000., 10_000.] {
                if let RulerStep::Frames(n) = ruler_step(pps, fps, MIN) {
                    assert_eq!((fps as u32) % n, 0, "{n} frames at {fps}fps, {pps}px/s");
                }
            }
        }
        // Zoomed out: 4px/s wants 30s (120px), never a 15s or 20s stop.
        assert_eq!(ruler_step(4., 25., MIN), RulerStep::Seconds(30));
        // 1px/s: a minute is 60px, so two minutes.
        assert_eq!(ruler_step(1., 25., MIN), RulerStep::Seconds(120));
        assert_eq!(ruler_step(0.1, 25., MIN), RulerStep::Seconds(1800));
        // A zoom no view allows still answers with the far stop.
        assert_eq!(ruler_step(0.001, 25., MIN), RulerStep::Seconds(3600));
    }

    #[test]
    fn every_zoom_gives_a_pitch_of_at_least_the_minimum() {
        for fps in [24., 25., 29.97, 30., 60.] {
            let mut pps = crate::layout::PPS_MIN;
            while pps <= 10_000. {
                let step = ruler_step(pps, fps, MIN);
                let got = pitch(step, pps, fps);
                assert!(
                    got >= MIN || step == RulerStep::Seconds(3600),
                    "pps {pps} fps {fps}: {step:?} gives {got}px"
                );
                pps *= 1.1;
            }
        }
    }

    #[test]
    fn second_labels_read_as_seconds_then_as_minutes() {
        assert_eq!(ruler_seconds_label(0), "0s");
        assert_eq!(ruler_seconds_label(1), "1s");
        assert_eq!(ruler_seconds_label(30), "30s");
        assert_eq!(ruler_seconds_label(59), "59s");
        assert_eq!(ruler_seconds_label(60), "1:00");
        assert_eq!(ruler_seconds_label(90), "1:30");
        assert_eq!(ruler_seconds_label(600), "10:00");
        assert_eq!(ruler_seconds_label(3600), "1:00:00");
        assert_eq!(ruler_seconds_label(3723), "1:02:03");
        // A second step walks in its own units.
        let labels: Vec<_> = (0..4).map(|i| ruler_label(RulerStep::Seconds(2), i, 25.)).collect();
        assert_eq!(labels, ["0s", "2s", "4s", "6s"]);
        let labels: Vec<_> = (0..4).map(|i| ruler_label(RulerStep::Seconds(30), i, 25.)).collect();
        assert_eq!(labels, ["0s", "30s", "1:00", "1:30"]);
    }

    #[test]
    fn frame_labels_read_as_frames_past_the_second_and_never_as_a_timecode() {
        // The 2s-at-25fps clip the audit saw: five-frame ticks read 0s, +5f,
        // +10f, +15f, +20f, 1s, +5f -- never `00:01:00`.
        let labels: Vec<_> = (0..7).map(|i| ruler_label(RulerStep::Frames(5), i, 25.)).collect();
        assert_eq!(labels, ["0s", "+5f", "+10f", "+15f", "+20f", "1s", "+5f"]);
        // 24fps in twos: the twelfth tick is the whole second.
        assert_eq!(ruler_label(RulerStep::Frames(2), 12, 24.), "1s");
        assert_eq!(ruler_label(RulerStep::Frames(2), 11, 24.), "+22f");
        // 30fps in fives, 60fps in tens.
        assert_eq!(ruler_label(RulerStep::Frames(5), 7, 30.), "+5f");
        assert_eq!(ruler_label(RulerStep::Frames(10), 6, 60.), "1s");
        assert_eq!(ruler_label(RulerStep::Frames(10), 7, 60.), "+10f");
        // Single frames at 24fps, deep into the bed, stay on their frame.
        assert_eq!(ruler_label(RulerStep::Frames(1), 24 * 61 + 3, 24.), "+3f");
        assert_eq!(ruler_label(RulerStep::Frames(1), 24 * 61, 24.), "1:01");
        // A fractional rate counts frames to its ceiling, as the timecode does.
        assert_eq!(ruler_label(RulerStep::Frames(1), 29, 29.97), "+29f");
        assert_eq!(ruler_label(RulerStep::Frames(1), 30, 29.97), "1s");
        for label in (0..200).map(|i| ruler_label(RulerStep::Frames(5), i, 25.)) {
            assert!(!label.starts_with("00:"), "{label} reads like a timecode");
        }
    }
}

#[cfg(test)]
mod between_clips_tests {
    use super::*;

    /// The stretch before a lane's first placement is not a gap a hand can close, so the bed
    /// never draws it on an empty-looking track; a hole with a placement on each side is.
    #[test]
    fn a_hole_before_the_first_placement_is_not_a_gap() {
        assert_eq!(between_clips(Some((0, 30))), None);
        assert_eq!(between_clips(Some((30, 10))), Some((30, 10)));
        assert_eq!(between_clips(None), None);
    }
}
