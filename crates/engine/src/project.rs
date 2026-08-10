//! Edit list: two lanes of *placed* source ranges. Pure data, no I/O.
//!
//! A [`Project`] holds a video lane and an audio lane. Each lane is a list of
//! [`Clip`]s, and a clip says three things: which half-open `[in, out)` range of
//! *source* frames it plays, which file it plays it from, and -- the whole model
//! -- where on the timeline it sits ([`Clip::start`]). Clips are placed at an
//! offset rather than queued end to end, so:
//!
//! * a **gap** is not an object, it is the absence of a placement. Nothing has
//!   to be inserted, kept in sync or garbage-collected for a hole to exist, and
//!   a hole cannot disagree with its neighbours about how long it is;
//! * **moving and trimming are arithmetic** on `start`/`in_frame`, never a
//!   splice of the list;
//! * the lanes are independent: deleting audio under a picture is one lane's
//!   business, and the picture does not shift.
//!
//! The price is one invariant this module enforces at every mutation and every
//! constructor: within a lane, clips are **sorted by `start` and never overlap**
//! ([`sorted_disjoint`]). Everything else -- mapping, spans, segments -- is a
//! walk over that order.
//!
//! Two frame spaces meet here and are never mixed: *source* frames index the
//! decoded file, *timeline* frames index what the viewer sees. Both are 0-based
//! (sample ids in the demuxer are 1-based; that conversion stays in `decode`).
//!
//! Grouping is [`Clip::link`]: clips carrying the same id were split from the
//! same take and move together. [`Project::split`] cuts every lane at a timeline
//! frame and hands the two sides two fresh ids; [`Project::regroup`] is its
//! inverse and rejoins them.
//!
//! Editing is metadata only. [`Project::split`] changes no timeline->source
//! mapping, so a running decoder stays correct across it; everything else does
//! and the caller must reseek. Every successful edit snapshots both lanes, so
//! [`Project::undo`] is an exact restore.
//!
//! A clip names its file by *index* into [`Project::sources`], which is
//! append-only: an index handed out once stays valid forever, so a clip on the
//! clipboard or inside an undo snapshot can never dangle. An index -- not an
//! `Arc<Path>` -- because [`Clip`] is `Copy`, which is what makes copy/paste a
//! plain assignment.

use std::path::{Path, PathBuf};

/// A half-open `[in_frame, out_frame)` range of frames of source
/// [`source`](Clip::source), placed at timeline frame [`start`](Clip::start).
/// Never empty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Clip {
    /// Timeline frame the clip's first frame is shown at. Meaningless on a
    /// clipboard copy -- [`Project::paste`] and [`Project::place`] overwrite it
    /// with where the caller asked for.
    pub start: u32,
    pub in_frame: u32,
    pub out_frame: u32,
    /// Index into [`Project::sources`].
    pub source: usize,
    /// Group id: clips sharing one were split from the same take. `None` for a
    /// clip that belongs to no group.
    pub link: Option<u32>,
}

impl Clip {
    /// Frame count; `>= 1` by the never-empty invariant.
    pub fn len(&self) -> u32 {
        self.out_frame - self.in_frame
    }

    /// One past the last timeline frame it covers.
    pub fn end(&self) -> u32 {
        self.start + self.len()
    }
}

/// Which lane an operation acts on. The lanes are peers: no operation reads one
/// to decide what to do to the other, except the grouped ones that say so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    Video,
    Audio,
}

impl Lane {
    pub const ALL: [Lane; 2] = [Lane::Video, Lane::Audio];

    fn index(self) -> usize {
        self as usize
    }
}

/// What a lane holds over one stretch of the timeline: either a placed clip or
/// a gap. Returned already trimmed to the position it was asked about, so a
/// caller can hand `len` straight to a decoder (or to a black-frame generator).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    /// Timeline frame the span starts at.
    pub start: u32,
    /// Frames it covers; `>= 1`.
    pub len: u32,
    /// `(source index, first source frame)`, or `None` for a gap -- which the
    /// engine renders as black frames and as silence.
    pub from: Option<(usize, u32)>,
}

impl Span {
    /// One past the last timeline frame it covers.
    pub fn end(&self) -> u32 {
        self.start + self.len
    }
}

/// The edit list plus its undo history.
#[derive(Clone, Debug)]
pub struct Project {
    /// Append-only: never popped, never reordered. See the module docs.
    sources: Vec<PathBuf>,
    /// Indexed by [`Lane::index`]. Each lane is sorted by `start` and disjoint.
    lanes: [Vec<Clip>; 2],
    /// Snapshots pushed *before* each successful edit; `undo` pops one.
    history: Vec<[Vec<Clip>; 2]>,
    /// Never rolled back by an undo: an id retired by an undone split must not
    /// come back and group two clips that were never together.
    next_link: u32,
}

impl Project {
    /// One clip per lane covering the whole of `path`, the two grouped -- the
    /// state of a freshly opened video, where the timeline is the source.
    /// `frame_count` of 0 would break the never-empty invariant, so it is
    /// clamped to one frame.
    pub fn single(path: impl AsRef<Path>, frame_count: u32) -> Self {
        let clip = Clip {
            start: 0,
            in_frame: 0,
            out_frame: frame_count.max(1),
            source: 0,
            link: Some(0),
        };
        Self {
            sources: vec![canonical(path.as_ref())],
            lanes: [vec![clip], vec![clip]],
            history: Vec::new(),
            next_link: 1,
        }
    }

    /// A project rebuilt from a saved edit list -- the load half of
    /// [`crate::edith`]. History is *not* saved, so [`Project::undo`] is
    /// `false` until the first edit of the new session. This is the one door
    /// untrusted parts come in through, so every invariant every other
    /// constructor keeps is checked here, by name and in release: both lanes
    /// empty, an empty clip, a clip naming a source that is not there, a clip
    /// whose end overflows, a lane that is unsorted or self-overlapping, and
    /// the grouping rules of [`Clip::link`] below.
    pub fn from_parts(
        sources: Vec<PathBuf>,
        video: Vec<Clip>,
        audio: Vec<Clip>,
    ) -> crate::Result<Self> {
        if video.is_empty() && audio.is_empty() {
            return Err("both lanes are empty: that is not a project".into());
        }
        let lanes = [video, audio];
        for (lane, name) in lanes.iter().zip(["video", "audio"]) {
            for c in lane {
                if c.out_frame <= c.in_frame {
                    return Err(format!(
                        "{name} clip at {} plays the empty range [{}, {})",
                        c.start, c.in_frame, c.out_frame
                    )
                    .into());
                }
                if c.source >= sources.len() {
                    return Err(format!(
                        "{name} clip at {} names source {} of {}",
                        c.start,
                        c.source,
                        sources.len()
                    )
                    .into());
                }
                if c.start.checked_add(c.len()).is_none() {
                    return Err(format!(
                        "{name} clip at {} runs past the last frame there is",
                        c.start
                    )
                    .into());
                }
            }
            if !sorted_disjoint(lane) {
                return Err(format!("the {name} lane is out of order or overlaps itself").into());
            }
        }
        links_are_consistent(&lanes)?;
        let next_link = lanes
            .iter()
            .flatten()
            .filter_map(|c| c.link)
            .max()
            // Saturating so a crafted file cannot make the counter wrap: at the
            // ceiling ids stop being fresh, which loses grouping, not memory.
            .map_or(0, |m| m.saturating_add(1));
        Ok(Self {
            sources: sources.iter().map(|s| canonical(s)).collect(),
            lanes,
            history: Vec::new(),
            next_link,
        })
    }

    /// The video lane -- what an export renders and what a single-lane caller
    /// still means by "the clips".
    pub fn clips(&self) -> &[Clip] {
        self.lane(Lane::Video)
    }

    pub fn lane(&self, lane: Lane) -> &[Clip] {
        &self.lanes[lane.index()]
    }

    /// The files the clips index into, in import order; index 0 is the file the
    /// project was opened with.
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }

    /// Index for `path`, appending it if it is new. Deduped by
    /// `fs::canonicalize`, so the same file reached by two paths imports once.
    /// The lanes are untouched -- see [`Project::append_clip`] -- so this pushes
    /// no history: a source entry alone changes nothing playable.
    pub fn import(&mut self, path: impl AsRef<Path>) -> usize {
        let path = canonical(path.as_ref());
        match self.sources.iter().position(|s| *s == path) {
            Some(idx) => idx,
            None => {
                self.sources.push(path);
                self.sources.len() - 1
            }
        }
    }

    /// Append a whole-file clip of `source` to the end of *both* lanes, grouped
    /// -- an import lands as one take. One history snapshot, so an import is one
    /// undo step, and undoing it leaves the (harmless) source entry behind,
    /// because indexes are forever. Refused for an unknown source index.
    pub fn append_clip(&mut self, source: usize, frame_count: u32) -> bool {
        if source >= self.sources.len() {
            return false;
        }
        self.snapshot();
        let start = self.timeline_frames();
        let clip = Clip {
            start,
            in_frame: 0,
            out_frame: frame_count.max(1),
            source,
            link: Some(self.new_link()),
        };
        for lane in &mut self.lanes {
            lane.push(clip);
        }
        true
    }

    /// The sources a clip actually names, with the clips reindexed onto them
    /// -- what a save writes. Indexes are forever *inside* a session (see
    /// [`Project::append_clip`]), so an undone import leaves an orphan source
    /// entry behind; writing that orphan to a project file would let a file
    /// nothing plays refuse a future load. New indexes are assigned in order of
    /// first use -- video lane first, then audio -- so the same project always
    /// emits the same bytes.
    pub fn without_orphan_sources(&self) -> (Vec<PathBuf>, Vec<Clip>, Vec<Clip>) {
        let mut moved = vec![None; self.sources.len()];
        let mut sources = Vec::new();
        let mut lanes = self.lanes.clone();
        for c in lanes.iter_mut().flatten() {
            let old = c.source;
            c.source = match moved[old] {
                Some(new) => new,
                None => {
                    sources.push(self.sources[old].clone());
                    moved[old] = Some(sources.len() - 1);
                    sources.len() - 1
                }
            };
        }
        let [video, audio] = lanes;
        (sources, video, audio)
    }

    /// Length of the timeline in frames: where the *last* lane runs out. A lane
    /// that ends early is a trailing gap in that lane, not a shorter timeline.
    pub fn timeline_frames(&self) -> u32 {
        self.lanes
            .iter()
            .filter_map(|lane| lane.last().map(Clip::end))
            .max()
            .unwrap_or(0)
    }

    /// `(timeline_start, len)` per video clip, in order -- what a UI lane needs
    /// to lay clips out. Gaps show up as the holes between consecutive entries.
    pub fn clip_spans(&self) -> Vec<(u32, u32)> {
        self.lane_spans(Lane::Video)
    }

    pub fn lane_spans(&self, lane: Lane) -> Vec<(u32, u32)> {
        self.lane(lane).iter().map(|c| (c.start, c.len())).collect()
    }

    /// Timeline frame -> `(clip index, source frame)` in `lane`. `None` in a gap
    /// and past the end -- [`Project::span_at`] is the version that tells those
    /// two apart.
    pub fn map(&self, lane: Lane, timeline_frame: u32) -> Option<(usize, u32)> {
        let clips = self.lane(lane);
        let idx = at(clips, timeline_frame)?;
        Some((
            idx,
            clips[idx].in_frame + (timeline_frame - clips[idx].start),
        ))
    }

    /// [`map`](Project::map) on the video lane -- the mapping a decoder follows.
    pub fn map_timeline(&self, timeline_frame: u32) -> Option<(usize, u32)> {
        self.map(Lane::Video, timeline_frame)
    }

    /// What `lane` holds from `timeline_frame` on: the rest of the clip covering
    /// it, or the gap running to the next clip (or to the end of the timeline,
    /// which is where a lane shorter than its neighbour keeps showing black).
    /// `None` at or past the end of the timeline, which is the only "nothing
    /// left" there is.
    pub fn span_at(&self, lane: Lane, timeline_frame: u32) -> Option<Span> {
        let total = self.timeline_frames();
        if timeline_frame >= total {
            return None;
        }
        let clips = self.lane(lane);
        Some(match at(clips, timeline_frame) {
            Some(idx) => Span {
                start: timeline_frame,
                len: clips[idx].end() - timeline_frame,
                from: Some((
                    clips[idx].source,
                    clips[idx].in_frame + (timeline_frame - clips[idx].start),
                )),
            },
            None => Span {
                start: timeline_frame,
                // The next placement, or the end of the timeline: a gap is
                // bounded by what comes after it, never by its own bookkeeping.
                len: clips
                    .iter()
                    .find(|c| c.start > timeline_frame)
                    .map_or(total, |c| c.start)
                    - timeline_frame,
                from: None,
            },
        })
    }

    /// Every span of `lane` from `timeline_frame` to the end of the timeline,
    /// gaps included -- the whole play list, which is what an export walks.
    pub fn spans_from(&self, lane: Lane, timeline_frame: u32) -> Vec<Span> {
        let mut out = Vec::new();
        let mut t = timeline_frame;
        while let Some(span) = self.span_at(lane, t) {
            t = span.end();
            out.push(span);
        }
        out
    }

    /// Split every lane at `timeline_frame`, so that frame becomes the first
    /// frame of a new clip, and hand the two sides two fresh group ids. Refused
    /// (`false`, no change) when no lane has a clip to split there -- at a
    /// placement's own start, in a gap, and past the end -- all of which would
    /// produce an empty clip or nothing at all.
    ///
    /// Metadata only: no mapping changes, in any lane.
    pub fn split(&mut self, timeline_frame: u32) -> bool {
        if !self
            .lanes
            .iter()
            .any(|lane| splittable(lane, timeline_frame).is_some())
        {
            return false;
        }
        self.snapshot();
        let (left, right) = (self.new_link(), self.new_link());
        for lane in &mut self.lanes {
            let Some(idx) = splittable(lane, timeline_frame) else {
                continue;
            };
            let mut tail = lane[idx];
            tail.in_frame += timeline_frame - tail.start;
            tail.start = timeline_frame;
            tail.link = Some(right);
            lane[idx].out_frame = tail.in_frame;
            lane[idx].link = Some(left);
            lane.insert(idx + 1, tail);
        }
        true
    }

    /// The inverse of [`split`](Project::split): rejoin the placements that meet
    /// at `timeline_frame` in every lane and put the result back in one group.
    /// Only what a split could have produced is rejoined -- the two sides must
    /// touch on the timeline *and* be consecutive frames of the same source --
    /// so the clip list comes back exactly as it was and traversal with it.
    /// `false` when no lane has such a pair.
    pub fn regroup(&mut self, timeline_frame: u32) -> bool {
        if !self
            .lanes
            .iter()
            .any(|lane| joinable(lane, timeline_frame).is_some())
        {
            return false;
        }
        self.snapshot();
        let link = self.new_link();
        for lane in &mut self.lanes {
            let Some(idx) = joinable(lane, timeline_frame) else {
                continue;
            };
            lane[idx].out_frame = lane[idx + 1].out_frame;
            lane[idx].link = Some(link);
            lane.remove(idx + 1);
        }
        true
    }

    /// Place `clip` in one lane at `timeline_frame`, overwriting whatever it
    /// lands on and leaving every other clip exactly where it is -- the
    /// per-lane paste. Anything already there is trimmed away (and split in two
    /// if `clip` lands inside it), which is what keeps the lane sorted and
    /// disjoint; nothing ripples, so a gap before the placement simply stays a
    /// gap. Refused only for an empty `clip`.
    ///
    /// The placement belongs to no group: a clipboard clip carries the link id
    /// of the take it was copied from, and placing it twice -- or placing a copy
    /// back over the clip it came from -- would put that id on two clips of one
    /// lane, which is what a link may never mean (see [`links_are_consistent`]).
    /// `None` rather than a fresh id because a one-lane placement has nothing on
    /// the other lane to be grouped *with*; [`Project::regroup`] is how clips
    /// become a group again.
    pub fn place(&mut self, lane: Lane, timeline_frame: u32, clip: Clip) -> bool {
        if clip.out_frame <= clip.in_frame {
            return false;
        }
        self.snapshot();
        let clip = Clip {
            start: timeline_frame,
            link: None,
            ..clip
        };
        let lane = &mut self.lanes[lane.index()];
        clear(lane, clip.start, clip.end());
        let idx = lane.partition_point(|c| c.start < clip.start);
        lane.insert(idx, clip);
        debug_assert!(sorted_disjoint(lane));
        true
    }

    /// Insert `clip` into *both* lanes at `timeline_frame` as one new group,
    /// pushing everything from there on later by its length -- the grouped,
    /// rippling paste a clipboard does. Mid-clip the clip it lands in is split
    /// around it; at or past the end of the timeline it is appended, because a
    /// paste means "put it here", not "put it here and leave black in front".
    /// Use [`place`](Project::place) to paste into one lane, or to make a gap.
    ///
    /// Exactly one history snapshot, so one [`Project::undo`] takes it back.
    /// Changes the timeline->source mapping: the caller must reseek. Refused
    /// only for an empty `clip`, which the never-empty invariant forbids but the
    /// public fields still allow a caller to build.
    pub fn paste(&mut self, timeline_frame: u32, clip: Clip) -> bool {
        if clip.out_frame <= clip.in_frame {
            return false;
        }
        self.snapshot();
        let at = timeline_frame.min(self.timeline_frames());
        let clip = Clip {
            start: at,
            link: Some(self.new_link()),
            ..clip
        };
        for lane in &mut self.lanes {
            open_room(lane, at, clip.len());
            let idx = lane.partition_point(|c| c.start < at);
            lane.insert(idx, clip);
            debug_assert!(sorted_disjoint(lane));
        }
        true
    }

    /// Lift the clip at `idx` out of `lane`, leaving a gap: black frames or
    /// silence, and nothing else moves. Refused for an out-of-range index and
    /// for the lift that would leave *both* lanes empty -- an empty timeline is
    /// the front-end's state, not a project's (see the never-empty invariant).
    pub fn lift(&mut self, lane: Lane, idx: usize) -> bool {
        let placed: usize = self.lanes.iter().map(Vec::len).sum();
        if idx >= self.lane(lane).len() || placed == 1 {
            return false;
        }
        self.snapshot();
        self.lanes[lane.index()].remove(idx);
        true
    }

    /// Cut the timeline frames `[at, at + len)` out of *every* lane and close
    /// the hole: everything after slides back by `len`. The rippling delete --
    /// [`lift`](Project::lift) is the one that leaves a gap. Refused for an
    /// empty range and when it would leave both lanes empty.
    pub fn ripple_delete(&mut self, at: u32, len: u32) -> bool {
        if len == 0 {
            return false;
        }
        let survivors: usize = self
            .lanes
            .iter()
            .map(|lane| {
                lane.iter()
                    .filter(|c| c.start < at || c.end() > at + len)
                    .count()
            })
            .sum();
        if survivors == 0 {
            return false;
        }
        self.snapshot();
        for lane in &mut self.lanes {
            clear(lane, at, at + len);
            for c in lane.iter_mut().filter(|c| c.start >= at) {
                c.start -= len;
            }
            debug_assert!(sorted_disjoint(lane));
        }
        true
    }

    /// Remove the video clip at `idx` and everything under it, closing the gap
    /// -- the whole-group delete a single-lane front-end means. `false` for a
    /// bad index. Changes the mapping: the caller must reseek.
    pub fn delete(&mut self, idx: usize) -> bool {
        let Some(clip) = self.clips().get(idx).copied() else {
            return false;
        };
        self.ripple_delete(clip.start, clip.len())
    }

    /// Restore both lanes from before the last successful edit. `false` when
    /// there is nothing left to undo.
    pub fn undo(&mut self) -> bool {
        match self.history.pop() {
            Some(prev) => {
                self.lanes = prev;
                true
            }
            None => false,
        }
    }

    /// Half-open `(source, start, end)` segments in *source* seconds, from
    /// `timeline_frame` to the end of the timeline -- the play list for the
    /// audio worker, read off the **audio** lane. A `None` source is a gap: the
    /// worker synthesises that many seconds of silence, which is what keeps the
    /// audio master clock counting across a hole instead of stalling on it.
    /// The first entry is partial when the position is mid-clip. Empty when the
    /// position is past the end or `fps` is not usable.
    pub fn segments_from(&self, timeline_frame: u32, fps: f64) -> Vec<(Option<usize>, f64, f64)> {
        if !(fps.is_finite() && fps > 0.0) {
            return Vec::new();
        }
        self.spans_from(Lane::Audio, timeline_frame)
            .iter()
            .map(|span| match span.from {
                // A clip's window is in *source* seconds: where in the file it
                // reads from, which a delete before it never shifts.
                Some((source, in_frame)) => (
                    Some(source),
                    f64::from(in_frame) / fps,
                    f64::from(in_frame + span.len) / fps,
                ),
                // A gap has no file, so all it can say is how long it is.
                None => (None, 0.0, f64::from(span.len) / fps),
            })
            .collect()
    }

    /// Pushes the undo snapshot. Every mutating method calls this once, *after*
    /// it has decided it will succeed -- a refusal must not cost an undo step.
    fn snapshot(&mut self) {
        self.history.push(self.lanes.clone());
    }

    fn new_link(&mut self) -> u32 {
        let id = self.next_link;
        self.next_link = self.next_link.saturating_add(1);
        id
    }
}

/// Index of the clip covering `frame`, or `None` for a gap or past the end.
/// Binary search: the sorted-disjoint invariant is what makes it legal.
fn at(clips: &[Clip], frame: u32) -> Option<usize> {
    let idx = clips.partition_point(|c| c.start <= frame).checked_sub(1)?;
    (frame < clips[idx].end()).then_some(idx)
}

/// Index of the clip `frame` falls *strictly inside*, i.e. the one a split
/// there would cut in two. A placement's own first frame is not inside it.
fn splittable(clips: &[Clip], frame: u32) -> Option<usize> {
    let idx = at(clips, frame)?;
    (frame > clips[idx].start).then_some(idx)
}

/// Index of the first of the two clips a [`Project::regroup`] at `frame` would
/// rejoin: they must touch there, and be consecutive frames of one source.
fn joinable(clips: &[Clip], frame: u32) -> Option<usize> {
    let idx = clips.iter().position(|c| c.end() == frame)?;
    let next = clips.get(idx + 1)?;
    (next.start == frame
        && next.source == clips[idx].source
        && next.in_frame == clips[idx].out_frame)
        .then_some(idx)
}

/// The lane invariant: sorted by `start`, no two placements overlapping, no
/// empty placement. Checked at every constructor and asserted after every
/// mutation -- the offset model is only tractable while it holds.
fn sorted_disjoint(clips: &[Clip]) -> bool {
    clips.iter().all(|c| c.out_frame > c.in_frame)
        && clips.windows(2).all(|w| w[0].end() <= w[1].start)
}

/// The grouping invariant, checked in release at [`Project::from_parts`]: a link
/// id names **at most one** clip per lane, and when it names one in each, the two
/// cover the same timeline span -- that is all "these move together" can mean.
///
/// A link the *other* lane does not carry is legal and is not an error: lifting
/// one half of a group ([`Project::lift`]) leaves exactly that, and a save of
/// that timeline has to load again.
fn links_are_consistent(lanes: &[Vec<Clip>; 2]) -> crate::Result<()> {
    let find = |lane: &Vec<Clip>, id: u32| lane.iter().find(|c| c.link == Some(id)).copied();
    for (lane, name) in lanes.iter().zip(["video", "audio"]) {
        for (i, c) in lane.iter().enumerate() {
            let Some(id) = c.link else { continue };
            if lane[..i].iter().any(|prev| prev.link == Some(id)) {
                return Err(format!("link {id} names two clips in the {name} lane").into());
            }
        }
    }
    for a in lanes[0].iter().filter(|c| c.link.is_some()) {
        let id = a.link.expect("filtered");
        let Some(b) = find(&lanes[1], id) else {
            continue;
        };
        if (a.start, a.end()) != (b.start, b.end()) {
            return Err(format!(
                "link {id} covers [{}, {}) in video and [{}, {}) in audio",
                a.start,
                a.end(),
                b.start,
                b.end()
            )
            .into());
        }
    }
    Ok(())
}

/// Removes the timeline frames `[start, end)` from `clips`: placements inside
/// it go, one straddling it is split in two, one overlapping an edge is
/// trimmed. Source frames follow the trim, so what is left plays what it always
/// played. Leaves the lane sorted and disjoint.
///
/// A piece the hole cut is no longer the take its group id names -- and both
/// halves of a straddled placement carrying that id would be the same id twice
/// in one lane -- so what is cut loses its link (see [`links_are_consistent`]).
fn clear(clips: &mut Vec<Clip>, start: u32, end: u32) {
    let mut out = Vec::with_capacity(clips.len() + 1);
    for c in clips.drain(..) {
        // Disjoint from the hole: untouched.
        if c.end() <= start || c.start >= end {
            out.push(c);
            continue;
        }
        // The head that survives in front of the hole.
        if c.start < start {
            out.push(Clip {
                out_frame: c.in_frame + (start - c.start),
                link: None,
                ..c
            });
        }
        // ...and the tail behind it, which keeps reading where it would have.
        if c.end() > end {
            out.push(Clip {
                start: end,
                in_frame: c.in_frame + (end - c.start),
                link: None,
                ..c
            });
        }
    }
    *clips = out;
}

/// Slides everything from `at` on later by `len`, splitting a placement that
/// straddles `at` so the two halves end up on either side of the hole. The
/// insert half of a rippling paste. The two halves lose the group id for the
/// reason [`clear`] gives.
fn open_room(clips: &mut Vec<Clip>, at: u32, len: u32) {
    if let Some(idx) = splittable(clips, at) {
        let mut tail = clips[idx];
        tail.in_frame += at - tail.start;
        tail.start = at;
        tail.link = None;
        clips[idx].out_frame = tail.in_frame;
        clips[idx].link = None;
        clips.insert(idx + 1, tail);
    }
    for c in clips.iter_mut().filter(|c| c.start >= at) {
        c.start += len;
    }
}

/// Absolute, symlink-resolved when the file is reachable; the path as given
/// when it is not -- an unreadable path still deserves an index, it simply
/// dedups by spelling.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 30.0;
    /// Never opened -- `Project` is pure data, so a path that does not exist is
    /// simply a path that dedups by spelling.
    const FILE: &str = "/nonexistent/a.mp4";
    const FILE2: &str = "/nonexistent/b.mp4";

    /// A contiguous placement, group id ignored: what most assertions compare.
    fn clip(start: u32, in_frame: u32, out_frame: u32, source: usize) -> Clip {
        Clip {
            start,
            in_frame,
            out_frame,
            source,
            link: None,
        }
    }

    /// Both lanes with their group ids blanked, for comparing shape.
    fn shape(p: &Project) -> [Vec<Clip>; 2] {
        Lane::ALL.map(|l| {
            p.lane(l)
                .iter()
                .map(|c| Clip { link: None, ..*c })
                .collect()
        })
    }

    /// The *source frame* every timeline frame reads, per lane -- the traversal
    /// a player performs. Deliberately not the clip index: a split changes which
    /// clip a frame belongs to, and nothing else.
    fn traversal(p: &Project) -> Vec<[Option<u32>; 2]> {
        (0..p.timeline_frames() + 2)
            .map(|f| Lane::ALL.map(|l| p.map(l, f).map(|(_, source)| source)))
            .collect()
    }

    /// Source clips [0,3) [3,5) [5,9) on both lanes -- the ledger's off-by-one
    /// fixture. Built through `split` so the constructor path is exercised too.
    fn three() -> Project {
        let mut p = Project::single(FILE, 9);
        assert!(p.split(3));
        assert!(p.split(5));
        assert_eq!(
            shape(&p),
            [
                vec![clip(0, 0, 3, 0), clip(3, 3, 5, 0), clip(5, 5, 9, 0)],
                vec![clip(0, 0, 3, 0), clip(3, 3, 5, 0), clip(5, 5, 9, 0)],
            ]
        );
        p
    }

    #[test]
    fn single_is_the_whole_file_on_both_lanes() {
        let p = Project::single(FILE, 150);
        assert_eq!(p.clips().len(), 1);
        assert_eq!(p.lane(Lane::Audio).len(), 1);
        assert_eq!(p.timeline_frames(), 150);
        assert_eq!(p.clip_spans(), vec![(0, 150)]);
        // grouped: video and audio carry the same link
        assert_eq!(p.clips()[0].link, p.lane(Lane::Audio)[0].link);
        assert!(p.clips()[0].link.is_some());
        // degenerate mapping: timeline == source
        assert_eq!(p.map_timeline(0), Some((0, 0)));
        assert_eq!(p.map_timeline(149), Some((0, 149)));
        assert_eq!(p.map_timeline(150), None);
        // never-empty invariant survives a bogus frame count
        assert_eq!(Project::single(FILE, 0).timeline_frames(), 1);
    }

    #[test]
    fn split_cuts_every_lane_and_regroups_it_back() {
        let mut p = Project::single(FILE, 9);
        let before = traversal(&p);
        let before_shape = shape(&p);

        assert!(p.split(4));
        for l in Lane::ALL {
            assert_eq!(p.lane(l).len(), 2, "{l:?} split");
        }
        // The two sides are two groups, one per side, matching across the lanes.
        let (v, a) = (p.lane(Lane::Video), p.lane(Lane::Audio));
        assert_eq!(v[0].link, a[0].link);
        assert_eq!(v[1].link, a[1].link);
        assert_ne!(v[0].link, v[1].link, "the halves are no longer one take");
        // A split changes no mapping, in either lane.
        assert_eq!(traversal(&p), before, "a split moves nothing");

        assert!(p.regroup(4), "the inverse rejoins them");
        assert_eq!(shape(&p), before_shape, "bit-exact back to one clip");
        assert_eq!(traversal(&p), before);
        assert_eq!(
            p.lane(Lane::Video)[0].link,
            p.lane(Lane::Audio)[0].link,
            "and the rejoined clip is one group again"
        );
    }

    #[test]
    fn split_refused_at_zero_boundary_end_and_in_a_gap() {
        let mut p = three();
        let before = shape(&p);
        assert!(!p.split(0), "timeline 0 has nothing before it");
        assert!(!p.split(3), "existing boundary");
        assert!(!p.split(5), "existing boundary");
        assert!(!p.split(9), "one past the last frame");
        assert!(!p.split(1_000));
        assert_eq!(shape(&p), before);
        // one undo lands before the *second* split, so the refusals pushed nothing
        assert!(p.undo());
        assert_eq!(p.clips().len(), 2, "refused splits push no history");

        // ...and a frame that is a gap in both lanes has nothing to split.
        let mut p = three();
        assert!(p.lift(Lane::Video, 1));
        assert!(p.lift(Lane::Audio, 1));
        assert!(!p.split(4), "a gap is not a clip");
    }

    #[test]
    fn regroup_refuses_what_a_split_could_not_have_made() {
        let mut p = three();
        // [3,5) and [5,9) are source-contiguous: that pair rejoins.
        assert!(p.regroup(5));
        assert_eq!(shape(&p)[0], vec![clip(0, 0, 3, 0), clip(3, 3, 9, 0)]);
        // A boundary with a gap on one side, and one where the sources do not
        // meet, are both refused.
        let mut p = three();
        assert!(p.lift(Lane::Video, 1));
        assert!(p.lift(Lane::Audio, 1));
        assert!(!p.regroup(3), "nothing starts at 3 any more");
        let mut p = three();
        for l in Lane::ALL {
            assert!(p.place(l, 3, clip(0, 100, 102, 0)));
        }
        assert!(!p.regroup(3), "source 100 does not follow source 3");
        assert!(!p.regroup(0), "nothing ends at 0");
        assert!(!p.regroup(1_000));
    }

    #[test]
    fn map_and_spans_sweep_every_boundary() {
        let p = three();
        assert_eq!(p.timeline_frames(), 9);
        assert_eq!(p.clip_spans(), vec![(0, 3), (3, 2), (5, 4)]);
        // contiguous source here, so the clip index is what actually moves
        let expect = [
            (0, (0, 0)),
            (1, (0, 1)),
            (2, (0, 2)),
            (3, (1, 3)), // half-open: the boundary frame belongs to the NEXT clip
            (4, (1, 4)),
            (5, (2, 5)),
            (6, (2, 6)),
            (7, (2, 7)),
            (8, (2, 8)),
        ];
        for (t, want) in expect {
            assert_eq!(p.map_timeline(t), Some(want), "timeline frame {t}");
        }
        assert_eq!(p.map_timeline(9), None);
        // A span is the rest of the clip from where it was asked about.
        assert_eq!(
            p.span_at(Lane::Video, 6),
            Some(Span {
                start: 6,
                len: 3,
                from: Some((0, 6))
            })
        );
        assert_eq!(p.span_at(Lane::Video, 9), None);
    }

    /// The clip a copy would hand back: source `[100, 102)`, unrelated to
    /// anything in `three()` so it is recognisable wherever it lands.
    const PASTED: Clip = Clip {
        start: 0,
        in_frame: 100,
        out_frame: 102,
        source: 0,
        link: None,
    };

    #[test]
    fn paste_mid_clip_splits_around_it() {
        let mut p = three();
        assert!(p.paste(6, PASTED)); // inside [5,9), one frame in
        assert_eq!(p.timeline_frames(), 11);
        assert_eq!(
            shape(&p)[0],
            vec![
                clip(0, 0, 3, 0),
                clip(3, 3, 5, 0),
                clip(5, 5, 6, 0),
                clip(6, 100, 102, 0),
                clip(8, 6, 9, 0),
            ]
        );
        assert_eq!(shape(&p)[1], shape(&p)[0], "a paste lands on both lanes");
        // The pasted frames are exactly where they were asked for, and the
        // split-off remainder resumes after them.
        assert_eq!(p.map_timeline(6), Some((3, 100)));
        assert_eq!(p.map_timeline(7), Some((3, 101)));
        assert_eq!(p.map_timeline(8), Some((4, 6)));
    }

    #[test]
    fn paste_at_boundary_zero_and_end() {
        let mut p = three();
        assert!(p.paste(3, PASTED), "existing boundary: no split");
        assert_eq!(p.clip_spans(), vec![(0, 3), (3, 2), (5, 2), (7, 4)]);
        assert_eq!(p.map_timeline(3), Some((1, 100)));

        let mut p = three();
        assert!(p.paste(0, PASTED));
        assert_eq!(p.map_timeline(0), Some((0, 100)));
        assert_eq!(p.map_timeline(2), Some((1, 0)));

        // At the end and past it both append -- a paste never leaves black in
        // front of itself; `place` is the call that makes a gap.
        let mut p = three();
        assert!(p.paste(9, PASTED));
        assert_eq!(p.clip_spans(), vec![(0, 3), (3, 2), (5, 4), (9, 2)]);
        assert!(p.paste(1_000, PASTED));
        assert_eq!(p.timeline_frames(), 13);

        // An empty clip is the one thing refused, and it pushes no history.
        let mut p = three();
        assert!(!p.paste(4, clip(0, 7, 7, 0)));
        assert_eq!(p.clips().len(), 3);
    }

    #[test]
    fn paste_undoes_in_one_step() {
        let mut p = three();
        let before = shape(&p);
        assert!(p.paste(6, PASTED));
        assert!(p.undo());
        assert_eq!(shape(&p), before, "a paste is one undo step");
    }

    #[test]
    fn a_copy_outlives_the_clip_it_came_from() {
        let mut p = three();
        let copied = p.clips()[1]; // [3,5)
        assert!(p.delete(1));
        // Just a frame range: the source is still on disk either way.
        assert!(p.paste(0, copied));
        assert_eq!(p.map_timeline(0), Some((0, 3)));
        assert_eq!(p.timeline_frames(), 9);
    }

    #[test]
    fn delete_closes_the_gap_on_both_lanes() {
        let mut p = three();
        assert!(p.delete(1));
        assert_eq!(p.timeline_frames(), 7);
        assert_eq!(p.clip_spans(), vec![(0, 3), (3, 4)]);
        assert_eq!(p.lane_spans(Lane::Audio), vec![(0, 3), (3, 4)]);
        // timeline 3 used to be source 3; the gap closed so it is source 5 now
        assert_eq!(p.map_timeline(3), Some((1, 5)));
        assert_eq!(p.map_timeline(6), Some((1, 8)));
        assert_eq!(p.map_timeline(7), None);
    }

    #[test]
    fn delete_first_last_and_only() {
        let mut p = three();
        assert!(p.delete(0));
        assert_eq!(p.map_timeline(0), Some((0, 3)));
        assert_eq!(p.timeline_frames(), 6);

        let mut p = three();
        assert!(p.delete(2));
        assert_eq!(p.timeline_frames(), 5);
        assert_eq!(p.map_timeline(4), Some((1, 4)));

        assert!(!p.delete(2), "index past the end");
        assert!(p.delete(1));
        assert!(!p.delete(0), "the last remaining clip stays");
        assert_eq!(p.clips().len(), 1);
    }

    /// The offset model's point: one lane loses a clip, the other does not move.
    #[test]
    fn a_lift_leaves_a_gap_and_moves_nothing() {
        let mut p = three();
        let video_before = shape(&p)[0].clone();
        assert!(p.lift(Lane::Audio, 1));
        assert_eq!(shape(&p)[0], video_before, "the picture never moved");
        assert_eq!(p.timeline_frames(), 9, "nor did the timeline get shorter");
        assert_eq!(p.lane_spans(Lane::Audio), vec![(0, 3), (5, 4)]);
        // The hole is a gap: mapped as nothing, spanned as a gap of its length.
        assert_eq!(p.map(Lane::Audio, 4), None);
        assert_eq!(
            p.span_at(Lane::Audio, 3),
            Some(Span {
                start: 3,
                len: 2,
                from: None
            })
        );
        assert_eq!(p.map(Lane::Video, 4), Some((1, 4)), "video plays on");
        assert!(p.undo());
        assert_eq!(p.lane_spans(Lane::Audio), vec![(0, 3), (3, 2), (5, 4)]);
    }

    #[test]
    fn a_trailing_gap_is_a_gap_to_the_end_of_the_timeline() {
        let mut p = three();
        assert!(p.lift(Lane::Audio, 2));
        assert_eq!(p.timeline_frames(), 9, "video still runs to 9");
        assert_eq!(
            p.span_at(Lane::Audio, 5),
            Some(Span {
                start: 5,
                len: 4,
                from: None
            }),
            "the audio lane holds silence to the end of the picture"
        );
        assert_eq!(p.span_at(Lane::Audio, 9), None);
        // Every lane covers the timeline exactly, gaps included.
        for l in Lane::ALL {
            let spans = p.spans_from(l, 0);
            assert_eq!(spans.iter().map(|s| s.len).sum::<u32>(), 9, "{l:?}");
            assert_eq!(spans[0].start, 0);
        }
    }

    #[test]
    fn the_last_clip_of_the_last_lane_cannot_be_lifted() {
        let mut p = Project::single(FILE, 9);
        assert!(p.lift(Lane::Audio, 0), "a silent timeline is fine");
        assert!(!p.lift(Lane::Video, 0), "an empty one is not");
        assert!(!p.lift(Lane::Audio, 0), "index past the end");
        assert_eq!(p.timeline_frames(), 9);
    }

    #[test]
    fn place_overwrites_only_its_own_lane() {
        let mut p = three();
        let audio_before = shape(&p)[1].clone();
        // Straddles [3,5) and eats into [5,9): the first is trimmed, the second
        // is trimmed, nothing shifts.
        assert!(p.place(Lane::Video, 4, clip(0, 100, 102, 0)));
        assert_eq!(
            shape(&p)[0],
            vec![
                clip(0, 0, 3, 0),
                clip(3, 3, 4, 0),
                clip(4, 100, 102, 0),
                clip(6, 6, 9, 0),
            ]
        );
        assert_eq!(shape(&p)[1], audio_before, "the audio lane is untouched");
        assert_eq!(p.timeline_frames(), 9, "an overwrite is not an insert");

        // Placed inside one clip, it splits it in two.
        let mut p = Project::single(FILE, 9);
        assert!(p.place(Lane::Video, 4, clip(0, 100, 101, 0)));
        assert_eq!(
            shape(&p)[0],
            vec![clip(0, 0, 4, 0), clip(4, 100, 101, 0), clip(5, 5, 9, 0)]
        );
        // ...and placed past the end it makes a gap, which is the whole point.
        assert!(p.place(Lane::Video, 20, clip(0, 100, 102, 0)));
        assert_eq!(p.timeline_frames(), 22);
        assert_eq!(
            p.span_at(Lane::Video, 9),
            Some(Span {
                start: 9,
                len: 11,
                from: None
            })
        );
        assert!(!p.place(Lane::Video, 0, clip(0, 7, 7, 0)), "empty clip");
    }

    #[test]
    fn ripple_delete_spans_partial_clips() {
        let mut p = three();
        // [2, 6) covers the tail of clip 0, all of clip 1 and the head of clip 2.
        assert!(p.ripple_delete(2, 4));
        assert_eq!(shape(&p)[0], vec![clip(0, 0, 2, 0), clip(2, 6, 9, 0)]);
        assert_eq!(shape(&p)[1], shape(&p)[0]);
        assert_eq!(p.timeline_frames(), 5);
        assert!(!p.ripple_delete(0, 0), "an empty range is not a delete");
        assert!(!p.ripple_delete(0, 100), "and one that empties every lane");
        assert!(p.undo());
        assert_eq!(shape(&p), shape(&three()));
    }

    #[test]
    fn undo_restores_exactly() {
        let mut p = three();
        let after_splits = shape(&p);
        assert!(p.delete(1));
        assert!(p.undo());
        assert_eq!(shape(&p), after_splits);

        // walk all the way back through both splits, then run dry
        assert!(p.undo());
        assert_eq!(p.clips().len(), 2);
        assert!(p.undo());
        assert_eq!(shape(&p)[0], vec![clip(0, 0, 9, 0)]);
        assert!(!p.undo(), "empty history");
        assert_eq!(shape(&p)[0], vec![clip(0, 0, 9, 0)]);
    }

    /// Undo/redo by hand across a split must never merge two takes into one
    /// group by reusing an id.
    #[test]
    fn group_ids_are_never_reused() {
        let mut p = Project::single(FILE, 9);
        assert!(p.split(3));
        let first = p.clips()[1].link;
        assert!(p.undo());
        assert!(p.split(5));
        assert_ne!(p.clips()[1].link, first, "a retired id must not come back");
    }

    #[test]
    fn segments_from_reads_the_audio_lane() {
        let p = three();
        // mid-clip: partial first segment, whole clips after it
        assert_eq!(
            p.segments_from(4, FPS),
            vec![
                (Some(0), 4.0 / 30.0, 5.0 / 30.0),
                (Some(0), 5.0 / 30.0, 9.0 / 30.0)
            ]
        );
        // on a boundary: nothing partial
        assert_eq!(
            p.segments_from(3, FPS),
            vec![
                (Some(0), 3.0 / 30.0, 5.0 / 30.0),
                (Some(0), 5.0 / 30.0, 9.0 / 30.0)
            ]
        );
        // from the top: one entry per clip
        assert_eq!(p.segments_from(0, FPS).len(), 3);
        // last clip only
        assert_eq!(p.segments_from(8, FPS), vec![(Some(0), 8.0 / 30.0, 0.3)]);
        // past the end / unusable fps
        assert!(p.segments_from(9, FPS).is_empty());
        assert!(p.segments_from(0, 0.0).is_empty());
        assert!(p.segments_from(0, f64::NAN).is_empty());
    }

    #[test]
    fn a_gap_in_the_audio_lane_is_a_silent_segment_of_its_own_length() {
        let mut p = three();
        assert!(p.lift(Lane::Audio, 1)); // silence over timeline [3, 5)
        assert_eq!(
            p.segments_from(0, FPS),
            vec![
                (Some(0), 0.0, 3.0 / 30.0),
                (None, 0.0, 2.0 / 30.0),
                (Some(0), 5.0 / 30.0, 9.0 / 30.0),
            ]
        );
        // The list still covers exactly the timeline: that is what stops the
        // master clock stalling at the hole.
        let played: f64 = p.segments_from(0, FPS).iter().map(|(_, a, b)| b - a).sum();
        assert!((played - 9.0 / 30.0).abs() < 1e-9, "{played}");
        // Starting inside the gap trims the silence, not the clip after it.
        assert_eq!(
            p.segments_from(4, FPS),
            vec![(None, 0.0, 1.0 / 30.0), (Some(0), 5.0 / 30.0, 9.0 / 30.0)]
        );
        // ...and a lane that is nothing but gap is a run of silence.
        assert!(p.lift(Lane::Audio, 0));
        assert!(p.lift(Lane::Audio, 0));
        assert_eq!(p.segments_from(0, FPS), vec![(None, 0.0, 0.3)]);
    }

    /// `three()` plus a whole second source appended: clips [0,3) [3,5) [5,9)
    /// of source 0 then [0,4) of source 1.
    fn two_sources() -> Project {
        let mut p = three();
        let s = p.import(FILE2);
        assert_eq!(s, 1);
        assert!(p.append_clip(s, 4));
        p
    }

    #[test]
    fn import_dedups_and_appends() {
        let mut p = Project::single(FILE, 9);
        assert_eq!(p.sources().len(), 1, "the opened file is source 0");
        assert_eq!(p.import(FILE), 0, "reimporting the open file reuses 0");
        assert_eq!(p.import(FILE2), 1);
        assert_eq!(p.import(FILE2), 1, "second import of the same path");
        assert_eq!(p.sources().len(), 2);
        assert!(!p.append_clip(2, 5), "unknown source index");
        assert_eq!(p.clips().len(), 1, "a refusal changes nothing");

        // Two spellings of one real file are one source: this is the case the
        // raw-path comparison above would get wrong.
        let here = concat!(env!("CARGO_MANIFEST_DIR"), "/src/project.rs");
        let detour = concat!(env!("CARGO_MANIFEST_DIR"), "/src/../src/project.rs");
        let mut p = Project::single(here, 9);
        assert_eq!(p.import(detour), 0);
        assert_eq!(p.sources().len(), 1);
    }

    #[test]
    fn append_clip_is_one_undo_step() {
        let mut p = two_sources();
        assert_eq!(p.timeline_frames(), 13);
        assert_eq!(shape(&p)[0][3], clip(9, 0, 4, 1));
        assert_eq!(shape(&p)[1][3], clip(9, 0, 4, 1), "and on the audio lane");
        assert_eq!(p.map_timeline(9), Some((3, 0)), "source 1 starts at 9");
        assert!(p.undo());
        assert_eq!(shape(&p), shape(&three()), "one step back to one source");
        assert_eq!(
            p.sources().len(),
            2,
            "the orphan source entry stays -- indexes are forever"
        );
        // ...and it is still usable, so a redo-by-hand costs no reimport.
        assert!(p.append_clip(1, 0));
        assert_eq!(p.clips()[3].len(), 1, "clamped like `single`");
    }

    #[test]
    fn edits_carry_the_source_along() {
        let mut p = two_sources();
        // A split inside the second source splits into two source-1 clips.
        assert!(p.split(11));
        assert_eq!(shape(&p)[0][3], clip(9, 0, 2, 1));
        assert_eq!(shape(&p)[0][4], clip(11, 2, 4, 1));

        // A source-1 clip pasted into source-0 territory keeps its file, and so
        // does the source-0 half that got split off around it.
        let copied = p.clips()[4];
        assert!(p.paste(1, copied));
        assert_eq!(p.clips()[1].source, 1);
        assert_eq!(p.clips()[1].in_frame, 2);
        assert_eq!(p.clips()[2].source, 0);
        assert!(p.undo());
        assert_eq!(p.clips()[1].source, 0, "undo restores sources too");
    }

    #[test]
    fn segments_from_names_the_source() {
        let p = two_sources();
        // Mid-clip in source 0, then the rest of source 0, then source 1 whole.
        assert_eq!(
            p.segments_from(4, FPS),
            vec![
                (Some(0), 4.0 / 30.0, 5.0 / 30.0),
                (Some(0), 5.0 / 30.0, 9.0 / 30.0),
                (Some(1), 0.0, 4.0 / 30.0),
            ]
        );
        // Mid-clip in source 1: only that source is left.
        assert_eq!(
            p.segments_from(10, FPS),
            vec![(Some(1), 1.0 / 30.0, 4.0 / 30.0)]
        );
        assert!(p.segments_from(13, FPS).is_empty());
        // Segments still cover exactly the timeline across the join.
        let played: f64 = p.segments_from(0, FPS).iter().map(|(_, a, b)| b - a).sum();
        assert!((played - f64::from(p.timeline_frames()) / FPS).abs() < 1e-9);
    }

    #[test]
    fn segments_follow_deletes_and_fps() {
        let mut p = three();
        assert!(p.delete(0));
        // source seconds, not timeline: deleting the head does not shift them
        let segs = p.segments_from(0, 25.0);
        assert_eq!(
            segs,
            vec![(Some(0), 3.0 / 25.0, 0.2), (Some(0), 0.2, 9.0 / 25.0)]
        );
        let played: f64 = segs.iter().map(|(_, a, b)| b - a).sum();
        assert!(
            (played - p.timeline_frames() as f64 / 25.0).abs() < 1e-9,
            "segments must cover exactly the timeline: {played}"
        );
    }

    #[test]
    fn orphan_sources_are_pruned_and_the_clips_reindexed() {
        // Import a second file, undo it: source 1 is now an orphan.
        let mut p = two_sources();
        assert!(p.undo());
        let (sources, video, audio) = p.without_orphan_sources();
        assert_eq!(sources, vec![PathBuf::from(FILE)], "the orphan is gone");
        assert_eq!(video, three().clips(), "the clips are untouched");
        assert_eq!(audio, three().lane(Lane::Audio));

        // Three sources where only the middle one is orphaned: the survivors
        // renumber, and the clips follow.
        let mut p = Project::single(FILE, 9);
        assert_eq!(p.import(FILE2), 1);
        assert_eq!(p.import("/nonexistent/c.mp4"), 2);
        assert!(p.append_clip(2, 4));
        let (sources, video, audio) = p.without_orphan_sources();
        assert_eq!(
            sources,
            vec![PathBuf::from(FILE), PathBuf::from("/nonexistent/c.mp4")]
        );
        assert_eq!(video.iter().map(|c| c.source).collect::<Vec<_>>(), [0, 1]);
        // ...and what comes out is loadable, with the same timeline.
        let reloaded = Project::from_parts(sources, video, audio).expect("from_parts");
        assert_eq!(reloaded.timeline_frames(), p.timeline_frames());
        assert_eq!(reloaded.sources().len(), 2);
    }

    #[test]
    fn from_parts_has_no_history_and_checks_the_invariants() {
        let (sources, video, audio) = three().without_orphan_sources();
        let mut p = Project::from_parts(sources.clone(), video.clone(), audio.clone())
            .expect("valid parts");
        assert_eq!(p.clips(), three().clips());
        assert!(!p.undo(), "a loaded project has nothing to undo");
        assert!(p.split(4), "...and is editable from there");
        assert!(p.undo());

        // A lane may be empty; both may not.
        assert!(Project::from_parts(sources.clone(), video.clone(), Vec::new()).is_ok());
        assert!(Project::from_parts(sources.clone(), Vec::new(), Vec::new()).is_err());
        let bad: [Vec<Clip>; 5] = [
            vec![clip(0, 0, 3, 1)],                   // source that is not there
            vec![clip(0, 3, 3, 0)],                   // empty clip
            vec![clip(3, 0, 3, 0), clip(0, 3, 5, 0)], // out of order
            vec![clip(0, 0, 5, 0), clip(3, 3, 5, 0)], // overlapping
            vec![clip(u32::MAX - 1, 0, 3, 0)],        // end past the last frame
        ];
        for clips in bad {
            assert!(
                Project::from_parts(sources.clone(), clips.clone(), Vec::new()).is_err(),
                "{clips:?}"
            );
            assert!(Project::from_parts(sources.clone(), video.clone(), clips).is_err());
        }
        // Group ids survive a load, and the next split gets a fresh one.
        let mut p = Project::from_parts(sources, video, audio).expect("valid parts");
        assert!(p.split(4));
        assert!(p.clips().iter().all(|c| c.link.is_some()));
    }

    /// The grouping rules, at the untrusted door and after every edit that can
    /// touch a link. A link id is at most one clip per lane and, when both lanes
    /// carry it, one span -- and no sequence of edits may produce otherwise,
    /// because what an edit produces is what a save writes and a load reads.
    #[test]
    fn a_link_id_is_never_two_clips_of_one_lane() {
        let sources = vec![PathBuf::from(FILE)];
        let linked = |start, in_frame, out_frame, link| Clip {
            start,
            in_frame,
            out_frame,
            source: 0,
            link: Some(link),
        };

        // The door: named errors, one per cause.
        let err = |video: Vec<Clip>, audio: Vec<Clip>| {
            Project::from_parts(sources.clone(), video, audio)
                .expect_err("refused")
                .to_string()
        };
        assert!(
            err(vec![linked(0, 0, 3, 7), linked(3, 3, 5, 7)], Vec::new())
                .contains("link 7 names two clips in the video lane"),
            "a duplicate id inside one lane is refused by name"
        );
        assert!(
            err(vec![linked(0, 0, 5, 2)], vec![linked(0, 0, 3, 2)])
                .contains("link 2 covers [0, 5) in video and [0, 3) in audio"),
            "a pair that does not cover one span is refused by name"
        );
        // A one-sided link is *not* an error: it is what a lift leaves behind.
        let mut p = Project::single(FILE, 9);
        assert!(p.lift(Lane::Audio, 0));
        let (sources, video, audio) = p.clone().without_orphan_sources();
        assert!(
            Project::from_parts(sources, video, audio).is_ok(),
            "a lifted lane's project has to load again"
        );

        // The edits: nothing below may break either rule.
        let consistent = |p: &Project, what: &str| {
            let lanes = Lane::ALL.map(|l| p.lane(l).to_vec());
            assert!(links_are_consistent(&lanes).is_ok(), "{what}: {lanes:?}");
        };
        let mut p = three();
        let copied = p.clips()[0];
        assert!(
            copied.link.is_some(),
            "the clipboard clip carries its group"
        );
        assert!(p.lift(Lane::Video, 0));
        assert!(p.place(Lane::Video, 0, copied));
        assert!(p.place(Lane::Video, 20, copied), "and again, further out");
        assert_eq!(
            p.clips().iter().filter(|c| c.link == copied.link).count(),
            0,
            "a placement belongs to no group, so it cannot duplicate one"
        );
        consistent(&p, "lift + place twice");
        assert!(
            p.place(Lane::Audio, 4, copied),
            "over a linked clip's middle"
        );
        consistent(&p, "place through a grouped clip");
        assert!(p.paste(6, copied), "a rippling paste through another");
        consistent(&p, "paste");
        assert!(p.split(2));
        consistent(&p, "split");
        assert!(p.ripple_delete(1, 3));
        consistent(&p, "ripple_delete");
        assert!(p.undo());
        consistent(&p, "undo");
    }
}
