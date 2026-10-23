//! Timelines for other editors: OpenTimelineIO ([`otio`], read and written),
//! Final Cut Pro XML ([`fcpxml`]) and CMX3600 EDL ([`edl`]), write only.
//!
//! Every format is written from one plain [`Timeline`] that [`of_project`]
//! reads off the active sequence. OpenTimelineIO reads back into the same
//! type, and [`build`] makes it a new sequence of a project.

pub mod edl;
pub mod fcpxml;
pub mod otio;

use std::path::PathBuf;

use crate::project::{Clip, Lane, LaneKind, Marker, Project, Speed};
use crate::transitions::TransitionKind;

/// One sequence, as an editor's timeline: its tracks in order, the markers on
/// it, and what an export had to leave out.
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub name: String,
    /// Timeline frames per second.
    pub fps: f64,
    /// The canvas size the picture is cut at.
    pub resolution: (u32, u32),
    /// Video tracks first, then audio, each in lane order.
    pub tracks: Vec<Track>,
    pub markers: Vec<Marker>,
    /// What the export left out, in words for the user.
    pub notes: Vec<String>,
}

/// One lane of the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub kind: LaneKind,
    /// Position among the tracks of its kind: `V1` is 0.
    pub ord: usize,
    pub items: Vec<Item>,
}

/// What sits on a track, in order, back to back.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Clip(ClipItem),
    /// Empty space of this many timeline frames.
    Gap(u32),
}

/// A clip: where it plays, from which file, over which source range.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipItem {
    pub path: PathBuf,
    /// The timeline frame it starts at.
    pub start: u32,
    /// The source frames it plays, `in_frame..out_frame`.
    pub in_frame: u32,
    pub out_frame: u32,
    pub speed: Speed,
    /// Timeline frames it covers: the source range at its speed.
    pub frames: u32,
    /// A transition from this clip into the next one: its kind and length in frames.
    pub transition: Option<(TransitionKind, u32)>,
}

impl ClipItem {
    /// The file's name, which is what a clip is called in another editor.
    pub fn file_name(&self) -> String {
        self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }
}

impl Timeline {
    /// The timeline frame the last item of any track ends at.
    pub fn frames(&self) -> u32 {
        self.tracks
            .iter()
            .map(|t| {
                t.items.iter().fold(0, |at, item| match item {
                    Item::Clip(c) => c.start + c.frames,
                    Item::Gap(n) => at + n,
                })
            })
            .max()
            .unwrap_or(0)
    }
}

/// The timeline of `project`'s active sequence. `project` is a flattened
/// snapshot ([`crate::PlaybackSession::export_snapshot`]), so a compound clip
/// already stands for what it plays.
pub fn of_project(project: &Project, fps: f64, resolution: (u32, u32)) -> Timeline {
    let mut tracks = Vec::new();
    let mut notes = Vec::new();
    for kind in [LaneKind::Video, LaneKind::Audio] {
        for lane in project.lanes().into_iter().filter(|l| l.kind == kind) {
            tracks.push(track_of(project, lane));
        }
    }
    if project.subtitle_lanes().iter().any(|&l| !project.sub_lane(l).is_empty()) {
        notes.push("Subtitles were left out.".to_string());
    }
    let styled = tracks.iter().any(|t| {
        let lane = Lane::new(t.kind, t.ord);
        project.lane(lane).iter().any(|c| {
            c.fade_in > 0
                || c.fade_out > 0
                || c.eq.is_some()
                || c.color.is_some()
                || c.transform.is_some()
                || c.gain_db != 0.0
                || c.reversed
                || !c.effects.is_empty()
        })
    });
    if styled {
        notes.push("Colour, effects, fades, levels and reverse were left out.".to_string());
    }
    Timeline {
        name: project.sequence_name(project.active_sequence()).unwrap_or("Sequence 1").to_string(),
        fps,
        resolution,
        tracks,
        markers: project.markers().to_vec(),
        notes,
    }
}

fn track_of(project: &Project, lane: Lane) -> Track {
    let clips = project.lane(lane);
    let mut items = Vec::new();
    let mut cursor = 0;
    for (i, c) in clips.iter().enumerate() {
        if c.start > cursor {
            items.push(Item::Gap(c.start - cursor));
        }
        let frames = c.frames();
        let path = project.sources().get(c.source).map_or_else(PathBuf::new, |s| s.path.clone());
        // A transition is a dissolve into the next clip only when the two abut.
        let transition = clips
            .get(i + 1)
            .filter(|next| c.transition_out > 0 && next.start == c.start + frames)
            .map(|_| (c.transition_kind, c.transition_out));
        items.push(Item::Clip(ClipItem {
            path,
            start: c.start,
            in_frame: c.in_frame,
            out_frame: c.out_frame,
            speed: c.speed,
            frames,
            transition,
        }));
        cursor = c.start + frames;
    }
    Track { kind: lane.kind, ord: lane.ord, items }
}

/// Makes `t` a new sequence of `project`, named after it, and switches to it.
/// Each clip is taken in by its path, the way a library file is; a transition
/// that does not sit between two abutting clips is refused.
pub fn build(project: &mut Project, t: &Timeline) -> crate::Result<u32> {
    // A name the project already has gets " (2)", " (3)" and so on, as a new sequence does.
    let name = (1..)
        .map(|n| match n {
            1 => t.name.clone(),
            n => format!("{} ({n})", t.name),
        })
        .find(|name| project.sequence_named(name).is_none())
        .unwrap_or_else(|| t.name.clone());
    let id = project.new_sequence(Some(name.as_str()))?;
    project.switch_sequence(id)?;
    for track in &t.tracks {
        let lane = Lane::new(track.kind, track.ord);
        while project.lane_count(track.kind) <= track.ord {
            project.add_lane(track.kind);
        }
        for item in &track.items {
            let Item::Clip(c) = item else { continue };
            let source = project.import(&c.path, 0);
            let mut clip = Clip::source_range(source, c.in_frame, c.out_frame);
            clip.speed = c.speed;
            if !project.place(lane, c.start, clip) {
                return Err(format!("could not place {} on {}", c.path.display(), lane.label()).into());
            }
        }
        let mut idx = 0;
        for item in &track.items {
            let Item::Clip(c) = item else { continue };
            if let Some((kind, frames)) = c.transition
                && !project.set_transition(lane, idx, kind, frames)
            {
                return Err(format!("a transition on {} is not between two clips", lane.label()).into());
            }
            idx += 1;
        }
    }
    for marker in &t.markers {
        project.add_marker(marker.clone())?;
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::MarkerColor;

    fn clip(path: &str, start: u32, in_frame: u32, out_frame: u32, transition: Option<(TransitionKind, u32)>) -> Item {
        Item::Clip(ClipItem {
            path: PathBuf::from(path),
            start,
            in_frame,
            out_frame,
            speed: Speed::NORMAL,
            frames: out_frame - in_frame,
            transition,
        })
    }

    /// Three clips on V1 with a dissolve after the first, three on A1, and a marker.
    fn cut() -> Timeline {
        let items = |v: &str, dissolve: Option<(TransitionKind, u32)>| {
            vec![
                clip(v, 0, 0, 50, dissolve),
                clip(v, 50, 10, 60, None),
                clip(v, 100, 0, 25, None),
            ]
        };
        Timeline {
            name: "Cut".into(),
            fps: 25.0,
            resolution: (1920, 1080),
            tracks: vec![
                Track { kind: LaneKind::Video, ord: 0, items: items("/media/a.mp4", Some((TransitionKind::Dissolve, 10))) },
                Track { kind: LaneKind::Audio, ord: 0, items: items("/media/a.mp4", None) },
            ],
            markers: vec![Marker {
                frame: 60,
                length: 0,
                name: "Beat".into(),
                note: String::new(),
                color: MarkerColor::Red,
            }],
            notes: Vec::new(),
        }
    }

    /// The structure survives OTIO export and import, and FCPXML and EDL carry the key lines.
    #[test]
    fn otio_round_trip_keeps_tracks_transitions_and_markers() {
        let mut p = Project::single("/media/a.mp4", 100);
        let t = cut();
        build(&mut p, &t).expect("builds");
        let seen = of_project(&p, 25.0, (1920, 1080));
        assert_eq!(seen.name, "Cut");
        assert_eq!(seen.tracks, t.tracks);
        assert_eq!(seen.markers, t.markers);

        let back = otio::read(&otio::write(&seen), 25.0).expect("reads its own export");
        assert_eq!(back.tracks, t.tracks);
        assert_eq!(back.markers, t.markers);
        assert_eq!(back.name, "Cut");

        let x = fcpxml::write(&seen);
        assert!(x.contains("<fcpxml version=\"1.10\">"), "{x}");
        assert!(x.contains("<asset-clip ref=\"r2\" offset=\"0s\""), "{x}");
        assert!(x.contains("<marker start=\"4/5s\""), "{x}");
        let e = edl::write(&seen);
        assert!(e.starts_with("TITLE: Cut\nFCM: NON-DROP FRAME\n"), "{e}");
        assert!(e.contains("001  A        V     C        00:00:00:00 00:00:02:00 01:00:00:00 01:00:02:00\n"), "{e}");
        assert!(e.contains("D    010"), "{e}");
        assert!(e.contains("* FROM CLIP NAME:  a.mp4"), "{e}");
    }
}
