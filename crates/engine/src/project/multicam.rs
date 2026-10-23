//! Multicam clips: several cameras of one event, synced, cut between by angle.
//!
//! A multicam clip is a compound clip ([`super::sequence`]) whose sequence is flagged as a
//! multicam source: angle `n` is that sequence's `n`th video lane and `n`th audio lane, each
//! holding one camera's clip placed at its sync offset. On the timeline the compound clip carries
//! an [`Angle`](super::Angle): the render plays that angle's picture, and its sound too unless the
//! sound is pinned to another angle. Cutting between cameras is a razor cut plus a new angle on
//! the right half ([`Project::switch_angle`]), so a multicam cut is a row of plain compound clips
//! side by side, each trimmed with the ordinary tools. [`Project::flatten_multicam`] turns them
//! into the camera clips they play.

use super::sequence::{MAX_SEQUENCE_NAME_CHARS, SeqData};
use super::*;

/// The most angles a multicam clip holds (the angle monitor is a 3x3 grid, the number keys 1-9).
pub const MAX_ANGLES: usize = 9;

/// One camera of a multicam clip, as [`Project::make_multicam`] takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AngleSpec {
    /// Index into [`Project::sources`].
    pub source: usize,
    /// What the angle is called; empty for "Angle N".
    pub name: String,
    /// The source's length in timeline frames.
    pub frames: u32,
    /// Where the camera's first frame sits on the common timeline of the multicam clip.
    pub start: u32,
    /// Whether the source has a sound track to put on the angle's audio lane.
    pub audio: bool,
}

/// The multicam clip at a frame ([`Project::multicam_under`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MulticamUnder {
    pub lane: Lane,
    pub idx: usize,
    /// The multicam source sequence.
    pub sequence: u32,
    pub names: Vec<String>,
    /// The angle (from 0) the clip shows there.
    pub shown: usize,
}

/// What [`Project::make_multicam`] made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MulticamMade {
    /// The multicam source sequence.
    pub sequence: u32,
    /// The compound clips now on the timeline (picture, then sound when there is any).
    pub clips: Vec<(Lane, usize)>,
}

impl Project {
    /// The multicam sequence a clip plays and its angle names, when it is a multicam clip.
    pub fn multicam_of(&self, clip: &Clip) -> Option<(u32, &[String])> {
        let id = self.compound_of(clip)?;
        self.angle_names(id).map(|n| (id, n))
    }

    /// The multicam clip showing at timeline `frame` on the lowest video lane that has one: its
    /// lane, index, source sequence, angle names, and the angle it shows there.
    pub fn multicam_under(&self, frame: u32) -> Option<MulticamUnder> {
        for lane in self.lanes().into_iter().filter(|l| l.kind == LaneKind::Video) {
            let Some((idx, c)) = self.lane(lane).iter().enumerate().find(|(_, c)| c.start <= frame && frame < c.end()) else {
                continue;
            };
            if let Some((sequence, names)) = self.multicam_of(c) {
                let shown = usize::from(c.angle.video).min(names.len().saturating_sub(1));
                return Some(MulticamUnder { lane, idx, sequence, names: names.to_vec(), shown });
            }
        }
        None
    }

    /// One flattened project per angle of the multicam clip under `frame`, each with that clip
    /// showing the angle: what the angle monitor composes its tiles from. `None` with no multicam
    /// clip there.
    pub fn angle_views(&self, frame: u32) -> Option<(MulticamUnder, Vec<Project>)> {
        let under = self.multicam_under(frame)?;
        let mut views = Vec::new();
        for n in 0..under.names.len() {
            let mut p = self.raw_snapshot();
            let idx = self.index(under.lane)?;
            let clip = p.lanes.get_mut(idx)?.clips.get_mut(under.idx)?;
            clip.angle.video = n as u8;
            views.push(p.export_snapshot());
        }
        Some((under, views))
    }

    /// Makes a multicam source from `angles` and puts one clip of it (picture on `lane`, sound on
    /// the audio lane of the same number, linked) at `at`. One undo step. Refused for fewer than
    /// two or more than [`MAX_ANGLES`] angles, an unknown source, an empty angle, a lane that is
    /// not a video lane, or the sequence limit.
    pub fn make_multicam(&mut self, name: Option<&str>, angles: &[AngleSpec], lane: Lane, at: u32) -> crate::Result<MulticamMade> {
        if angles.len() < 2 || angles.len() > MAX_ANGLES {
            return Err(format!("a multicam clip needs 2 to {MAX_ANGLES} angles, got {}", angles.len()).into());
        }
        if lane.kind != LaneKind::Video || self.index(lane).is_none() {
            return Err("a multicam clip goes on a video lane".into());
        }
        if self.seq.names.len() >= MAX_SEQUENCES {
            return Err(format!("a project holds at most {MAX_SEQUENCES} sequences").into());
        }
        for a in angles {
            if a.source >= self.sources.len() {
                return Err(format!("angle source {} is not in the project", a.source).into());
            }
            if a.frames == 0 {
                return Err("an angle has no frames".into());
            }
        }
        let title = match name {
            Some(n) => self.unique_name(n, None)?,
            None => self.fresh_name("Multicam"),
        };
        let names: Vec<String> = angles
            .iter()
            .enumerate()
            .map(|(i, a)| match a.name.trim() {
                "" => format!("Angle {}", i + 1),
                n => n.chars().take(MAX_SEQUENCE_NAME_CHARS).collect(),
            })
            .collect();
        let len = angles.iter().map(|a| a.start + a.frames).max().unwrap_or(1);
        self.snapshot();
        let mut video = Vec::new();
        let mut audio = Vec::new();
        for a in angles {
            let link = a.audio.then(|| self.new_link());
            let mut v = Clip::source_range(a.source, 0, a.frames);
            v.start = a.start;
            v.link = link;
            video.push(LaneData::new(LaneKind::Video, vec![v]));
            let mut sound = Vec::new();
            if a.audio {
                let mut s = Clip::source_range(a.source, 0, a.frames);
                s.start = a.start;
                s.link = link;
                sound.push(s);
            }
            audio.push(LaneData::new(LaneKind::Audio, sound));
        }
        let id = self.seq_next;
        self.seq_next += 1;
        let pos = self.seq.names.iter().position(|(i, _)| *i == self.seq.active).map_or(self.seq.names.len(), |p| p + 1);
        self.seq.names.insert(pos, (id, title));
        let mut data = SeqData::empty();
        data.lanes = video.into_iter().chain(audio).collect();
        self.seq.parked.push((id, data));
        self.seq.mcam.push((id, names));
        let alane = Lane::new(LaneKind::Audio, lane.ord);
        while self.lane_count(LaneKind::Audio) <= alane.ord {
            self.lanes.push(LaneData::new(LaneKind::Audio, Vec::new()));
        }
        let link = self.new_link();
        let mut made = Vec::new();
        for target in [lane, alane] {
            let mut c = Clip::source_range(sequence_source(id), 0, len);
            c.start = at;
            c.link = Some(link);
            self.insert_over(target, c, &[]);
            if let Some(i) = self.lane(target).iter().position(|x| x.start == at && x.source == sequence_source(id)) {
                made.push((target, i));
            }
        }
        self.flat.clear();
        Ok(MulticamMade { sequence: id, clips: made })
    }

    /// Cuts the multicam clip under `frame` (on `lane`, or the lowest video lane that has one)
    /// there and shows `video` from that frame on, as one undo step; `audio` is `Some(None)` to
    /// make the sound follow the picture, `Some(Some(n))` to keep it on angle `n`. A cut at the
    /// clip's own start is not needed and changes the whole clip. Returns the clips that now
    /// carry the new angle (lane and start). Refused with nothing changed when there is no
    /// multicam clip there, the angle does not exist, or nothing would change.
    pub fn switch_angle(
        &mut self,
        lane: Option<Lane>,
        frame: u32,
        video: Option<u8>,
        audio: Option<Option<u8>>,
    ) -> crate::Result<Vec<(Lane, u32)>> {
        if video.is_none() && audio.is_none() {
            return Err("name an angle for the picture or for the sound".into());
        }
        let lanes: Vec<Lane> = match lane {
            Some(l) => vec![l],
            None => self.lanes().into_iter().filter(|l| l.kind == LaneKind::Video).collect(),
        };
        let mut found = None;
        for l in lanes {
            if let Some(c) = self.lane(l).iter().find(|c| c.start <= frame && frame < c.end())
                && self.multicam_of(c).is_some()
            {
                found = Some((l, c.clone()));
                break;
            }
        }
        let Some((vlane, head)) = found else {
            return Err("there is no multicam clip there: park the playhead on one".into());
        };
        let (id, names) = self.multicam_of(&head).map(|(i, n)| (i, n.len())).unwrap_or((0, 0));
        for a in video.into_iter().chain(audio.flatten()) {
            if usize::from(a) >= names {
                return Err(format!("angle {} does not exist: \"{}\" has {names}", a + 1, self.sequence_name(id).unwrap_or("multicam")).into());
            }
        }
        // The group the cut touches: the picture clip and the sound clips linked to it.
        let src = head.source;
        let mut group: Vec<Lane> = vec![vlane];
        if let Some(link) = head.link {
            for l in self.lanes().into_iter().filter(|l| l.kind == LaneKind::Audio) {
                if self.lane(l).iter().any(|c| c.link == Some(link) && c.source == src && c.start <= frame && frame < c.end()) {
                    group.push(l);
                }
            }
        }
        let apply = |c: &mut Clip| {
            if let Some(v) = video {
                c.angle.video = v;
            }
            if let Some(a) = audio {
                c.angle.audio = a;
            }
        };
        let current: Vec<Angle> = group
            .iter()
            .filter_map(|&l| self.lane(l).iter().find(|c| c.start <= frame && frame < c.end()).map(|c| c.angle))
            .collect();
        let mut probe = current.clone();
        for a in &mut probe {
            let mut c = Clip::source_range(0, 0, 1);
            c.angle = *a;
            apply(&mut c);
            *a = c.angle;
        }
        if probe == current {
            return Err("that angle is already showing there".into());
        }
        self.snapshot();
        let idxs: Vec<usize> = group.iter().filter_map(|&l| self.index(l)).collect();
        self.write_split(frame, false, &idxs);
        let mut touched = Vec::new();
        for &l in &group {
            let Some(clips) = self.lane_mut(l) else { continue };
            if let Some(c) = clips.iter_mut().find(|c| c.start <= frame && frame < c.end() && c.source == src) {
                apply(c);
                touched.push((l, c.start));
            }
        }
        self.flat.clear();
        Ok(touched)
    }

    /// Replaces the multicam clip at `lane`/`idx`, every other clip of the same multicam source in
    /// this sequence, and their linked partners with the camera clips they play (the angle each
    /// shows), one undo step. The multicam source sequence stays. Refused, changing nothing,
    /// when another clip is in the way.
    pub fn flatten_multicam(&mut self, lane: Lane, idx: usize) -> crate::Result<Vec<(Lane, usize)>> {
        let Some(c) = self.lane(lane).get(idx) else {
            return Err(format!("there is no clip {} #{idx}", lane.label()).into());
        };
        if self.multicam_of(c).is_none() {
            return Err(format!("{} #{idx} is not a multicam clip", lane.label()).into());
        }
        let src = c.source;
        let mut picks = Vec::new();
        for l in self.lanes().into_iter().filter(|l| l.kind != LaneKind::Subtitle) {
            for (i, o) in self.lane(l).iter().enumerate() {
                if o.source == src {
                    picks.push((l, i));
                }
            }
        }
        self.break_apart(&picks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(source: usize, start: u32, frames: u32, audio: bool) -> AngleSpec {
        AngleSpec { source, name: String::new(), frames, start, audio }
    }

    fn pair() -> (Project, MulticamMade) {
        let mut p = Project::single("/tmp/edith-mcam-a.mp4", 100);
        p.import("/tmp/edith-mcam-b.mp4", 0);
        let made = p
            .make_multicam(Some("Show"), &[spec(0, 10, 100, true), spec(1, 0, 120, true)], Lane::V1, 0)
            .expect("made");
        (p, made)
    }

    type Strip = Vec<(LaneKind, Vec<(u32, u32, u32, usize)>)>;
    /// What the render reads: every lane's clips as (start, in, out, source).
    fn strip(p: &Project) -> Strip {
        let v = p.render_view();
        v.lanes.iter().map(|l| (l.kind, l.clips.iter().map(|c| (c.start, c.in_frame, c.out_frame, c.source)).collect())).collect()
    }

    fn lane_of(s: &Strip, kind: LaneKind) -> Vec<(u32, u32, u32, usize)> {
        s.iter().find(|(k, _)| *k == kind).map(|(_, c)| c.clone()).unwrap_or_default()
    }

    #[test]
    fn a_multicam_clip_plays_the_first_angle_at_its_sync_offset() {
        let (p, made) = pair();
        assert_eq!(made.clips.len(), 2);
        assert_eq!(p.angle_names(made.sequence).map(<[String]>::to_vec), Some(vec!["Angle 1".to_string(), "Angle 2".to_string()]));
        let s = strip(&p);
        assert_eq!(lane_of(&s, LaneKind::Video), vec![(10, 0, 100, 0)]);
        assert_eq!(lane_of(&s, LaneKind::Audio), vec![(10, 0, 100, 0)]);
        assert_eq!(p.sequences().len(), 2);
    }

    #[test]
    fn switching_the_angle_cuts_the_clip_and_is_one_undo_step() {
        let (mut p, _) = pair();
        let before = strip(&p);
        let touched = p.switch_angle(None, 60, Some(1), None).expect("switch");
        assert_eq!(touched, vec![(Lane::V1, 60), (Lane::A1, 60)]);
        let s = strip(&p);
        assert_eq!(lane_of(&s, LaneKind::Video), vec![(10, 0, 50, 0), (60, 60, 120, 1)]);
        assert_eq!(lane_of(&s, LaneKind::Audio), vec![(10, 0, 50, 0), (60, 60, 120, 1)], "the sound follows the picture");
        assert_eq!(p.lane(Lane::V1).len(), 2);
        assert!(p.undo());
        assert_eq!(strip(&p), before);
        assert_eq!(p.lane(Lane::V1).len(), 1);
        assert!(p.redo());
        assert_eq!(p.lane(Lane::V1).len(), 2);
        assert!(p.switch_angle(None, 60, Some(1), None).is_err(), "already showing there");
        assert!(p.switch_angle(None, 61, Some(2), None).is_err(), "no third angle");
        assert!(p.switch_angle(None, 500, Some(0), None).is_err(), "nothing there");
        // Back to the first angle half way into the second segment.
        p.switch_angle(None, 90, Some(0), None).expect("again");
        assert_eq!(lane_of(&strip(&p), LaneKind::Video), vec![(10, 0, 50, 0), (60, 60, 90, 1), (90, 80, 100, 0)]);
    }

    #[test]
    fn the_sound_can_stay_on_an_angle() {
        let (mut p, _) = pair();
        p.switch_angle(None, 60, Some(1), Some(Some(0))).expect("switch");
        let s = strip(&p);
        assert_eq!(lane_of(&s, LaneKind::Video), vec![(10, 0, 50, 0), (60, 60, 120, 1)]);
        assert_eq!(lane_of(&s, LaneKind::Audio), vec![(10, 0, 50, 0), (60, 50, 100, 0)], "angle 1's sound plays on under angle 2");
        // The next cut keeps the pin; asking the sound to follow again lifts it.
        p.switch_angle(None, 100, Some(0), None).expect("next");
        assert_eq!(p.lane(Lane::A1)[2].angle, Angle { video: 0, audio: Some(0) });
        p.switch_angle(None, 100, None, Some(None)).expect("lift the pin");
        assert_eq!(p.lane(Lane::A1)[2].angle, Angle { video: 0, audio: None });
        assert!(p.switch_angle(None, 100, None, Some(None)).is_err(), "the sound already follows");
    }

    #[test]
    fn flattening_leaves_the_render_as_it_was() {
        let (mut p, _) = pair();
        p.switch_angle(None, 60, Some(1), None).expect("switch");
        p.switch_angle(None, 90, Some(0), Some(Some(1))).expect("switch");
        let before = strip(&p);
        let out = p.flatten_multicam(Lane::V1, 1).expect("flatten");
        assert_eq!(out.len(), 6, "three picture and three sound clips");
        assert!(p.lanes.iter().flat_map(|l| &l.clips).all(|c| !is_sequence_source(c.source)), "plain clips only");
        assert_eq!(strip(&p), before);
        assert!(p.undo());
        assert!(p.lane(Lane::V1).iter().all(|c| is_sequence_source(c.source)));
        assert!(p.flatten_multicam(Lane::V1, 7).is_err());
    }

    #[test]
    fn a_multicam_needs_two_to_nine_angles_and_a_video_lane() {
        let (mut p, _) = pair();
        assert!(p.make_multicam(None, &[spec(0, 0, 10, true)], Lane::V1, 0).is_err());
        let ten: Vec<AngleSpec> = (0..10).map(|_| spec(0, 0, 10, true)).collect();
        assert!(p.make_multicam(None, &ten, Lane::V1, 0).is_err());
        assert!(p.make_multicam(None, &[spec(0, 0, 10, true), spec(5, 0, 10, true)], Lane::V1, 0).is_err());
        assert!(p.make_multicam(None, &[spec(0, 0, 10, true), spec(1, 0, 10, true)], Lane::A1, 0).is_err());
        assert!(p.make_multicam(Some("Show"), &[spec(0, 0, 10, true), spec(1, 0, 10, true)], Lane::V1, 0).is_err(), "name taken");
    }

    #[test]
    fn deleting_or_copying_a_multicam_source_keeps_its_angles_straight() {
        let (mut p, made) = pair();
        assert!(p.delete_sequence(made.sequence).is_err(), "a clip plays it");
        let copy = p.duplicate_sequence(made.sequence).expect("copy");
        assert_eq!(p.angle_count(copy), Some(2));
        p.delete_sequence(copy).expect("delete");
        assert_eq!(p.angle_count(copy), None);
        assert!(p.undo());
        assert_eq!(p.angle_count(copy), Some(2));
    }
}
