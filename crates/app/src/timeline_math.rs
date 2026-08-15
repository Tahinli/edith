//! The timeline's arithmetic: takes, trims, snapping and envelopes.

use crate::*;

/// Whether this clip is a whole take, i.e. whether deleting it may close the
/// hole under it: a take is what the first pair of lanes carries between them,
/// `V1`'s picture and the sound grouped with it, and dropping one moves the
/// frames after it on every lane.
///
/// Everything else is a half or a layer, and is *lifted* instead: a half whose
/// picture was lifted (what a lift leaves behind) has no take to ripple, and a
/// clip on a further lane is laid over the timeline rather than part of it --
/// closing a hole under it would drag the take beneath out of step with it.
pub(crate) fn whole_take(session: &PlaybackSession, lane: Lane, idx: usize) -> bool {
    let Some(clip) = session.lane_clips(lane).get(idx) else {
        return false;
    };
    let paired = || {
        session
            .lanes()
            .into_iter()
            .filter(|&other| other != lane)
            .flat_map(|other| session.lane_clips(other))
            .any(|o| o.link.is_some() && o.link == clip.link)
    };
    match (lane.kind, lane.ord) {
        (_, 1..) => false,
        // The picture of a take -- unless the take has been taken apart: a
        // detached picture (a group id no other lane carries, which is also what
        // a lift of the sound leaves) is a half like the sound is, and a ripple
        // under it would drag away the very half it was detached from. A clip in
        // no group at all is not a half but a placement, and on `V1` a placement
        // is the take there is.
        (LaneKind::Video, _) => clip.link.is_none() || paired(),
        // The sound of a take, only while the take is still there: its group is
        // carried by a clip on some other lane.
        (LaneKind::Audio, _) => paired(),
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
pub(crate) fn span_partner(session: &PlaybackSession, lane: Lane, idx: usize) -> Option<(Lane, usize)> {
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

/// Whether a click marks this clip: the clip that was clicked always, and the
/// other lane's clip of the same group with it. A clip whose group has no other
/// half -- what a lift leaves behind -- marks only itself, which is what makes a
/// detached half separately deletable.
pub(crate) fn marked(
    here: (Lane, usize),
    link: Option<u32>,
    sel: Option<(Lane, usize)>,
    sel_link: Option<u32>,
) -> bool {
    sel == Some(here) || (link.is_some() && link == sel_link)
}

/// Whether a clip is wide enough to be worth naming.
pub(crate) fn show_label(w: f32) -> bool {
    w >= LABEL_MIN_W
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
                    in_frame: 0,
                    out_frame: keep,
                    start: to,
                    ..clip
                },
                false => Clip {
                    in_frame: clip.out_frame - keep.min(clip.out_frame),
                    start: to,
                    ..clip
                },
            }
        }
        Edge::End => match clip.speed.fit(to.saturating_sub(clip.start)) {
            Some(keep) => Clip {
                out_frame: clip.in_frame + keep,
                ..clip
            },
            None => clip,
        },
    }
}

/// How many timeline frames a stretch of a subtitle track is worth: the one
/// conversion between the two clocks a caption has -- microseconds for its
/// words, frames for where it sits -- and the app-side twin of the engine's own
/// (`Project::trim_sub_room`). Never zero: a placement of no frames is the one
/// [`Project::place_sub`] refuses as empty, and a track shorter than a frame is
/// still a caption somebody dragged.
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

/// The part of a clip's box that is on the bed, in the box's own pixels:
/// `(left, width)` of its intersection with the visible strip. Everything drawn
/// *inside* a box -- its waveform, its name, its speed badge -- is placed in
/// here rather than at the box's own edges, which at a deep zoom sit thousands
/// of pixels off either side of the screen: a label out there is a label nobody
/// can read, and a waveform out there is a path with a point per two pixels of a
/// width nobody can see. A bed that has not been measured yet answers with the
/// whole box, which is what was drawn before there was a bed to clip to.
pub(crate) fn visible_slice(left: f32, width: f32, bed: f32) -> (f32, f32) {
    if bed <= 0. {
        return (0., width);
    }
    let from = (-left).clamp(0., width);
    let to = (bed - left).clamp(from, width);
    (from, to - from)
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
pub(crate) fn envelope(peaks: &[(f32, f32)], from: f64, to: f64, w: f32, h: f32) -> Vec<(f32, f32, f32)> {
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
/// it already is -- and less the other halves of its group, which travel with
/// it. `skip` is a lane's place in `lanes` and an index into it. The playhead
/// and the head of the timeline go on the end: a clip meets the cursor and the
/// start of the show as readily as it meets another take.
///
/// All lanes rather than the one being dropped on, because a cut is made across
/// the timeline: a title on V2 lines up with the shot under it, and a sound
/// effect lines up with the frame it belongs to.
pub(crate) fn snap_marks(lanes: &[&[Clip]], skip: Option<(usize, usize)>, playhead: u32) -> Vec<u32> {
    let link = skip
        .and_then(|(lane, idx)| lanes.get(lane)?.get(idx))
        .and_then(|clip| clip.link);
    let mut marks: Vec<u32> = lanes
        .iter()
        .enumerate()
        .flat_map(|(lane, clips)| {
            clips
                .iter()
                .enumerate()
                .filter(move |&(idx, clip)| {
                    Some((lane, idx)) != skip && !(link.is_some() && clip.link == link)
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
pub(crate) fn snap_cue(on: bool, raw: u32, len: u32, tol: u32, marks: &[u32]) -> (u32, Option<u32>) {
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
pub(crate) fn lane_refuses(path: &Path, lane: Lane) -> Option<String> {
    let name = file_name(path);
    let label = lane.label();
    match lane.kind {
        LaneKind::Video if engine::is_audio(path) => {
            Some(format!("NOT ON {label} — {name} has no picture; drop it on an audio lane"))
        }
        LaneKind::Audio if engine::is_image(path) => {
            Some(format!("NOT ON {label} — {name} is a still image; drop it on a video lane"))
        }
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
