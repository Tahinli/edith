//! Sequences and compound (nested) clips.
//!
//! A project holds N named timelines -- *sequences* -- and exactly one is
//! active. The active one lives where every other edit already looks for it (the
//! project's own `lanes`, `titles`, `chapters`, ...), so nothing that edits a
//! timeline had to learn about sequences; the others are *parked* here, whole,
//! and [`Project::switch_sequence`] swaps one for the other. Resolution, rate,
//! limiter and the like stay project-wide ([`ProjectSettings`]).
//!
//! A **compound clip** is an ordinary [`Clip`] whose `source` is a sequence
//! rather than a file: `source = SEQ_BASE + id`. Its `[in_frame, out_frame)` is a
//! window of the inner sequence's timeline, counted in timeline frames, and it
//! is placed like any clip. A compound clip on a video lane plays the inner
//! sequence's picture, on an audio lane the inner sequence's mix; "nest" makes
//! one of each, linked.
//!
//! Playback and export never see a compound clip: they read
//! [`Project::render_view`], the active sequence with every compound clip
//! expanded in place into the inner clips it stands for, trimmed to the window
//! and shifted to the clip's start. A nested render is therefore the flat
//! render of the same clips, by construction. A compound clip plays at normal
//! speed forward only ([`Project::set_speed`] and friends refuse it). Its own
//! placement (position, scale, turn, opacity) and colour are laid over each inner clip when the
//! view is built ([`put_over`]); its effects and crop are not applied -- they belong on the
//! clips inside.

use super::*;
use crate::transform::TransformParams;
use std::sync::{Arc, Mutex};

/// A clip whose `source` is at least this names a sequence (`source - SEQ_BASE`),
/// not an entry of [`Project::sources`].
pub const SEQ_BASE: usize = 1 << 30;

/// The most sequences a project holds.
pub const MAX_SEQUENCES: usize = 64;

/// The longest a sequence name may be, in characters.
pub const MAX_SEQUENCE_NAME_CHARS: usize = 100;

/// How deep compound clips may nest when rendering (a guard behind the cycle
/// check, which already makes a loop impossible).
const MAX_NEST_DEPTH: usize = 8;

/// Whether a clip's `source` names a sequence.
pub fn is_sequence_source(source: usize) -> bool {
    source >= SEQ_BASE
}

/// The `source` value of a clip that plays sequence `id`.
pub fn sequence_source(id: u32) -> usize {
    SEQ_BASE + id as usize
}

/// The sequence a clip's `source` names, if it names one.
pub fn sequence_id_of(source: usize) -> Option<u32> {
    source.checked_sub(SEQ_BASE).and_then(|n| u32::try_from(n).ok())
}

/// Everything about a timeline that is not project-wide.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SeqData {
    pub(super) lanes: Vec<LaneData>,
    pub(super) subtitles: Vec<SubtitleTrack>,
    pub(super) cue_edits: Vec<(usize, usize, String)>,
    pub(super) cue_adds: Vec<AddedCue>,
    pub(super) titles: Vec<Title>,
    pub(super) chapters: Vec<Chapter>,
    pub(super) markers: Vec<Marker>,
}

impl SeqData {
    pub(super) fn empty() -> Self {
        Self {
            lanes: LaneData::two_lanes(Vec::new(), Vec::new()),
            subtitles: Vec::new(),
            cue_edits: Vec::new(),
            cue_adds: Vec::new(),
            titles: Vec::new(),
            chapters: Vec::new(),
            markers: Vec::new(),
        }
    }
}

/// The sequence list: which one is active, every name in tab order, and the
/// data of the ones that are not active. In every undo step, so a new, renamed
/// or deleted sequence undoes with the rest.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Seqs {
    pub(super) active: u32,
    pub(super) names: Vec<(u32, String)>,
    pub(super) parked: Vec<(u32, SeqData)>,
    /// The sequences that are multicam clips' sources, each with its angle names in lane order:
    /// angle `n` is the `n`th video lane and the `n`th audio lane of that sequence.
    pub(super) mcam: Vec<(u32, Vec<String>)>,
    /// The sequences Nest made under its default name and that nothing has renamed since. Only
    /// these may be dropped when a break-apart leaves them unplayed; a name never decides it.
    /// Not written to the project file: after a reopen such a tab is kept, never removed.
    pub(super) nested: Vec<u32>,
}

impl Seqs {
    pub(super) fn single() -> Self {
        Self { active: 0, names: vec![(0, "Sequence 1".to_string())], parked: Vec::new(), mcam: Vec::new(), nested: Vec::new() }
    }
}

/// One row of [`Project::sequences`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceInfo {
    pub id: u32,
    pub name: String,
    pub active: bool,
    /// Its length in timeline frames.
    pub frames: u32,
    /// How many compound clips in other sequences play it.
    pub used: usize,
}

/// What [`Project::nest`] made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nested {
    /// The new sequence.
    pub sequence: u32,
    /// The compound clips now standing where the selection was.
    pub clips: Vec<(Lane, usize)>,
}

/// The expanded timeline cache behind [`Project::render_view`].
#[derive(Default)]
pub(super) struct FlatCache(Mutex<Option<(u64, Arc<Project>)>>);

impl Clone for FlatCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for FlatCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FlatCache")
    }
}

impl FlatCache {
    pub(super) fn clear(&self) {
        if let Ok(mut g) = self.0.lock() {
            *g = None;
        }
    }
}

/// What playback and export read: the project itself, or its expanded copy.
pub enum RenderView<'a> {
    Same(&'a Project),
    Flat(Arc<Project>),
}

impl std::ops::Deref for RenderView<'_> {
    type Target = Project;
    fn deref(&self) -> &Project {
        match self {
            RenderView::Same(p) => p,
            RenderView::Flat(p) => p,
        }
    }
}

/// The part of `c` between clip-relative timeline frames `from..to`, placed at
/// `start`. `None` when nothing of it is in there.
fn piece(c: &Clip, from: u32, to: u32, start: u32) -> Option<Clip> {
    let len = c.frames();
    let (from, to) = (from.min(len), to.min(len));
    if from >= to {
        return None;
    }
    let mut p = c.clone();
    if from > 0 {
        let off = c.source_offset(from);
        match c.reversed {
            true => p.out_frame = c.out_frame.saturating_sub(off),
            false => p.in_frame = c.in_frame + off,
        }
        if !c.speed_keys.is_empty() {
            p.speed_keys = ramp_keys_from(&c.speed_keys, from);
        }
        p.fade_in = 0;
    }
    if p.out_frame <= p.in_frame {
        return None;
    }
    let want = to - from;
    if p.frames() > want {
        let src = p.fit_source(want)?;
        match c.reversed {
            true => p.in_frame = p.out_frame - src,
            false => p.out_frame = p.in_frame + src,
        }
        p.fade_out = 0;
        p.transition_out = 0;
    }
    if p.out_frame <= p.in_frame {
        return None;
    }
    p.fade_in = p.fade_in.min(p.frames());
    p.fade_out = p.fade_out.min(p.frames());
    p.transition_out = p.transition_out.min(p.frames());
    p.start = start;
    Some(p)
}

/// The transform and colour tables of the flat copy, which compound clips add composed entries to,
/// with the canvas size a position is measured against.
struct Tables {
    transform: Vec<TransformParams>,
    color: Vec<ColorParams>,
    size: (u32, u32),
}

impl Tables {
    fn transform_at(&mut self, t: TransformParams) -> Option<u16> {
        let at = match self.transform.iter().position(|q| *q == t) {
            Some(at) => at,
            None if self.transform.len() >= usize::from(u16::MAX) => return None,
            None => {
                self.transform.push(t);
                self.transform.len() - 1
            }
        };
        Some(at as u16)
    }

    fn color_at(&mut self, c: ColorParams) -> Option<u16> {
        let at = match self.color.iter().position(|q| *q == c) {
            Some(at) => at,
            None if self.color.len() >= usize::from(u16::MAX) => return None,
            None => {
                self.color.push(c);
                self.color.len() - 1
            }
        };
        Some(at as u16)
    }
}

/// `inner` placed on a canvas that `outer` then moves, scales, turns and fades as one picture:
/// the inner position goes through the outer scale and turn, then the outer offset follows.
/// The outer crop is not applied (it would crop the inner crop's frame, which no single
/// placement says).
fn compose_transform(inner: TransformParams, outer: TransformParams, size: (u32, u32)) -> TransformParams {
    let (w, h) = (size.0.max(1) as f32, size.1.max(1) as f32);
    let (sin, cos) = outer.rotate.to_radians().sin_cos();
    let (x, y) = (inner.pos_x * w * outer.scale, inner.pos_y * h * outer.scale);
    TransformParams {
        pos_x: (x * cos - y * sin) / w + outer.pos_x,
        pos_y: (x * sin + y * cos) / h + outer.pos_y,
        scale: inner.scale * outer.scale,
        rotate: inner.rotate + outer.rotate,
        opacity: inner.opacity * outer.opacity,
        ..inner
    }
}

/// `inner` graded again by `outer`: the sliders stack, the RGB look is the inner one's unless it has none.
fn compose_color(inner: ColorParams, outer: ColorParams) -> ColorParams {
    ColorParams {
        brightness: (inner.brightness + outer.brightness).clamp(-1.0, 1.0),
        contrast: (inner.contrast * outer.contrast).clamp(0.0, 2.0),
        saturation: (inner.saturation * outer.saturation).clamp(0.0, 2.0),
        tint: (inner.tint + outer.tint).clamp(-1.0, 1.0),
        look: if inner.look == crate::color::Look::default() { outer.look } else { inner.look },
    }
}

/// Lays a compound clip's own placement and grade over an inner piece `p`.
fn put_over(p: &mut Clip, outer: Option<TransformParams>, grade: Option<ColorParams>, tables: &mut Tables) {
    if let Some(o) = outer.filter(|o| !o.is_identity()) {
        let size = tables.size;
        let base = p.transform.and_then(|i| tables.transform.get(usize::from(i)).copied()).unwrap_or_default();
        p.transform = tables.transform_at(compose_transform(base, o, size));
        for k in &mut p.transform_keys {
            k.params = compose_transform(k.params, o, size);
        }
        for k in &mut p.opacity_keys {
            k.opacity = (k.opacity * o.opacity).clamp(0.0, 1.0);
        }
    }
    if let Some(g) = grade {
        p.color = match p.color.and_then(|i| tables.color.get(usize::from(i)).copied()) {
            Some(inner) => tables.color_at(compose_color(inner, g)),
            None => tables.color_at(g),
        };
        for k in &mut p.color_keys {
            k.params = compose_color(k.params, g);
        }
    }
}

fn compound_ids(lanes: &[LaneData]) -> impl Iterator<Item = u32> + '_ {
    lanes
        .iter()
        .flat_map(|l| &l.clips)
        .filter_map(|c| sequence_id_of(c.source))
}

fn clean_name(name: &str) -> crate::Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a sequence needs a name".into());
    }
    if name.chars().count() > MAX_SEQUENCE_NAME_CHARS {
        return Err(format!("a sequence name is at most {MAX_SEQUENCE_NAME_CHARS} characters").into());
    }
    if name.chars().any(char::is_control) {
        return Err("a sequence name is one line of plain text".into());
    }
    Ok(name.to_string())
}

impl Project {
    /// Every sequence in tab order.
    pub fn sequences(&self) -> Vec<SequenceInfo> {
        self.seq
            .names
            .iter()
            .map(|(id, name)| {
                let lanes = self.lanes_of(*id);
                let frames = lanes
                    .map(|ls| ls.iter().flat_map(|l| &l.clips).map(Clip::end).max().unwrap_or(0))
                    .unwrap_or(0);
                let used = self
                    .seq
                    .names
                    .iter()
                    .filter(|(other, _)| other != id)
                    .filter_map(|(other, _)| self.lanes_of(*other))
                    .map(|ls| compound_ids(ls).filter(|x| x == id).count())
                    .sum();
                SequenceInfo { id: *id, name: name.clone(), active: *id == self.seq.active, frames, used }
            })
            .collect()
    }

    /// The id of the active sequence.
    pub fn active_sequence(&self) -> u32 {
        self.seq.active
    }

    /// How many sequences the project holds.
    pub fn sequence_count(&self) -> usize {
        self.seq.names.len()
    }

    /// The name of sequence `id`.
    pub fn sequence_name(&self, id: u32) -> Option<&str> {
        self.seq.names.iter().find(|(i, _)| *i == id).map(|(_, n)| n.as_str())
    }

    /// The sequence called `name` (exact, then ignoring case).
    pub fn sequence_named(&self, name: &str) -> Option<u32> {
        let name = name.trim();
        self.seq
            .names
            .iter()
            .find(|(_, n)| n == name)
            .or_else(|| self.seq.names.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)))
            .map(|(i, _)| *i)
    }

    /// The sequence a clip plays, when it is a compound clip.
    pub fn compound_of(&self, clip: &Clip) -> Option<u32> {
        sequence_id_of(clip.source).filter(|id| self.sequence_name(*id).is_some())
    }

    /// How many angles sequence `id` has when it is a multicam source.
    pub fn angle_count(&self, id: u32) -> Option<usize> {
        self.seq.mcam.iter().find(|(i, _)| *i == id).map(|(_, n)| n.len())
    }

    /// The angle names of sequence `id` when it is a multicam source.
    pub fn angle_names(&self, id: u32) -> Option<&[String]> {
        self.seq.mcam.iter().find(|(i, _)| *i == id).map(|(_, n)| n.as_slice())
    }

    /// Whether any sequence holds a compound clip, or there is more than one
    /// sequence: what makes a saved file v45.
    pub fn uses_sequences(&self) -> bool {
        self.seq.names.len() > 1
            || self.lanes.iter().flat_map(|l| &l.clips).any(|c| is_sequence_source(c.source))
    }

    /// The rate and the other time-scaled settings cannot be changed once there
    /// is more than one sequence (the inner windows are in timeline frames).
    pub fn rate_locked(&self) -> bool {
        self.uses_sequences()
    }

    pub(super) fn lanes_of(&self, id: u32) -> Option<&[LaneData]> {
        if id == self.seq.active {
            return Some(&self.lanes);
        }
        self.seq.parked.iter().find(|(i, _)| *i == id).map(|(_, d)| d.lanes.as_slice())
    }

    /// Whether sequence `from` plays, directly or through others, sequence `target`.
    fn reaches(&self, from: u32, target: u32) -> bool {
        let mut seen = vec![from];
        let mut stack = vec![from];
        while let Some(id) = stack.pop() {
            let Some(lanes) = self.lanes_of(id) else { continue };
            for next in compound_ids(lanes) {
                if next == target {
                    return true;
                }
                if !seen.contains(&next) {
                    seen.push(next);
                    stack.push(next);
                }
            }
        }
        false
    }

    /// Why `clip` may not be placed in the active sequence, if it may not: it
    /// plays a sequence that does not exist, or one that plays (or is) the
    /// active sequence.
    pub fn compound_refusal(&self, clip: &Clip) -> Option<String> {
        let id = sequence_id_of(clip.source)?;
        let Some(name) = self.sequence_name(id) else {
            return Some(format!("there is no sequence {id}"));
        };
        if id == self.seq.active {
            return Some(format!("\"{name}\" cannot contain itself"));
        }
        if self.reaches(id, self.seq.active) {
            let me = self.sequence_name(self.seq.active).unwrap_or("this sequence");
            return Some(format!("\"{name}\" already contains \"{me}\": that would nest a sequence inside itself"));
        }
        None
    }

    /// Whether the clip is refused by [`compound_refusal`](Project::compound_refusal).
    pub(super) fn cycles(&self, clip: &Clip) -> bool {
        is_sequence_source(clip.source) && self.compound_refusal(clip).is_some()
    }

    /// Checks every compound clip of every sequence names a real sequence and
    /// that no sequence plays itself. The loader's gate.
    pub(crate) fn check_sequences(&self) -> crate::Result<()> {
        for (id, name) in &self.seq.names {
            let Some(lanes) = self.lanes_of(*id) else {
                return Err(format!("sequence {id} has no data").into());
            };
            for inner in compound_ids(lanes) {
                if self.sequence_name(inner).is_none() {
                    return Err(format!("\"{name}\" plays sequence {inner}, which the file does not hold").into());
                }
            }
            for c in lanes.iter().flat_map(|l| &l.clips).filter(|c| is_sequence_source(c.source)) {
                let inner = sequence_id_of(c.source).unwrap_or(u32::MAX);
                if inner == *id || self.reaches(inner, *id) {
                    return Err(format!("\"{name}\" contains itself").into());
                }
            }
        }
        Ok(())
    }

    /// Empties the sequence list a fresh project starts with ([`Seqs::single`], the
    /// placeholder "Sequence 1" with id 0). A loader calls this before it absorbs a
    /// file's sequences: the placeholder is not a sequence the file holds, and left
    /// in place it collides with a parked sequence 0 ("holds sequence 0 twice").
    pub(crate) fn clear_sequences(&mut self) {
        self.seq.names.clear();
        self.seq.parked.clear();
        self.seq.mcam.clear();
        self.seq.nested.clear();
    }

    /// Makes `data` (read off a loaded standalone project) a parked sequence.
    /// Its effect-table indexes are shifted past this project's tables.
    pub(crate) fn absorb_sequence(&mut self, id: u32, name: &str, other: Project) -> crate::Result<()> {
        if self.seq.names.iter().any(|(i, _)| *i == id) {
            return Err(format!("the file holds sequence {id} twice").into());
        }
        let fits = |have: usize, add: usize| have + add <= usize::from(u16::MAX);
        if !fits(self.eq.len(), other.eq.len())
            || !fits(self.color.len(), other.color.len())
            || !fits(self.transform.len(), other.transform.len())
        {
            return Err("the sequences hold more effect settings than a project can".into());
        }
        let (eq_at, color_at, transform_at) = (self.eq.len(), self.color.len(), self.transform.len());
        let shift = |i: u16, by: usize| (usize::from(i) + by) as u16;
        let mut lanes = other.lanes;
        for c in lanes.iter_mut().flat_map(|l| &mut l.clips) {
            c.eq = c.eq.map(|i| shift(i, eq_at));
            c.color = c.color.map(|i| shift(i, color_at));
            c.transform = c.transform.map(|i| shift(i, transform_at));
        }
        self.eq.extend(other.eq);
        self.color.extend(other.color);
        self.transform.extend(other.transform);
        self.next_link = self.next_link.max(other.next_link);
        self.seq.names.push((id, name.to_string()));
        self.seq.parked.push((
            id,
            SeqData {
                lanes,
                subtitles: other.subtitles,
                cue_edits: other.cue_edits,
                cue_adds: other.cue_adds,
                titles: other.titles,
                chapters: other.chapters,
                markers: other.markers,
            },
        ));
        self.seq_next = self.seq_next.max(id + 1);
        Ok(())
    }

    /// Names the active sequence and the tab order, as a load reads them off a file.
    pub(crate) fn set_sequence_meta(&mut self, active: u32, active_name: &str, order: &[u32]) {
        self.seq.active = active;
        match self.seq.names.iter_mut().find(|(i, _)| *i == active) {
            Some(row) => row.1 = active_name.to_string(),
            None => self.seq.names.insert(0, (active, active_name.to_string())),
        }
        let mut sorted: Vec<(u32, String)> = Vec::new();
        for id in order {
            if let Some(row) = self.seq.names.iter().find(|(i, _)| i == id)
                && !sorted.iter().any(|(i, _)| i == id)
            {
                sorted.push(row.clone());
            }
        }
        for row in &self.seq.names {
            if !sorted.iter().any(|(i, _)| *i == row.0) {
                sorted.push(row.clone());
            }
        }
        self.seq.names = sorted;
        self.seq_next = self.seq_next.max(self.seq.names.iter().map(|(i, _)| i + 1).max().unwrap_or(1));
    }

    /// A project that has sequence `id` as its active one and nothing else
    /// different, for a writer to take parts off. The undo history is left out.
    pub(crate) fn as_sequence(&self, id: u32) -> crate::Result<Project> {
        let mut p = self.raw_snapshot();
        p.switch_sequence(id)?;
        Ok(p)
    }

    /// The multicam sources and their angle names, for the writer.
    pub(crate) fn multicam_rows(&self) -> Vec<(u32, Vec<String>)> {
        self.seq.mcam.clone()
    }

    /// Flags the sequences a file names as multicam sources (`mcam` lines, grouped by sequence in
    /// order). Refused when a sequence is missing or does not hold one video and one sound lane
    /// per angle.
    pub(crate) fn set_multicam(&mut self, lines: &[(u32, String)]) -> crate::Result<()> {
        let mut mcam: Vec<(u32, Vec<String>)> = Vec::new();
        for (id, name) in lines {
            match mcam.iter_mut().find(|(i, _)| i == id) {
                Some((_, names)) => names.push(name.clone()),
                None => mcam.push((*id, vec![name.clone()])),
            }
        }
        for (id, names) in &mcam {
            let Some(lanes) = self.lanes_of(*id) else {
                return Err(format!("multicam names sequence {id}, which the file does not hold").into());
            };
            let count = |k: LaneKind| lanes.iter().filter(|l| l.kind == k).count();
            if names.len() > super::multicam::MAX_ANGLES || count(LaneKind::Video) != names.len() || count(LaneKind::Audio) != names.len() {
                return Err(format!("multicam sequence {id} must hold one picture lane and one sound lane per angle").into());
            }
        }
        self.seq.mcam = mcam;
        Ok(())
    }

    /// Every sequence id and name in tab order, with the active one marked.
    pub(crate) fn sequence_rows(&self) -> Vec<(u32, String, bool)> {
        self.seq.names.iter().map(|(i, n)| (*i, n.clone(), *i == self.seq.active)).collect()
    }

    /// The name a new nested sequence starts from when none is given: the first clip nested,
    /// so the tab says what is inside ("Interview (nested)"). A compound clip names its sequence.
    fn nested_base_name(&self, picks: &[(Lane, usize)]) -> String {
        let first = picks.first().and_then(|&(lane, i)| self.lane(lane).get(i)).map(|c| c.source);
        let stem = match first {
            Some(source) => match sequence_id_of(source) {
                Some(id) => self.sequence_name(id).map(str::to_string),
                None => self.sources.get(source).and_then(|s| s.path.file_stem()).map(|s| s.to_string_lossy().into_owned()),
            },
            None => None,
        };
        match stem {
            Some(stem) if !stem.is_empty() => format!("{stem} (nested)"),
            _ => "Nested Sequence".to_string(),
        }
    }

    pub(super) fn fresh_name(&self, base: &str) -> String {
        if self.sequence_named(base).is_none() {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|n| self.sequence_named(n).is_none())
            .unwrap_or_else(|| base.to_string())
    }

    pub(super) fn unique_name(&self, name: &str, except: Option<u32>) -> crate::Result<String> {
        let name = clean_name(name)?;
        if let Some(other) = self.seq.names.iter().find(|(i, n)| n == &name && Some(*i) != except) {
            return Err(format!("a sequence named \"{}\" exists already", other.1).into());
        }
        Ok(name)
    }

    /// Adds an empty sequence (`V1` and `A1`) after the active one in the tab
    /// order. One undo step. It is not switched to; see [`switch_sequence`](Project::switch_sequence).
    pub fn new_sequence(&mut self, name: Option<&str>) -> crate::Result<u32> {
        if self.seq.names.len() >= MAX_SEQUENCES {
            return Err(format!("a project holds at most {MAX_SEQUENCES} sequences").into());
        }
        let name = match name {
            Some(n) => self.unique_name(n, None)?,
            None => self.fresh_name(&format!("Sequence {}", self.seq.names.len() + 1)),
        };
        self.snapshot();
        let id = self.seq_next;
        self.seq_next += 1;
        let at = self.seq.names.iter().position(|(i, _)| *i == self.seq.active).map_or(self.seq.names.len(), |p| p + 1);
        self.seq.names.insert(at, (id, name));
        self.seq.parked.push((id, SeqData::empty()));
        self.flat.clear();
        Ok(id)
    }

    /// Renames sequence `id`. One undo step.
    pub fn rename_sequence(&mut self, id: u32, name: &str) -> crate::Result<()> {
        if self.sequence_name(id).is_none() {
            return Err(format!("there is no sequence {id}").into());
        }
        let name = self.unique_name(name, Some(id))?;
        if self.sequence_name(id) == Some(name.as_str()) {
            return Ok(());
        }
        self.snapshot();
        if let Some(row) = self.seq.names.iter_mut().find(|(i, _)| *i == id) {
            row.1 = name;
        }
        self.seq.nested.retain(|i| *i != id);
        Ok(())
    }

    /// A copy of sequence `id` right after it in the tab order, named "<name> copy".
    /// Compound clips in the copy still play the same inner sequences. One undo step.
    pub fn duplicate_sequence(&mut self, id: u32) -> crate::Result<u32> {
        let Some(name) = self.sequence_name(id).map(str::to_string) else {
            return Err(format!("there is no sequence {id}").into());
        };
        if self.seq.names.len() >= MAX_SEQUENCES {
            return Err(format!("a project holds at most {MAX_SEQUENCES} sequences").into());
        }
        let data = if id == self.seq.active {
            self.active_data()
        } else {
            self.seq.parked.iter().find(|(i, _)| *i == id).map(|(_, d)| d.clone()).ok_or("sequence data is missing")?
        };
        let copy_name = self.fresh_name(&format!("{name} copy"));
        self.snapshot();
        let new = self.seq_next;
        self.seq_next += 1;
        let at = self.seq.names.iter().position(|(i, _)| *i == id).map_or(self.seq.names.len(), |p| p + 1);
        self.seq.names.insert(at, (new, copy_name));
        self.seq.parked.push((new, data));
        if let Some(names) = self.seq.mcam.iter().find(|(i, _)| *i == id).map(|(_, n)| n.clone()) {
            self.seq.mcam.push((new, names));
        }
        self.flat.clear();
        Ok(new)
    }

    /// Deletes sequence `id`; the neighbour in the tab order becomes active when
    /// it was the active one. Refused for the last sequence and for one that a
    /// compound clip still plays. One undo step. Returns the active sequence after.
    pub fn delete_sequence(&mut self, id: u32) -> crate::Result<u32> {
        let Some(name) = self.sequence_name(id).map(str::to_string) else {
            return Err(format!("there is no sequence {id}").into());
        };
        if self.seq.names.len() < 2 {
            return Err("a project keeps at least one sequence".into());
        }
        let users: Vec<String> = self
            .seq
            .names
            .iter()
            .filter(|(other, _)| *other != id)
            .filter(|(other, _)| self.lanes_of(*other).is_some_and(|ls| compound_ids(ls).any(|x| x == id)))
            .map(|(_, n)| format!("\"{n}\""))
            .collect();
        if !users.is_empty() {
            return Err(format!(
                "\"{name}\" is played by compound clips in {}: break them apart or delete them first",
                users.join(", ")
            )
            .into());
        }
        self.snapshot();
        if id == self.seq.active {
            let pos = self.seq.names.iter().position(|(i, _)| *i == id).unwrap_or(0);
            let next = if pos > 0 { self.seq.names[pos - 1].0 } else { self.seq.names[1].0 };
            self.switch_sequence(next)?;
        }
        self.seq.names.retain(|(i, _)| *i != id);
        self.seq.parked.retain(|(i, _)| *i != id);
        self.seq.mcam.retain(|(i, _)| *i != id);
        self.seq.nested.retain(|i| *i != id);
        self.flat.clear();
        Ok(self.seq.active)
    }

    fn active_data(&self) -> SeqData {
        SeqData {
            lanes: self.lanes.clone(),
            subtitles: self.subtitles.clone(),
            cue_edits: self.cue_edits.clone(),
            cue_adds: self.cue_adds.clone(),
            titles: self.titles.clone(),
            chapters: self.chapters.clone(),
            markers: self.markers.clone(),
        }
    }

    /// Makes sequence `id` the active one. Not an undo step (it edits nothing),
    /// and it leaves the redo stack alone.
    pub fn switch_sequence(&mut self, id: u32) -> crate::Result<()> {
        if id == self.seq.active {
            return Ok(());
        }
        let Some(at) = self.seq.parked.iter().position(|(i, _)| *i == id) else {
            return Err(format!("there is no sequence {id}").into());
        };
        let (_, inbound) = self.seq.parked.swap_remove(at);
        let outbound = SeqData {
            lanes: std::mem::replace(&mut self.lanes, inbound.lanes),
            subtitles: std::mem::replace(&mut self.subtitles, inbound.subtitles),
            cue_edits: std::mem::replace(&mut self.cue_edits, inbound.cue_edits),
            cue_adds: std::mem::replace(&mut self.cue_adds, inbound.cue_adds),
            titles: std::mem::replace(&mut self.titles, inbound.titles),
            chapters: std::mem::replace(&mut self.chapters, inbound.chapters),
            markers: std::mem::replace(&mut self.markers, inbound.markers),
        };
        let old = self.seq.active;
        self.seq.parked.push((old, outbound));
        self.seq.active = id;
        self.flat.clear();
        Ok(())
    }

    /// How many clips play `source` in each parked sequence that uses it, as `(sequence id, count)`:
    /// the library's "is it on the timeline" counts these as uses.
    pub fn source_uses_parked(&self, source: usize) -> Vec<(u32, usize)> {
        self.seq
            .parked
            .iter()
            .map(|(id, d)| (*id, d.lanes.iter().flat_map(|l| &l.clips).filter(|c| c.source == source).count()))
            .filter(|(_, n)| *n > 0)
            .collect()
    }

    /// The clips a compound clip `c` on a `kind` lane stands for, as the render sees them:
    /// trimmed to its window and placed relative to its own start. What the bench draws a
    /// compound clip from (thumbnails of the picture, waveform of the mix).
    pub fn compound_pieces(&self, kind: LaneKind, c: &Clip) -> Vec<Clip> {
        if !is_sequence_source(c.source) {
            return Vec::new();
        }
        let mut tables = Tables { transform: self.transform.clone(), color: self.color.clone(), size: self.settings.resolution };
        let lane = LaneData::new(kind, vec![c.clone()]);
        self.expand(&[lane], 0, &mut tables)
            .into_iter()
            .flat_map(|l| l.clips)
            .map(|mut p| {
                p.start = p.start.saturating_sub(c.start);
                p
            })
            .collect()
    }

    /// The sequence the compound clip at `lane`/`idx` plays.
    pub fn compound_at(&self, lane: Lane, idx: usize) -> Option<u32> {
        self.lane(lane).get(idx).and_then(|c| self.compound_of(c))
    }

    /// Replaces the clips at `picks` with one compound clip per kind (picture,
    /// sound) of a new sequence that holds them. The new sequence keeps the
    /// picks' lanes in order (compacted), the clips shifted to start at 0. One
    /// undo step. Refused when another clip sits in the way on a lane involved.
    pub fn nest(&mut self, picks: &[(Lane, usize)], name: Option<&str>) -> crate::Result<Nested> {
        if picks.is_empty() {
            return Err("nest needs at least one clip".into());
        }
        if self.seq.names.len() >= MAX_SEQUENCES {
            return Err(format!("a project holds at most {MAX_SEQUENCES} sequences").into());
        }
        let mut picks: Vec<(Lane, usize)> = picks.to_vec();
        picks.sort_by_key(|(l, i)| (l.kind as u8, l.ord, *i));
        picks.dedup();
        for &(lane, idx) in &picks {
            if lane.kind == LaneKind::Subtitle {
                return Err("captions cannot be nested".into());
            }
            if self.lane(lane).get(idx).is_none() {
                return Err(format!("there is no clip {} #{idx}", lane.label()).into());
            }
        }
        // A take goes whole: the linked partners of a pick (the sound of a picture) come too.
        let links: Vec<u32> = picks.iter().filter_map(|&(l, i)| self.lane(l)[i].link).collect();
        if !links.is_empty() {
            for lane in self.lanes().into_iter().filter(|l| l.kind != LaneKind::Subtitle) {
                for (i, c) in self.lane(lane).iter().enumerate() {
                    if c.link.is_some_and(|k| links.contains(&k)) && !picks.contains(&(lane, i)) {
                        picks.push((lane, i));
                    }
                }
            }
            picks.sort_by_key(|(l, i)| (l.kind as u8, l.ord, *i));
        }
        let lo = picks.iter().map(|&(l, i)| self.lane(l)[i].start).min().unwrap_or(0);
        let hi = picks.iter().map(|&(l, i)| self.lane(l)[i].end()).max().unwrap_or(0);
        let mut lanes_used: Vec<Lane> = picks.iter().map(|p| p.0).collect();
        lanes_used.dedup();
        for &lane in &lanes_used {
            for (i, c) in self.lane(lane).iter().enumerate() {
                if !picks.contains(&(lane, i)) && c.start < hi && c.end() > lo {
                    return Err(format!(
                        "{} has a clip at {} between the selected ones: select it too, or nest a range with nothing else on those lanes",
                        lane.label(),
                        c.start
                    )
                    .into());
                }
            }
        }
        let (name, default_named) = match name {
            Some(n) => (self.unique_name(n, None)?, false),
            None => (self.fresh_name(&self.nested_base_name(&picks)), true),
        };
        // The inner lanes: the picked video lanes in order, then the audio ones.
        let mut inner: Vec<LaneData> = Vec::new();
        let mut first_video = None;
        let mut first_audio = None;
        for &lane in &lanes_used {
            let clips: Vec<Clip> = picks
                .iter()
                .filter(|p| p.0 == lane)
                .map(|&(l, i)| {
                    let mut c = self.lane(l)[i].clone();
                    c.start -= lo;
                    c
                })
                .collect();
            inner.push(LaneData::new(lane.kind, clips));
            match lane.kind {
                LaneKind::Video if first_video.is_none() => first_video = Some(lane),
                LaneKind::Audio if first_audio.is_none() => first_audio = Some(lane),
                _ => {}
            }
        }
        self.snapshot();
        let id = self.seq_next;
        self.seq_next += 1;
        // Out of the parent, last index first so the others stay put.
        for &lane in &lanes_used {
            let mut idxs: Vec<usize> = picks.iter().filter(|p| p.0 == lane).map(|p| p.1).collect();
            idxs.sort_unstable_by(|a, b| b.cmp(a));
            if let Some(clips) = self.lane_mut(lane) {
                for i in idxs {
                    clips.remove(i);
                }
            }
        }
        let at = self.seq.names.iter().position(|(i, _)| *i == self.seq.active).map_or(self.seq.names.len(), |p| p + 1);
        self.seq.names.insert(at, (id, name));
        if default_named {
            self.seq.nested.push(id);
        }
        let mut data = SeqData::empty();
        data.lanes = inner;
        self.seq.parked.push((id, data));
        let link = (first_video.is_some() && first_audio.is_some()).then(|| self.new_link());
        let mut made = Vec::new();
        for lane in [first_video, first_audio].into_iter().flatten() {
            let mut c = Clip::source_range(sequence_source(id), 0, hi - lo);
            c.start = lo;
            c.link = link;
            if let Some(clips) = self.lane_mut(lane) {
                let at = clips.partition_point(|x| x.start < lo);
                clips.insert(at, c);
                made.push((lane, at));
            }
        }
        self.flat.clear();
        Ok(Nested { sequence: id, clips: made })
    }

    /// Puts the clips of each compound clip at `picks` (and its linked partner)
    /// back into the sequence, in place of the compound clip: the inner lanes
    /// land on the compound clip's lane and the ones above it. One undo step.
    /// Refused, changing nothing, when another clip is in the way. Captions and
    /// titles inside are not carried.
    pub fn break_apart(&mut self, picks: &[(Lane, usize)]) -> crate::Result<Vec<(Lane, usize)>> {
        let mut set: Vec<(Lane, usize)> = Vec::new();
        for &(lane, idx) in picks {
            let Some(c) = self.lane(lane).get(idx) else {
                return Err(format!("there is no clip {} #{idx}", lane.label()).into());
            };
            if self.compound_of(c).is_none() {
                return Err(format!("{} #{idx} is not a compound clip", lane.label()).into());
            }
            if !set.contains(&(lane, idx)) {
                set.push((lane, idx));
            }
            if let Some(link) = c.link {
                for other in self.lanes() {
                    for (i, o) in self.lane(other).iter().enumerate() {
                        if o.link == Some(link) && o.source == c.source && !set.contains(&(other, i)) {
                            set.push((other, i));
                        }
                    }
                }
            }
        }
        if set.is_empty() {
            return Err("break apart needs a compound clip".into());
        }
        // The pieces each compound clip stands for, by target lane, with which compound
        // clip each came out of (its inner link ids are only meaningful per compound).
        let mut plan: Vec<(Lane, Clip, usize)> = Vec::new();
        for (n, &(lane, idx)) in set.iter().enumerate() {
            let c = self.lane(lane)[idx].clone();
            let id = self.compound_of(&c).unwrap_or(0);
            let Some(inner) = self.lanes_of(id) else {
                return Err(format!("sequence {id} is missing").into());
            };
            let mut same: Vec<&LaneData> = inner.iter().filter(|l| l.kind == lane.kind).collect();
            if let Some(n) = self.angle_count(id) {
                let pick = usize::from(c.angle.of(lane.kind)).min(n.saturating_sub(1));
                same = same.get(pick).copied().into_iter().collect();
            }
            for (slot, il) in same.into_iter().enumerate() {
                let target = Lane::new(lane.kind, lane.ord + slot);
                for q in &il.clips {
                    let lo = q.start.max(c.in_frame);
                    let hi = q.end().min(c.out_frame);
                    if lo >= hi {
                        continue;
                    }
                    if let Some(p) = piece(q, lo - q.start, hi - q.start, c.start + (lo - c.in_frame)) {
                        plan.push((target, p, n));
                    }
                }
            }
        }
        // Room: nothing but the compound clips being broken may be in the way.
        for (target, p, _) in &plan {
            for (i, o) in self.lane(*target).iter().enumerate() {
                let broken = set.contains(&(*target, i));
                if !broken && o.start < p.end() && o.end() > p.start {
                    return Err(format!(
                        "{} has a clip at {} where the compound clip's contents land: move it first",
                        target.label(),
                        o.start
                    )
                    .into());
                }
            }
        }
        // The sequences the broken compound clips played, for the tidy-up below.
        let mut inner_ids: Vec<u32> = set.iter().filter_map(|&(l, i)| self.compound_of(&self.lane(l)[i])).collect();
        inner_ids.sort_unstable();
        inner_ids.dedup();
        self.snapshot();
        // Lanes the plan needs that do not exist yet.
        for (target, _, _) in &plan {
            while self.lane_count(target.kind) <= target.ord {
                self.lanes.push(LaneData::new(target.kind, Vec::new()));
            }
        }
        // Out with the compound clips, last index first.
        let mut removals = set.clone();
        removals.sort_by_key(|a| std::cmp::Reverse((a.0.kind as u8, a.0.ord, a.1)));
        for (lane, idx) in removals {
            if let Some(clips) = self.lane_mut(lane) {
                clips.remove(idx);
            }
        }
        // Fresh link ids: one per (compound clip, inner link), so two copies of one
        // sequence never share a group.
        let mut fresh: Vec<((usize, u32), u32)> = Vec::new();
        let mut placed: Vec<(Lane, u32)> = Vec::new();
        for (target, mut p, owner) in plan {
            if let Some(l) = p.link {
                let id = match fresh.iter().find(|(k, _)| *k == (owner, l)) {
                    Some((_, id)) => *id,
                    None => {
                        let id = self.new_link();
                        fresh.push(((owner, l), id));
                        id
                    }
                };
                p.link = Some(id);
            }
            placed.push((target, p.start));
            if let Some(clips) = self.lane_mut(target) {
                let at = clips.partition_point(|x| x.start < p.start);
                clips.insert(at, p);
            }
        }
        let made: Vec<(Lane, usize)> = placed
            .into_iter()
            .filter_map(|(lane, start)| self.lane(lane).iter().position(|c| c.start == start).map(|i| (lane, i)))
            .collect();
        // A sequence Nest made (under its default name, never renamed) that nothing plays any more
        // would only be an orphan tab: it goes in this same undo step.
        for id in inner_ids {
            let orphan = self.seq.nested.contains(&id)
                && !self.seq.mcam.iter().any(|(i, _)| *i == id)
                && !self.seq.names.iter().any(|(other, _)| {
                    *other != id && self.lanes_of(*other).is_some_and(|ls| compound_ids(ls).any(|x| x == id))
                })
                && id != self.seq.active;
            if orphan {
                self.seq.names.retain(|(i, _)| *i != id);
                self.seq.parked.retain(|(i, _)| *i != id);
                self.seq.nested.retain(|i| *i != id);
            }
        }
        self.flat.clear();
        Ok(made)
    }

    /// Places a compound clip playing sequence `seq` on `lane` at `at`, over the
    /// sequence's whole length, replacing what is there (the clip is placed like
    /// any other). Refused when `seq` would contain the active sequence.
    pub fn place_sequence(&mut self, seq: u32, lane: Lane, at: u32) -> crate::Result<()> {
        let Some(name) = self.sequence_name(seq).map(str::to_string) else {
            return Err(format!("there is no sequence {seq}").into());
        };
        let len = self.sequence_frames(seq);
        if len == 0 {
            return Err(format!("\"{name}\" is empty: there is nothing to play").into());
        }
        let clip = Clip::source_range(sequence_source(seq), 0, len);
        if let Some(why) = self.compound_refusal(&clip) {
            return Err(why.into());
        }
        if lane.kind == LaneKind::Subtitle {
            return Err("a compound clip goes on a video or audio lane".into());
        }
        if !self.place(lane, at, clip) {
            return Err(format!("{} cannot take the clip there", lane.label()).into());
        }
        Ok(())
    }

    /// The length of sequence `id` in timeline frames.
    pub fn sequence_frames(&self, id: u32) -> u32 {
        self.lanes_of(id).map_or(0, |ls| ls.iter().flat_map(|l| &l.clips).map(Clip::end).max().unwrap_or(0))
    }

    /// A copy of the project with the undo history left out and everything else
    /// as it is, compound clips included.
    pub fn raw_snapshot(&self) -> Self {
        Self {
            sources: self.sources.clone(),
            lanes: self.lanes.clone(),
            eq: self.eq.clone(),
            color: self.color.clone(),
            transform: self.transform.clone(),
            history: Vec::new(),
            redo: Vec::new(),
            pushes: self.history_pushes(),
            revision: self.revision(),
            batching: false,
            batch_snapped: false,
            pending: None,
            hold_cap: false,
            next_link: self.next_link,
            subtitles: self.subtitles.clone(),
            cue_edits: self.cue_edits.clone(),
            cue_adds: self.cue_adds.clone(),
            titles: self.titles.clone(),
            chapters: self.chapters.clone(),
            markers: self.markers.clone(),
            settings: self.settings.clone(),
            seq: self.seq.clone(),
            bins: self.bins.clone(),
            bin_next: self.bin_next,
            seq_next: self.seq_next,
            flat: FlatCache::default(),
        }
    }

    /// The project an export or a render reads: the active sequence with its
    /// compound clips expanded, the undo history left behind.
    pub fn export_snapshot(&self) -> Self {
        if !self.lanes.iter().flat_map(|l| &l.clips).any(|c| is_sequence_source(c.source) || c.mark.off) {
            let mut p = self.raw_snapshot();
            p.seq = Seqs { active: p.seq.active, names: p.seq.names.clone(), parked: Vec::new(), mcam: p.seq.mcam.clone(), nested: p.seq.nested.clone() };
            return p;
        }
        self.flatten()
    }

    /// [`export_snapshot`](Project::export_snapshot) of sequence `seq` (the active one for `None`).
    pub fn render_snapshot(&self, seq: Option<u32>) -> crate::Result<Self> {
        match seq {
            Some(id) if id != self.seq.active => {
                let mut p = self.raw_snapshot();
                p.switch_sequence(id)?;
                Ok(p.export_snapshot())
            }
            _ => Ok(self.export_snapshot()),
        }
    }

    fn flatten(&self) -> Self {
        let mut p = self.raw_snapshot();
        let mut tables = Tables { transform: self.transform.clone(), color: self.color.clone(), size: self.settings.resolution };
        p.lanes = self.expand(&self.lanes, 0, &mut tables);
        p.transform = tables.transform;
        p.color = tables.color;
        p.seq = Seqs { active: p.seq.active, names: p.seq.names.clone(), parked: Vec::new(), mcam: p.seq.mcam.clone(), nested: p.seq.nested.clone() };
        p
    }

    /// What playback and export read: this project when it holds no compound
    /// clip, otherwise its expansion, kept until the timeline changes.
    pub fn render_view(&self) -> RenderView<'_> {
        // A switched-off clip is left out of the render like a compound one is expanded: by a copy.
        if !self.lanes.iter().flat_map(|l| &l.clips).any(|c| is_sequence_source(c.source) || c.mark.off) {
            return RenderView::Same(self);
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for l in &self.lanes {
            (l.kind as u8, l.gain_db.to_bits(), l.hidden, l.muted, l.clips.len()).hash(&mut h);
            for c in &l.clips {
                (c.start, c.in_frame, c.out_frame, c.source, c.speed.permille(), c.reversed, c.angle.video, c.angle.audio).hash(&mut h);
                (c.fade_in, c.fade_out, c.gain_db.to_bits(), c.transition_out, c.eq, c.color, c.transform, c.mark).hash(&mut h);
            }
        }
        format!("{:?}", self.settings).hash(&mut h);
        (self.history_pushes(), self.titles.len(), self.chapters.len(), self.markers.len(), self.subtitles.len()).hash(&mut h);
        let key = h.finish();
        let mut guard = match self.flat.0.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        if let Some((k, p)) = guard.as_ref()
            && *k == key
        {
            return RenderView::Flat(Arc::clone(p));
        }
        let flat = Arc::new(self.flatten());
        *guard = Some((key, Arc::clone(&flat)));
        RenderView::Flat(flat)
    }

    fn expand(&self, lanes: &[LaneData], depth: usize, tables: &mut Tables) -> Vec<LaneData> {
        let mut out: Vec<LaneData> = Vec::with_capacity(lanes.len());
        let mut audio_map: Vec<usize> = Vec::new();
        let mut audio_out = 0usize;
        for lane in lanes {
            let compound = lane.kind != LaneKind::Subtitle && lane.clips.iter().any(|c| is_sequence_source(c.source));
            if !compound {
                if lane.kind == LaneKind::Audio {
                    audio_map.push(audio_out);
                    audio_out += 1;
                }
                let mut whole = lane.clone();
                whole.clips.retain(|c| !c.mark.off);
                out.push(whole);
                continue;
            }
            let mut base = lane.clone();
            base.clips.retain(|c| !is_sequence_source(c.source) && !c.mark.off);
            let mut extra: Vec<LaneData> = Vec::new();
            for c in lane.clips.iter().filter(|c| is_sequence_source(c.source) && !c.mark.off) {
                let Some(id) = sequence_id_of(c.source) else { continue };
                if depth >= MAX_NEST_DEPTH {
                    continue;
                }
                let Some(inner_lanes) = self.lanes_of(id) else { continue };
                let inner = self.expand(inner_lanes, depth + 1, tables);
                let mut same: Vec<&LaneData> = inner.iter().filter(|l| l.kind == lane.kind).collect();
                if let Some(n) = self.angle_count(id) {
                    // A multicam clip plays one angle: that angle's lane, alone.
                    let pick = usize::from(c.angle.of(lane.kind)).min(n.saturating_sub(1));
                    same = same.get(pick).copied().into_iter().collect();
                }
                for (slot, il) in same.into_iter().enumerate() {
                    let skip = match lane.kind {
                        LaneKind::Video => il.hidden,
                        _ => il.muted,
                    };
                    let target: &mut LaneData = if slot == 0 {
                        &mut base
                    } else {
                        while extra.len() < slot {
                            let mut e = LaneData::new(lane.kind, Vec::new());
                            e.gain_db = lane.gain_db;
                            e.hidden = lane.hidden;
                            e.muted = lane.muted;
                            e.voice = lane.voice.clone();
                            extra.push(e);
                        }
                        &mut extra[slot - 1]
                    };
                    if skip {
                        continue;
                    }
                    for q in il.clips.iter().filter(|q| !q.mark.off) {
                        let lo = q.start.max(c.in_frame);
                        let hi = q.end().min(c.out_frame);
                        if lo >= hi {
                            continue;
                        }
                        let Some(mut p) = piece(q, lo - q.start, hi - q.start, c.start + (lo - c.in_frame)) else { continue };
                        if lane.kind == LaneKind::Audio {
                            p.gain_db = (p.gain_db + il.gain_db + c.gain_db).clamp(MIN_CLIP_GAIN_DB, MAX_CLIP_GAIN_DB);
                            if lo == c.in_frame && lo >= q.start {
                                p.fade_in = p.fade_in.max(c.fade_in.min(p.frames()));
                            }
                            if hi == c.out_frame && hi <= q.end() {
                                p.fade_out = p.fade_out.max(c.fade_out.min(p.frames()));
                            }
                        }
                        p.link = None;
                        if lane.kind == LaneKind::Video {
                            let outer = c.transform.and_then(|i| tables.transform.get(usize::from(i)).copied());
                            let grade = c.color.and_then(|i| tables.color.get(usize::from(i)).copied());
                            put_over(&mut p, outer, grade, tables);
                        }
                        target.clips.push(p);
                    }
                }
            }
            base.clips.sort_by_key(|c| c.start);
            if lane.kind == LaneKind::Audio {
                audio_map.push(audio_out);
                audio_out += 1 + extra.len();
            }
            out.push(base);
            for mut e in extra {
                e.clips.sort_by_key(|c| c.start);
                out.push(e);
            }
        }
        if audio_map.iter().enumerate().any(|(i, m)| i != *m) {
            for l in out.iter_mut().filter(|l| l.kind == LaneKind::Audio) {
                for d in &mut l.ducks {
                    if let Some(m) = audio_map.get(d.under) {
                        d.under = *m;
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "/tmp/edith-sequence-test.mp4";

    fn base() -> Project {
        let mut p = Project::single(FILE, 100);
        p.import("/tmp/edith-sequence-test-b.mp4", 0);
        p
    }

    fn clip(src: usize, a: u32, b: u32, start: u32) -> Clip {
        let mut c = Clip::source_range(src, a, b);
        c.start = start;
        c
    }

    type Strip = Vec<(LaneKind, Vec<(u32, u32, u32, usize)>)>;
    fn strip(p: &Project) -> Strip {
        p.lanes
            .iter()
            .map(|l| (l.kind, l.clips.iter().map(|c| (c.start, c.in_frame, c.out_frame, c.source)).collect()))
            .collect()
    }

    #[test]
    fn one_sequence_by_default() {
        let p = base();
        let rows = p.sequences();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].active);
        assert_eq!(rows[0].frames, 100);
        assert!(!p.uses_sequences());
    }

    #[test]
    fn new_rename_duplicate_delete_switch_and_undo() {
        let mut p = base();
        let b = p.new_sequence(Some("B")).unwrap();
        assert_eq!(p.sequence_count(), 2);
        assert!(p.new_sequence(Some("B")).is_err(), "names are unique");
        p.rename_sequence(b, "Interview").unwrap();
        assert_eq!(p.sequence_name(b), Some("Interview"));
        p.switch_sequence(b).unwrap();
        assert_eq!(p.active_sequence(), b);
        assert!(p.lane(Lane::V1).is_empty(), "the new sequence is empty");
        assert!(p.place(Lane::V1, 10, clip(0, 0, 20, 0)));
        p.switch_sequence(0).unwrap();
        assert_eq!(p.lane(Lane::V1).len(), 1);
        assert_eq!(p.lane(Lane::V1)[0].out_frame, 100);
        let copy = p.duplicate_sequence(b).unwrap();
        assert_eq!(p.sequence_name(copy), Some("Interview copy"));
        assert_eq!(p.sequence_frames(copy), 30);
        // The undo stack holds new, rename, place (in B), duplicate.
        assert!(p.undo());
        assert_eq!(p.sequence_count(), 2, "the copy is gone");
        assert!(p.undo());
        assert_eq!(p.sequence_frames(b), 0, "the placement in B is undone, and the sequence list with it");
        assert!(p.undo());
        assert_eq!(p.sequence_name(b), Some("B"));
        assert!(p.undo());
        assert_eq!(p.sequence_count(), 1);
        assert!(p.redo());
        assert_eq!(p.sequence_count(), 2);
        let after = p.delete_sequence(b).unwrap();
        assert_eq!(after, 0);
        assert!(p.delete_sequence(0).is_err(), "the last sequence stays");
        assert!(p.undo());
        assert_eq!(p.sequence_count(), 2);
    }

    #[test]
    fn nest_then_break_apart_is_the_identity() {
        let mut p = base();
        assert!(p.place(Lane::V1, 0, clip(0, 0, 100, 0)));
        let before = strip(&p);
        let depth = p.history_len();
        let n = p.nest(&[(Lane::V1, 0), (Lane::A1, 0)], Some("Scene")).unwrap();
        assert_eq!(p.history_len(), depth + 1, "one undo step");
        assert_eq!(n.clips.len(), 2);
        assert!(p.lane(Lane::V1)[0].source >= SEQ_BASE);
        assert_eq!(p.lane(Lane::V1)[0].link, p.lane(Lane::A1)[0].link);
        assert_eq!(p.sequence_frames(n.sequence), 100);
        let depth = p.history_len();
        let made = p.break_apart(&[(Lane::V1, 0)]).unwrap();
        assert_eq!(p.history_len(), depth + 1, "one undo step");
        assert_eq!(made.len(), 2);
        assert_eq!(strip(&p), before);
        // And each step undoes.
        assert!(p.undo());
        assert!(p.lane(Lane::V1)[0].source >= SEQ_BASE);
        assert!(p.undo());
        assert_eq!(strip(&p), before);
        assert_eq!(p.sequence_count(), 1);
    }

    #[test]
    fn break_apart_drops_the_default_named_sequence_nest_made_in_the_same_undo_step() {
        let mut p = base();
        assert!(p.place(Lane::V1, 0, clip(0, 0, 100, 0)));
        let n = p.nest(&[(Lane::V1, 0), (Lane::A1, 0)], None).unwrap();
        assert!(p.sequence_name(n.sequence).is_some_and(|n| n.ends_with(" (nested)")));
        assert_eq!(p.sequence_count(), 2);
        let depth = p.history_len();
        p.break_apart(&[(Lane::V1, 0)]).unwrap();
        assert_eq!(p.history_len(), depth + 1, "one undo step");
        assert_eq!(p.sequence_count(), 1, "no orphan tab is left");
        assert!(p.undo());
        assert_eq!(p.sequence_count(), 2, "undo brings the sequence back with the compound clip");
    }

    #[test]
    fn a_user_sequence_named_like_a_nest_survives_break_apart_and_nest_undo() {
        let mut p = base();
        assert!(p.place(Lane::V1, 0, clip(0, 0, 100, 0)));
        // The user's own sequence, named the way Nest names one.
        let user = p.nest(&[(Lane::V1, 0), (Lane::A1, 0)], Some("x (nested)")).unwrap().sequence;
        assert_eq!(p.sequence_count(), 2);
        p.break_apart(&[(Lane::V1, 0)]).unwrap();
        assert_eq!(p.sequence_count(), 2, "a sequence the user named is never dropped by name");
        assert_eq!(p.sequence_name(user), Some("x (nested)"));
        // A default Nest, then undo: undo takes exactly the sequence that Nest made.
        let depth = p.history_len();
        let made = p.nest(&[(Lane::V1, 0), (Lane::A1, 0)], None).unwrap().sequence;
        assert_eq!(p.sequence_count(), 3);
        assert!(p.undo());
        assert_eq!(p.history_len(), depth);
        assert_eq!(p.sequence_count(), 2);
        assert_eq!(p.sequence_name(user), Some("x (nested)"));
        assert!(p.sequence_name(made).is_none(), "the default-named one is gone with its undo step");
    }

    #[test]
    fn break_apart_keeps_a_sequence_the_user_named() {
        let mut p = base();
        assert!(p.place(Lane::V1, 0, clip(0, 0, 100, 0)));
        p.nest(&[(Lane::V1, 0), (Lane::A1, 0)], Some("Scene")).unwrap();
        p.break_apart(&[(Lane::V1, 0)]).unwrap();
        assert_eq!(p.sequence_count(), 2, "a named sequence is the user's work");
    }

    #[test]
    fn nest_refuses_a_clip_in_the_way() {
        let mut p = base();
        p.lanes[0].clips = vec![clip(0, 0, 10, 0), clip(1, 0, 10, 10), clip(0, 10, 20, 20)];
        let err = p.nest(&[(Lane::V1, 0), (Lane::V1, 2)], None).unwrap_err().to_string();
        assert!(err.contains("between the selected"), "{err}");
        assert_eq!(p.sequence_count(), 1, "a refusal changes nothing");
        assert_eq!(p.lane(Lane::V1).len(), 3);
    }

    #[test]
    fn cycles_are_refused() {
        let mut p = base();
        let b = p.new_sequence(Some("B")).unwrap();
        // A plays B.
        p.place(Lane::V1, 0, clip(0, 0, 10, 0));
        p.switch_sequence(b).unwrap();
        p.place(Lane::V1, 0, clip(0, 0, 10, 0));
        p.switch_sequence(0).unwrap();
        p.place_sequence(b, Lane::V1, 20).unwrap();
        // B may not play A (directly or not), nor itself.
        p.switch_sequence(b).unwrap();
        let err = p.place_sequence(0, Lane::V1, 40).unwrap_err().to_string();
        assert!(err.contains("inside itself"), "{err}");
        let own = Clip::source_range(sequence_source(b), 0, 5);
        assert!(!p.place(Lane::V1, 40, own), "a sequence cannot play itself");
        // A copy pasted from the clipboard is refused the same way.
        p.switch_sequence(0).unwrap();
        let comp = p.lane(Lane::V1).iter().find(|c| c.source >= SEQ_BASE).unwrap().clone();
        p.switch_sequence(b).unwrap();
        assert!(!p.paste(50, comp));
        // A file that loops is refused by the check the loader uses.
        let mut q = p.raw_snapshot();
        q.lanes[0].clips.push(clip(0, 0, 1, 0));
        q.lanes[0].clips.last_mut().unwrap().source = sequence_source(0);
        assert!(q.check_sequences().is_err());
        // A sequence a compound clip plays cannot be deleted.
        p.switch_sequence(0).unwrap();
        let err = p.delete_sequence(b).unwrap_err().to_string();
        assert!(err.contains("played by compound clips"), "{err}");
    }

    #[test]
    fn a_compound_expands_to_the_clips_it_stands_for() {
        let mut p = base();
        p.lanes[0].clips = vec![clip(0, 10, 40, 5), clip(1, 0, 20, 40)];
        p.lanes[1].clips = vec![clip(0, 10, 40, 5)];
        let flat = strip(&p);
        p.nest(&[(Lane::V1, 0), (Lane::V1, 1), (Lane::A1, 0)], None).unwrap();
        let view = p.render_view();
        assert_eq!(strip(&view), flat, "the nested render is the flat one");
        assert_eq!(view.timeline_frames(), p.timeline_frames());
    }

    #[test]
    fn a_trimmed_compound_shows_the_window() {
        let mut p = base();
        p.lanes[0].clips = vec![clip(0, 0, 60, 0)];
        p.lanes[1].clips = vec![clip(0, 0, 60, 0)];
        p.nest(&[(Lane::V1, 0), (Lane::A1, 0)], None).unwrap();
        // Trim the compound to inner frames 10..40, and move it.
        for kind in [LaneKind::Video, LaneKind::Audio] {
            let lane = Lane::new(kind, 0);
            let at = p.index(lane).unwrap();
            let c = &mut p.lanes[at].clips[0];
            c.in_frame = 10;
            c.out_frame = 40;
            c.start = 100;
        }
        let view = p.render_view();
        assert_eq!(view.lane(Lane::V1).len(), 1);
        let v = &view.lane(Lane::V1)[0];
        assert_eq!((v.start, v.in_frame, v.out_frame, v.source), (100, 10, 40, 0));
        let a = &view.lane(Lane::A1)[0];
        assert_eq!((a.start, a.in_frame, a.out_frame, a.source), (100, 10, 40, 0));
    }

    #[test]
    fn inner_layers_and_sound_come_through_the_nest() {
        let mut p = base();
        p.add_lane(LaneKind::Video);
        p.add_lane(LaneKind::Audio);
        p.lanes[0].clips = vec![clip(0, 0, 50, 0)];
        let v2 = p.index(Lane::new(LaneKind::Video, 1)).unwrap();
        p.lanes[v2].clips = vec![clip(1, 0, 50, 10)];
        let a1 = p.index(Lane::A1).unwrap();
        p.lanes[a1].clips = vec![clip(0, 0, 50, 0)];
        let a2 = p.index(Lane::new(LaneKind::Audio, 1)).unwrap();
        p.lanes[a2].clips = vec![clip(1, 0, 30, 5)];
        let picks = vec![
            (Lane::V1, 0),
            (Lane::new(LaneKind::Video, 1), 0),
            (Lane::A1, 0),
            (Lane::new(LaneKind::Audio, 1), 0),
        ];
        p.nest(&picks, None).unwrap();
        let view = p.render_view();
        assert!(view.lane_count(LaneKind::Video) >= 2, "the inner layer keeps its own lane");
        assert!(view.lane_count(LaneKind::Audio) >= 2, "both sounds are in the mix");
        assert_eq!(view.lane(Lane::new(LaneKind::Video, 1))[0].start, 10);
        assert_eq!(view.lane(Lane::new(LaneKind::Audio, 1))[0].start, 5);
        assert_eq!(view.audio_lanes().len(), 2);
        // The inner mix and mute are honoured.
        let mut q = p.raw_snapshot();
        q.switch_sequence(1).unwrap();
        let second = q.index(Lane::new(LaneKind::Audio, 1)).unwrap();
        q.lanes[second].muted = true;
        q.switch_sequence(0).unwrap();
        assert_eq!(q.render_view().lane(Lane::new(LaneKind::Audio, 1)).len(), 0, "a muted inner lane is silent");
    }

    #[test]
    fn nested_compounds_expand_through_each_other() {
        let mut p = base();
        p.lanes[0].clips = vec![clip(0, 0, 50, 0)];
        p.lanes[1].clips = vec![clip(0, 0, 50, 0)];
        let first = p.nest(&[(Lane::V1, 0), (Lane::A1, 0)], Some("Inner")).unwrap();
        let second = p.nest(&[(Lane::V1, 0), (Lane::A1, 0)], Some("Outer")).unwrap();
        assert_ne!(first.sequence, second.sequence);
        let view = p.render_view();
        assert_eq!(strip(&view)[0].1, vec![(0, 0, 50, 0)]);
        assert_eq!(strip(&view)[1].1, vec![(0, 0, 50, 0)]);
        assert!(p.reaches(second.sequence, first.sequence));
        assert!(!p.reaches(first.sequence, second.sequence));
    }

    #[test]
    fn export_of_a_named_sequence_renders_that_one() {
        let mut p = base();
        let b = p.new_sequence(Some("B")).unwrap();
        p.switch_sequence(b).unwrap();
        p.place(Lane::V1, 0, clip(1, 0, 25, 0));
        p.switch_sequence(0).unwrap();
        let snap = p.render_snapshot(Some(b)).unwrap();
        assert_eq!(snap.lane(Lane::V1)[0].source, 1);
        assert_eq!(snap.timeline_frames(), 25);
        assert_eq!(p.active_sequence(), 0, "the project itself did not move");
        assert!(p.render_snapshot(Some(99)).is_err());
    }
}
