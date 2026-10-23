//! The edit-key verbs both the GUI and the MCP server run: copy and paste a clip's look, nudge, the split edit,
//! match frame and the replace edit's plan. Each acts on a [`PlaybackSession`], so the two front ends share one rule.

use std::path::PathBuf;

use crate::color::ColorParams;
use crate::eq::EqParams;
use crate::project::{Clip, Edge, Lane, LaneKind, Speed};
use crate::scale::FitPolicy;
use crate::transform::TransformParams;
use crate::PlaybackSession;

/// Which part of a copied clip's look a paste carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Color,
    Effects,
    Transform,
    Speed,
    Audio,
}

impl Part {
    fn word(self) -> &'static str {
        match self {
            Part::Color => "colour",
            Part::Effects => "effects",
            Part::Transform => "transform",
            Part::Speed => "speed",
            Part::Audio => "audio",
        }
    }
}

/// What a copy remembers of the clip, read at the moment of the copy so a later
/// edit of the source does not change what the paste brings. The picture half
/// and the sound half are both kept: copying either clip of a linked take
/// copies the take's look.
#[derive(Clone, Debug)]
pub struct CopiedLook {
    picture: Option<PictureLook>,
    audio: Option<AudioLook>,
    speed: SpeedLook,
}

#[derive(Clone, Debug)]
pub struct PictureLook {
    color: Option<ColorParams>,
    transform: Option<TransformParams>,
    fit: FitPolicy,
    effects: Vec<crate::effects::Effect>,
}

#[derive(Clone, Debug)]
pub struct AudioLook {
    gain_db: f32,
    eq: Option<EqParams>,
}

#[derive(Clone, Debug)]
pub struct SpeedLook {
    speed: Speed,
    keys: Vec<crate::project::SpeedKey>,
    reversed: bool,
}

/// The clip on a lane of `kind` that shares `(lane, idx)`'s group, if any.
pub fn linked_of_kind(
    session: &PlaybackSession,
    (lane, idx): (Lane, usize),
    kind: LaneKind,
) -> Option<(Lane, usize)> {
    let clip = session.lane_clips(lane).get(idx)?;
    if lane.kind == kind {
        return Some((lane, idx));
    }
    let id = clip.link?;
    session.lanes().into_iter().filter(|l| l.kind == kind).find_map(|l| {
        let i = session.lane_clips(l).iter().position(|c| c.link == Some(id))?;
        Some((l, i))
    })
}

/// What a paste did: how many things took it, which parts, and what was refused.
pub struct PasteReport {
    pub touched: usize,
    pub words: Vec<&'static str>,
    pub why: Vec<String>,
}

/// Pastes `parts` of `look` onto `picks`, as one undo step. A picture part
/// reaches the picture half of each pick's take, the audio part the sound
/// half; speed reaches the picks themselves.
pub fn paste_look(
    session: &mut PlaybackSession,
    look: &CopiedLook,
    picks: &[(Lane, usize)],
    parts: &[Part],
) -> PasteReport {
    let targets = |session: &PlaybackSession, kind: LaneKind| {
        let mut out: Vec<(Lane, usize)> = Vec::new();
        for &p in picks {
            if let Some(t) = linked_of_kind(session, p, kind)
                && !out.contains(&t)
            {
                out.push(t);
            }
        }
        out
    };
    let mut words: Vec<&'static str> = Vec::new();
    let mut why: Vec<String> = Vec::new();
    let mut touched = 0usize;
    session.begin_batch();
    for &part in parts {
        let applied = match part {
            Part::Color | Part::Effects | Part::Transform => match &look.picture {
                None => {
                    why.push(format!("{}: the copied clip has no picture", part.word()));
                    0
                }
                Some(pic) => {
                    let mut n = 0;
                    for (l, i) in targets(session, LaneKind::Video) {
                        let ok = match part {
                            Part::Color => session.set_color(l, i, pic.color),
                            Part::Effects => session.set_effects(l, i, pic.effects.clone()),
                            _ => {
                                let a = session.set_transform(l, i, pic.transform);
                                let b = session.set_fit(l, i, pic.fit);
                                a || b
                            }
                        };
                        n += usize::from(ok);
                    }
                    n
                }
            },
            Part::Audio => match &look.audio {
                None => {
                    why.push("audio: the copied clip has no sound".into());
                    0
                }
                Some(snd) => {
                    let mut n = 0;
                    for (l, i) in targets(session, LaneKind::Audio) {
                        let a = session.set_clip_gain(l, i, snd.gain_db);
                        let b = session.set_eq(l, i, snd.eq.clone());
                        n += usize::from(a || b);
                    }
                    n
                }
            },
            Part::Speed => {
                let mut n = 0;
                let mut refused = 0;
                for &(l, i) in picks {
                    let sp = &look.speed;
                    let mut ok = session.reversed_of(l, i) == sp.reversed
                        || session.set_reverse(l, i, sp.reversed).is_ok();
                    ok &= session.speed_of(l, i) == sp.speed || session.set_speed(l, i, sp.speed).is_ok();
                    if !sp.keys.is_empty() {
                        ok &= session.set_speed_ramp(l, i, &sp.keys).is_ok();
                    }
                    match ok {
                        true => n += 1,
                        false => refused += 1,
                    }
                }
                if refused > 0 {
                    why.push(format!("speed: no room on {refused} clip(s)"));
                }
                n
            }
        };
        touched += applied;
        if applied > 0 {
            words.push(part.word());
        }
    }
    session.end_batch();
    PasteReport { touched, words, why }
}

/// Moves `picks` (and their groups) `frames` along, all or nothing. `Err` says why not.
pub fn nudge(session: &mut PlaybackSession, picks: &[(Lane, usize)], frames: i64) -> Result<(), &'static str> {
    let &(lane, idx) = picks.last().ok_or("select the clips to move")?;
    let start = session.lane_clips(lane).get(idx).ok_or("that clip is gone")?.start;
    let to = (i64::from(start) + frames).max(0) as u32;
    if to == start {
        return Err("already at the start of the timeline");
    }
    match session.move_selection_to(picks, lane, idx, lane, to) {
        true => Ok(()),
        false => Err("a neighbouring clip is in the way"),
    }
}

/// The split edit: moves the `audio` (else picture) edge of `anchor`'s take to
/// `frame`, the edge nearer the frame being the one that moves. Unlinks first,
/// because a group trims together. Returns the clip and the edge it moved to; on
/// `Err` nothing has changed.
pub fn split_edit_in(
    session: &mut PlaybackSession,
    anchor: (Lane, usize),
    audio: bool,
    frame: u32,
) -> Result<(Lane, usize, Edge, u32), String> {
    split_edit_at(session, anchor, audio, None, frame)
}

/// [`split_edit_in`] with the edge chosen: `Some(edge)` moves that edge (`Start` the in
/// edge, `End` the out edge) and refuses a frame on the wrong side of the other one.
/// `None` picks the edge nearer `frame`.
pub fn split_edit_at(
    session: &mut PlaybackSession,
    anchor: (Lane, usize),
    audio: bool,
    edge: Option<Edge>,
    frame: u32,
) -> Result<(Lane, usize, Edge, u32), String> {
    let kind = if audio { LaneKind::Audio } else { LaneKind::Video };
    let (lane, idx) = linked_of_kind(session, anchor, kind).ok_or_else(|| {
        format!("select the {} clip or group it with its partner", if audio { "sound" } else { "picture" })
    })?;
    if session.lane_locked(lane) {
        return Err(format!("{} is locked", lane.label()));
    }
    let c = session.lane_clips(lane).get(idx).ok_or("that clip is gone")?.clone();
    let edge = match edge {
        Some(Edge::Start) if frame >= c.end() => return Err("the in edge would pass the out edge".into()),
        Some(Edge::End) if frame <= c.start => return Err("the out edge would pass the in edge".into()),
        Some(edge) => edge,
        None if frame >= c.end() => Edge::End,
        None if frame <= c.start || frame - c.start < c.end() - frame => Edge::Start,
        None => Edge::End,
    };
    let before = if edge == Edge::Start { c.start } else { c.end() };
    if before == frame {
        return Err("that edge is already at the playhead".into());
    }
    let pushed = session.history_pushes();
    session.begin_batch();
    session.ungroup(lane, idx);
    let ok = session.trim_clip(lane, idx, edge, frame);
    session.end_batch();
    if !ok {
        // Only a batch that changed something left a step to take back.
        if session.history_pushes() > pushed {
            session.undo();
        }
        return Err("no room to move that edge there".into());
    }
    let now = session.lane_clips(lane).get(idx).map_or(frame, |c| if edge == Edge::Start { c.start } else { c.end() });
    Ok((lane, idx, edge, now))
}

/// Where in which file the clip at `(lane, idx)` shows the timeline frame
/// `frame`: the library path, the audio stream and the seconds into the file.
pub fn matched_source(
    session: &PlaybackSession,
    (lane, idx): (Lane, usize),
    frame: u32,
    fps: f64,
) -> Option<(PathBuf, usize, f64)> {
    let clip = session.lane_clips(lane).get(idx)?;
    if crate::project::is_sequence_source(clip.source) {
        return None;
    }
    let src = session.sources().get(clip.source)?;
    let into = frame.saturating_sub(clip.start).min(clip.frames().saturating_sub(1));
    let from_in = clip.speed.source_at(into);
    let at = match clip.reversed {
        true => clip.out_frame.saturating_sub(1).saturating_sub(from_in),
        false => clip.in_frame + from_in,
    };
    let secs = if crate::is_image(&src.path) { 0. } else { f64::from(at) / fps };
    Some((src.path.clone(), src.audio_stream, secs))
}

/// Reads the look of the clip at `anchor`, with the other half of its take.
pub fn look_of(session: &PlaybackSession, anchor: (Lane, usize)) -> Option<CopiedLook> {
    let (lane, idx) = anchor;
    let clip = session.lane_clips(lane).get(idx)?;
    let picture = linked_of_kind(session, anchor, LaneKind::Video).map(|(l, i)| PictureLook {
        color: session.color_of(l, i).copied(),
        transform: session.transform_of(l, i).copied(),
        fit: session.fit_of(l, i),
        effects: session.effects_of(l, i).to_vec(),
    });
    let audio = linked_of_kind(session, anchor, LaneKind::Audio).map(|(l, i)| AudioLook {
        gain_db: session.clip_gain_of(l, i).unwrap_or(0.),
        eq: session.eq_of(l, i).cloned(),
    });
    Some(CopiedLook {
        picture,
        audio,
        speed: SpeedLook { speed: clip.speed, keys: clip.speed_keys.clone(), reversed: clip.reversed },
    })
}

/// The replace edit's plan: the `(lane, clip)` halves that overwrite the clip at
/// `(lane, idx)` exactly -- same start, same length -- with `source` from `in_f`,
/// the linked other half of the take included when it covers the same frames and
/// the source has that half. `total` is the source's length in frames.
/// Where a replace lands: its start, its length and the clips to place.
pub type ReplacePlan = (u32, u32, Vec<(Lane, Clip)>);

pub fn replace_plan(
    session: &PlaybackSession,
    (lane, idx): (Lane, usize),
    source: usize,
    in_f: u32,
    total: u32,
    (picture, sound, still): (bool, bool, bool),
) -> Result<ReplacePlan, String> {
    let old = session.lane_clips(lane).get(idx).ok_or("that clip is gone")?.clone();
    let len = old.end() - old.start;
    if !still && in_f + len > total {
        return Err(format!("the item has {} frames after its start; the clip is {len}", total.saturating_sub(in_f)));
    }
    match lane.kind {
        LaneKind::Video if !picture => return Err("that item has no picture".into()),
        LaneKind::Audio if !sound => return Err("that item has no sound".into()),
        _ => {}
    }
    let replacement = Clip::source_range(source, in_f, in_f + len);
    let mut items = vec![(lane, replacement.clone())];
    let other = if lane.kind == LaneKind::Video { LaneKind::Audio } else { LaneKind::Video };
    if let Some((pl, pi)) = linked_of_kind(session, (lane, idx), other)
        && let Some(p) = session.lane_clips(pl).get(pi)
        && (p.start, p.end()) == (old.start, old.end())
        && ((pl.kind == LaneKind::Audio && sound) || (pl.kind == LaneKind::Video && picture))
    {
        items.push((pl, replacement));
    }
    Ok((old.start, len, items))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> PlaybackSession {
        crate::ao::set_silent_output();
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        PlaybackSession::open(p).expect("open the fixture")
    }

    #[test]
    fn replace_plan_keeps_place_and_length_and_takes_the_linked_half() {
        let s = open();
        let len = s.lane_clips(Lane::V1)[0].frames();
        let total = s.file_frames(&s.sources()[0].path);
        let (at, n, items) = replace_plan(&s, (Lane::V1, 0), 0, 0, total, (true, true, false)).expect("plan");
        assert_eq!((at, n), (0, len));
        let lanes: Vec<Lane> = items.iter().map(|(l, _)| *l).collect();
        assert_eq!(lanes, vec![Lane::V1, Lane::A1]);
        assert!(items.iter().all(|(_, c)| (c.in_frame, c.out_frame) == (0, len)));
        // A picture-only item cannot replace a sound clip, and a short item is refused with its length.
        assert!(replace_plan(&s, (Lane::A1, 0), 0, 0, total, (true, false, false)).is_err());
        assert!(replace_plan(&s, (Lane::V1, 0), 0, 5, len, (true, true, false)).unwrap_err().contains("frames after its start"));
    }

    #[test]
    fn matched_source_reads_the_source_time_and_nudge_moves_the_take() {
        let mut s = open();
        let fps = 30.0;
        let (path, _, secs) = matched_source(&s, (Lane::V1, 0), 30, fps).expect("a file");
        assert!(path.ends_with("test_av.mp4"));
        assert!((secs - 1.0).abs() < 1e-6, "{secs}");
        assert!(nudge(&mut s, &[], 5).is_err());
        nudge(&mut s, &[(Lane::V1, 0)], 10).expect("room after");
        assert_eq!((s.lane_clips(Lane::V1)[0].start, s.lane_clips(Lane::A1)[0].start), (10, 10));
        assert!(nudge(&mut s, &[(Lane::V1, 0)], 0).is_err(), "no distance is nothing to do");
    }
}
