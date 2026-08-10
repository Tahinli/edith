//! Edit list: N ordered lanes of *placed* source ranges. Pure data, no I/O.
//!
//! A [`Project`] holds video lanes and audio lanes -- `V1..Vn`, `A1..An`, named
//! by a [`Lane`] handle of a [`LaneKind`] and a 0-based `ord`. Each lane is a
//! list of
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
//! * the lanes are independent and are peers: deleting audio under a picture is
//!   one lane's business, and the picture does not shift. No lane is special
//!   except by what a *caller* reads -- playback and export take `V1`, the audio
//!   worker takes `A1`, until a compositing slice teaches them the rest.
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
//! same take and move together. An id is *not* a pairing of two lanes -- it
//! names at most one clip per lane and every clip carrying it covers the same
//! timeline span, on however many lanes ([`links_are_consistent`]).
//! [`Project::split`] cuts every lane at a timeline frame and hands each side
//! fresh ids, one per group of lanes whose halves line up; [`Project::regroup`]
//! is its inverse and rejoins them.
//!
//! Editing is metadata only. [`Project::split`] changes no timeline->source
//! mapping, so a running decoder stays correct across it; everything else does
//! and the caller must reseek. Every successful edit snapshots every lane, so
//! [`Project::undo`] is an exact restore -- of the clips *and* of the lane list.
//!
//! A clip names its file by *index* into [`Project::sources`], which is
//! append-only: an index handed out once stays valid forever, so a clip on the
//! clipboard or inside an undo snapshot can never dangle. An index -- not an
//! `Arc<Path>` -- because [`Clip`] is `Copy`, which is what makes copy/paste a
//! plain assignment.
//!
//! A clip names its equalizer the same way, by index into a second append-only
//! table ([`Clip::eq`], [`Project::set_eq`]): the params are a `Vec` of bands
//! and a `Copy` clip cannot hold one. So an EQ'd clip that is split hands both
//! halves the same settings, a copy carries them, and an undo restores whatever
//! index the clip had. Only a *save* prunes the table
//! ([`Project::without_orphan_sources`]), for the reason an undone import
//! leaves its source entry behind: an index handed out is an index forever.

use std::path::{Path, PathBuf};

use crate::eq::EqParams;

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
    /// Index into the project's equalizer table
    /// ([`Project::eq_of`]/[`Project::set_eq`]), or `None` for a clip that plays
    /// flat. An index -- not the params -- for [`Clip::source`]'s reason: this
    /// stays `Copy`, so an EQ'd clip survives a copy, a paste and an undo as a
    /// plain assignment. The table is append-only within a session, so an index
    /// on a clipboard clip or inside an undo snapshot can never dangle.
    pub eq: Option<u16>,
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

/// A file the timeline plays from, and *which* of its audio streams it plays:
/// a file carrying one track per language is several sources, one per stream,
/// sharing a path. Two entries differing only in the stream are two sources --
/// they are what a clip names, and a clip plays exactly one stream.
///
/// The path is always canonical ([`Source::new`]), so the same file reached by
/// two paths is one source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    /// Position among the file's audio tracks in file order, the numbering
    /// [`crate::AudioSession::probe_streams`] hands out. `0` for a file with a
    /// single track, and for a silent one.
    pub audio_stream: usize,
}

impl Source {
    pub fn new(path: impl AsRef<Path>, audio_stream: usize) -> Self {
        Self {
            path: canonical(path.as_ref()),
            audio_stream,
        }
    }
}

/// What a lane carries, which is what a *gap* in it means: black frames on a
/// video lane, silence on an audio one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LaneKind {
    Video,
    Audio,
}

/// Which lane an operation acts on: a kind plus a 0-based position among the
/// lanes of that kind, so [`Lane::V1`] is the first video lane and
/// `Lane::new(LaneKind::Audio, 1)` is `A2`.
///
/// A handle, not the lane itself. It stays meaningful while lanes are added,
/// and one naming a lane that is not there reads as an empty lane and refuses
/// every mutation -- a front-end that always asks for `V1`/`A1` needs no
/// bounds check of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lane {
    pub kind: LaneKind,
    pub ord: usize,
}

impl Lane {
    /// The first video lane: what a decoder plays and what an export renders.
    pub const V1: Lane = Lane::new(LaneKind::Video, 0);
    /// The first audio lane: what the audio worker plays.
    pub const A1: Lane = Lane::new(LaneKind::Audio, 0);

    pub const fn new(kind: LaneKind, ord: usize) -> Self {
        Self { kind, ord }
    }

    /// `V1`, `A2`: how a lane is written in a header column and named in an
    /// error about it.
    pub fn label(self) -> String {
        let kind = match self.kind {
            LaneKind::Video => 'V',
            LaneKind::Audio => 'A',
        };
        format!("{kind}{}", self.ord + 1)
    }
}

/// One lane: what it is and what it holds. A struct rather than a bare
/// `Vec<Clip>` because per-lane state is what the next slices add -- mute, a
/// compositing mode, a name -- and it belongs next to the clips it applies to.
#[derive(Clone, Debug)]
struct LaneData {
    kind: LaneKind,
    /// Sorted by `start` and disjoint ([`sorted_disjoint`]).
    clips: Vec<Clip>,
}

impl LaneData {
    /// The two lanes a project starts with: `V1` then `A1`, in display order.
    /// One place, because a freshly opened file is exactly this pair.
    fn two_lanes(video: Vec<Clip>, audio: Vec<Clip>) -> Vec<LaneData> {
        vec![
            LaneData {
                kind: LaneKind::Video,
                clips: video,
            },
            LaneData {
                kind: LaneKind::Audio,
                clips: audio,
            },
        ]
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

/// What a save writes and a load takes back: the sources, every lane in
/// display order with its kind, and the equalizer table their clips index into
/// -- [`Project::without_orphan_sources`] out, [`Project::from_parts`] in.
pub type Parts = (Vec<Source>, Vec<(LaneKind, Vec<Clip>)>, Vec<EqParams>);

/// The edit list plus its undo history.
#[derive(Clone, Debug)]
pub struct Project {
    /// Append-only: never popped, never reordered. See the module docs.
    sources: Vec<Source>,
    /// In display order, which is the order [`Lane::ord`] counts in: the nth
    /// lane of a kind here is that kind's `ord` n. Never empty.
    lanes: Vec<LaneData>,
    /// Append-only, exactly as `sources` is: what [`Clip::eq`] indexes into.
    eq: Vec<EqParams>,
    /// Snapshots pushed *before* each successful edit; `undo` pops one. The
    /// whole lane list, so adding a lane undoes as well.
    history: Vec<Vec<LaneData>>,
    /// Never rolled back by an undo: an id retired by an undone split must not
    /// come back and group two clips that were never together.
    next_link: u32,
}

impl Project {
    /// `V1` and `A1`, one clip each covering the whole of `path` and the two
    /// grouped -- the state of a freshly opened video, where the timeline is the
    /// source. `frame_count` of 0 would break the never-empty invariant, so it
    /// is clamped to one frame.
    pub fn single(path: impl AsRef<Path>, frame_count: u32) -> Self {
        let clip = Clip {
            start: 0,
            in_frame: 0,
            out_frame: frame_count.max(1),
            source: 0,
            link: Some(0),
            eq: None,
        };
        Self {
            // A file is opened on its first audio stream: nothing has picked
            // one yet, and every file with audio at all has that one.
            sources: vec![Source::new(path, 0)],
            lanes: LaneData::two_lanes(vec![clip], vec![clip]),
            eq: Vec::new(),
            history: Vec::new(),
            next_link: 1,
        }
    }

    /// A project rebuilt from a saved edit list -- the load half of
    /// [`crate::edith`]. The lanes come in display order, each with its kind, and
    /// an `ord` is a lane's position among the lanes of that kind, exactly as
    /// [`Project::lanes`] hands them back. History is *not* saved, so
    /// [`Project::undo`] is `false` until the first edit of the new session.
    /// This is the one door untrusted parts come in through, so every invariant
    /// every other constructor keeps is checked here, by name and in release:
    /// every lane empty, an empty clip, a clip naming a source (or an equalizer)
    /// that is not there, a clip whose end overflows, a lane that is unsorted or
    /// self-overlapping, and the grouping rules of [`Clip::link`] below.
    pub fn from_parts(
        sources: Vec<Source>,
        lanes: Vec<(LaneKind, Vec<Clip>)>,
        eq: Vec<EqParams>,
    ) -> crate::Result<Self> {
        if let Some(bad) = eq.iter().position(|p| !finite(p)) {
            return Err(format!("eq {bad} holds a band that is not a finite number").into());
        }
        if lanes.iter().all(|(_, clips)| clips.is_empty()) {
            return Err("every lane is empty: that is not a project".into());
        }
        let lanes: Vec<LaneData> = lanes
            .into_iter()
            .map(|(kind, clips)| LaneData { kind, clips })
            .collect();
        for (data, lane) in lanes.iter().zip(handles(&lanes)) {
            let name = lane.label();
            for c in &data.clips {
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
                if c.eq.is_some_and(|i| usize::from(i) >= eq.len()) {
                    return Err(format!(
                        "{name} clip at {} names eq {} of {}",
                        c.start,
                        c.eq.unwrap_or_default(),
                        eq.len()
                    )
                    .into());
                }
            }
            if !sorted_disjoint(&data.clips) {
                return Err(format!("the {name} lane is out of order or overlaps itself").into());
            }
        }
        links_are_consistent(&lanes)?;
        let next_link = lanes
            .iter()
            .flat_map(|l| &l.clips)
            .filter_map(|c| c.link)
            .max()
            // Saturating so a crafted file cannot make the counter wrap: at the
            // ceiling ids stop being fresh, which loses grouping, not memory.
            .map_or(0, |m| m.saturating_add(1));
        Ok(Self {
            sources: sources
                .iter()
                .map(|s| Source::new(&s.path, s.audio_stream))
                .collect(),
            lanes,
            eq,
            history: Vec::new(),
            next_link,
        })
    }

    /// The first video lane -- what an export renders and what a single-lane
    /// caller still means by "the clips".
    pub fn clips(&self) -> &[Clip] {
        self.lane(Lane::V1)
    }

    /// The clips of `lane`, or nothing at all for a lane that is not there.
    pub fn lane(&self, lane: Lane) -> &[Clip] {
        self.index(lane).map_or(&[][..], |i| &self.lanes[i].clips)
    }

    /// Every lane's handle, in display order -- what a walk over "all lanes"
    /// iterates, and what a front-end lays out top to bottom.
    pub fn lanes(&self) -> Vec<Lane> {
        handles(&self.lanes)
    }

    /// How many lanes of `kind` there are: `Lane::new(kind, ord)` names a lane
    /// for every `ord` below it, and none at or above it.
    pub fn lane_count(&self, kind: LaneKind) -> usize {
        self.lanes.iter().filter(|l| l.kind == kind).count()
    }

    /// Appends an empty lane of `kind` and hands back its handle. One undo
    /// step, because the lane list is state a front-end shows: an added lane
    /// that could not be taken back would be the one edit with no way out.
    /// Nothing plays differently until something is placed on it.
    ///
    /// The new lane being empty is fine even though [`Project::from_parts`]
    /// refuses an all-empty project: the lanes that were there still hold the
    /// timeline, and "no lane holds anything" is what that door refuses.
    pub fn add_lane(&mut self, kind: LaneKind) -> Lane {
        self.snapshot();
        self.lanes.push(LaneData {
            kind,
            clips: Vec::new(),
        });
        Lane::new(kind, self.lane_count(kind) - 1)
    }

    /// Where `lane` sits in [`Project::lanes`], or `None` for a lane that is
    /// not there. The one place a handle becomes a position.
    fn index(&self, lane: Lane) -> Option<usize> {
        self.lanes
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == lane.kind)
            .map(|(i, _)| i)
            .nth(lane.ord)
    }

    fn lane_mut(&mut self, lane: Lane) -> Option<&mut Vec<Clip>> {
        let i = self.index(lane)?;
        Some(&mut self.lanes[i].clips)
    }

    /// The files the clips index into, in import order; index 0 is the file the
    /// project was opened with.
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// The same list in the `(path, stream)` shape the audio engine indexes
    /// into -- [`crate::AudioSession::open_multi_streams`] and
    /// [`crate::AudioSession::copy_multi_streams`] take it, so the stream a clip
    /// was placed with is the stream that plays *and* the stream that exports.
    pub fn audio_sources(&self) -> Vec<(PathBuf, usize)> {
        self.sources
            .iter()
            .map(|s| (s.path.clone(), s.audio_stream))
            .collect()
    }

    /// Index for `path` played on `audio_stream`, appending it if it is new.
    /// Deduped by `fs::canonicalize` *and* by stream, so the same file reached
    /// by two paths imports once and two streams of one file are two sources.
    /// The lanes are untouched -- see [`Project::append_clip`] -- so this pushes
    /// no history: a source entry alone changes nothing playable.
    pub fn import(&mut self, path: impl AsRef<Path>, audio_stream: usize) -> usize {
        let source = Source::new(path, audio_stream);
        match self.sources.iter().position(|s| *s == source) {
            Some(idx) => idx,
            None => {
                self.sources.push(source);
                self.sources.len() - 1
            }
        }
    }

    /// Append a whole-file clip of `source` to the end of *every* lane, grouped
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
            eq: None,
        };
        for data in &mut self.lanes {
            data.clips.push(clip);
        }
        true
    }

    /// The sources a clip actually names, with the clips reindexed onto them
    /// -- what a save writes. Indexes are forever *inside* a session (see
    /// [`Project::append_clip`]), so an undone import leaves an orphan source
    /// entry behind; writing that orphan to a project file would let a file
    /// nothing plays refuse a future load. New indexes are assigned in order of
    /// first use -- lane by lane, in display order -- so the same project always
    /// emits the same bytes.
    ///
    /// Every lane comes out, in display order and with its kind: that is what a
    /// v5 `.edith` holds (empty lanes included) and what [`Project::from_parts`]
    /// takes back.
    ///
    /// The equalizer table is pruned the same way and for the same reason: an
    /// undone [`set_eq`](Project::set_eq) leaves settings nothing plays behind,
    /// and this is the one moment they can go -- the indexes that survive it are
    /// only the ones a clip names.
    pub fn without_orphan_sources(&self) -> Parts {
        let mut moved = vec![None; self.sources.len()];
        let mut sources = Vec::new();
        let mut moved_eq = vec![None; self.eq.len()];
        let mut eq = Vec::new();
        let mut lanes = self.lanes.clone();
        for c in lanes.iter_mut().flat_map(|l| &mut l.clips) {
            let old = c.source;
            c.source = match moved[old] {
                Some(new) => new,
                None => {
                    sources.push(self.sources[old].clone());
                    moved[old] = Some(sources.len() - 1);
                    sources.len() - 1
                }
            };
            if let Some(old) = c.eq.map(usize::from) {
                c.eq = Some(match moved_eq[old] {
                    Some(new) => new,
                    None => {
                        eq.push(self.eq[old].clone());
                        let new = (eq.len() - 1) as u16;
                        moved_eq[old] = Some(new);
                        new
                    }
                });
            }
        }
        (
            sources,
            lanes.into_iter().map(|l| (l.kind, l.clips)).collect(),
            eq,
        )
    }

    /// What the clip at `idx` of `lane` plays through, or `None` for one that
    /// plays flat (and for an index that is not there) -- what a feeder and an
    /// export ask before they build an [`crate::eq::EqState`].
    pub fn eq_of(&self, lane: Lane, idx: usize) -> Option<&EqParams> {
        self.eq.get(usize::from(self.lane(lane).get(idx)?.eq?))
    }

    /// Give the clip at `idx` of `lane` these equalizer settings, or `None` to
    /// take them off. One undo step, like every other edit. `false` -- and no
    /// history -- for an index that is not there and for params holding a value
    /// that is not finite, which is the one thing the file format could not
    /// write and read back.
    ///
    /// Settings equal to ones already in the table share their entry, so a
    /// project that puts the same curve on twenty clips writes it once.
    pub fn set_eq(&mut self, lane: Lane, idx: usize, params: Option<EqParams>) -> bool {
        if idx >= self.lane(lane).len() {
            return false;
        }
        let slot = match params {
            None => None,
            Some(params) => {
                if !finite(&params) {
                    return false;
                }
                Some(match self.eq.iter().position(|p| *p == params) {
                    Some(at) => at as u16,
                    // ponytail: the table is append-only within a session (see
                    // the module docs) and `Clip::eq` is a u16, so the 65535th
                    // *distinct* setting of one session is refused rather than
                    // silently aliased. Upgrade path if a UI ever drags a slider
                    // that far: widen `Clip::eq` to u32 (Clip has the room) or
                    // snapshot the table into the history and compact it here.
                    None if self.eq.len() >= usize::from(u16::MAX) => return false,
                    None => {
                        self.eq.push(params);
                        (self.eq.len() - 1) as u16
                    }
                })
            }
        };
        self.snapshot();
        self.lane_mut(lane).expect("checked above")[idx].eq = slot;
        true
    }

    /// Length of the timeline in frames: where the *last* lane runs out. A lane
    /// that ends early is a trailing gap in that lane, not a shorter timeline.
    pub fn timeline_frames(&self) -> u32 {
        self.lanes
            .iter()
            .filter_map(|l| l.clips.last().map(Clip::end))
            .max()
            .unwrap_or(0)
    }

    /// `(timeline_start, len)` per `V1` clip, in order -- what a UI lane needs
    /// to lay clips out. Gaps show up as the holes between consecutive entries.
    pub fn clip_spans(&self) -> Vec<(u32, u32)> {
        self.lane_spans(Lane::V1)
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

    /// [`map`](Project::map) on `V1` -- the mapping a decoder follows.
    pub fn map_timeline(&self, timeline_frame: u32) -> Option<(usize, u32)> {
        self.map(Lane::V1, timeline_frame)
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
    /// Lanes' halves share an id only where they are the *same* span: the lanes
    /// are edited apart, so the clip being cut here may start (or end)
    /// somewhere else on another lane, and a group id whose clips disagree
    /// about their span is not one take (see [`links_are_consistent`], which
    /// refuses to load one).
    ///
    /// Metadata only: no mapping changes, in any lane.
    pub fn split(&mut self, timeline_frame: u32) -> bool {
        let cut: Vec<Option<Clip>> = self
            .lanes
            .iter()
            .map(|l| splittable(&l.clips, timeline_frame).map(|idx| l.clips[idx]))
            .collect();
        if cut.iter().all(Option::is_none) {
            return false;
        }
        self.snapshot();
        // Each side is its own question: two clips may start together and end
        // apart, and every lane whose halves do line up stays one take.
        let starts: Vec<Option<u32>> = cut.iter().map(|c| c.map(|c| c.start)).collect();
        let ends: Vec<Option<u32>> = cut.iter().map(|c| c.map(|c| c.end())).collect();
        let left = self.group_ids(&starts);
        let right = self.group_ids(&ends);
        for ((data, left), right) in self.lanes.iter_mut().zip(left).zip(right) {
            let Some(idx) = splittable(&data.clips, timeline_frame) else {
                continue;
            };
            let mut tail = data.clips[idx];
            tail.in_frame += timeline_frame - tail.start;
            tail.start = timeline_frame;
            tail.link = right;
            data.clips[idx].out_frame = tail.in_frame;
            data.clips[idx].link = left;
            data.clips.insert(idx + 1, tail);
        }
        true
    }

    /// The inverse of [`split`](Project::split): rejoin the placements that meet
    /// at `timeline_frame` in every lane and put the result back in one group.
    /// Only what a split could have produced is rejoined -- the two sides must
    /// touch on the timeline *and* be consecutive frames of the same source --
    /// so the clip list comes back exactly as it was and traversal with it.
    /// `false` when no lane has such a pair. The rejoined clips share one id
    /// only when they rejoin into the same span, for [`split`](Project::split)'s
    /// reason.
    pub fn regroup(&mut self, timeline_frame: u32) -> bool {
        // What each lane would end up covering, if it joins here at all.
        let joined: Vec<Option<(u32, u32)>> = self
            .lanes
            .iter()
            .map(|l| {
                joinable(&l.clips, timeline_frame)
                    .map(|idx| (l.clips[idx].start, l.clips[idx + 1].end()))
            })
            .collect();
        if joined.iter().all(Option::is_none) {
            return false;
        }
        self.snapshot();
        let ids = self.group_ids(&joined);
        for (data, link) in self.lanes.iter_mut().zip(ids) {
            let Some(idx) = joinable(&data.clips, timeline_frame) else {
                continue;
            };
            data.clips[idx].out_frame = data.clips[idx + 1].out_frame;
            data.clips[idx].link = link;
            data.clips.remove(idx + 1);
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
    /// another lane to be grouped *with*; [`Project::regroup`] is how clips
    /// become a group again.
    ///
    /// Refused for an empty `clip` and for a lane that is not there.
    pub fn place(&mut self, lane: Lane, timeline_frame: u32, clip: Clip) -> bool {
        if clip.out_frame <= clip.in_frame || self.index(lane).is_none() {
            return false;
        }
        self.snapshot();
        let clip = Clip {
            start: timeline_frame,
            link: None,
            ..clip
        };
        let clips = self.lane_mut(lane).expect("checked above");
        clear(clips, clip.start, clip.end());
        let idx = clips.partition_point(|c| c.start < clip.start);
        clips.insert(idx, clip);
        debug_assert!(sorted_disjoint(clips));
        true
    }

    /// Insert `clip` into *every* lane at `timeline_frame` as one new group,
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
        for data in &mut self.lanes {
            open_room(&mut data.clips, at, clip.len());
            let idx = data.clips.partition_point(|c| c.start < at);
            data.clips.insert(idx, clip);
            debug_assert!(sorted_disjoint(&data.clips));
        }
        true
    }

    /// Lift the clip at `idx` out of `lane`, leaving a gap: black frames or
    /// silence, and nothing else moves. Refused for an out-of-range index (which
    /// a lane that is not there always is) and for the lift that would leave
    /// *every* lane empty -- an empty timeline is the front-end's state, not a
    /// project's (see the never-empty invariant).
    pub fn lift(&mut self, lane: Lane, idx: usize) -> bool {
        let placed: usize = self.lanes.iter().map(|l| l.clips.len()).sum();
        if idx >= self.lane(lane).len() || placed == 1 {
            return false;
        }
        self.snapshot();
        self.lane_mut(lane).expect("checked above").remove(idx);
        true
    }

    /// Cut the timeline frames `[at, at + len)` out of *every* lane and close
    /// the hole: everything after slides back by `len`. The rippling delete --
    /// [`lift`](Project::lift) is the one that leaves a gap. Refused for an
    /// empty range and when it would leave every lane empty.
    pub fn ripple_delete(&mut self, at: u32, len: u32) -> bool {
        if len == 0 {
            return false;
        }
        let survivors: usize = self
            .lanes
            .iter()
            .map(|l| {
                l.clips
                    .iter()
                    .filter(|c| c.start < at || c.end() > at + len)
                    .count()
            })
            .sum();
        if survivors == 0 {
            return false;
        }
        self.snapshot();
        for data in &mut self.lanes {
            clear(&mut data.clips, at, at + len);
            for c in data.clips.iter_mut().filter(|c| c.start >= at) {
                c.start -= len;
            }
            debug_assert!(sorted_disjoint(&data.clips));
        }
        true
    }

    /// Remove the `V1` clip at `idx` and everything under it, closing the gap
    /// -- the whole-group delete a single-lane front-end means. `false` for a
    /// bad index. Changes the mapping: the caller must reseek.
    pub fn delete(&mut self, idx: usize) -> bool {
        let Some(clip) = self.clips().get(idx).copied() else {
            return false;
        };
        self.ripple_delete(clip.start, clip.len())
    }

    /// Restore every lane from before the last successful edit -- the clips and
    /// the lane list both. `false` when there is nothing left to undo.
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
    /// audio worker, read off **`A1`**. A `None` source is a gap: the
    /// worker synthesises that many seconds of silence, which is what keeps the
    /// audio master clock counting across a hole instead of stalling on it.
    /// The first entry is partial when the position is mid-clip. Empty when the
    /// position is past the end or `fps` is not usable.
    pub fn segments_from(&self, timeline_frame: u32, fps: f64) -> Vec<(Option<usize>, f64, f64)> {
        if !(fps.is_finite() && fps > 0.0) {
            return Vec::new();
        }
        self.spans_from(Lane::A1, timeline_frame)
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

    /// One fresh group id per *distinct* key, in lane order, and `None` where a
    /// lane has no key -- how [`split`](Project::split) and
    /// [`regroup`](Project::regroup) hand out ids across however many lanes.
    /// Lanes sharing a key end up sharing an id, which is exactly the rule
    /// [`links_are_consistent`] enforces: one id, one span.
    fn group_ids<K: PartialEq>(&mut self, keys: &[Option<K>]) -> Vec<Option<u32>> {
        // Linear, over a handful of lanes: a hash of a lane list would cost
        // more than it saves and would not be deterministic for free.
        let mut seen: Vec<(&K, u32)> = Vec::new();
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            out.push(
                key.as_ref()
                    .map(|k| match seen.iter().find(|(other, _)| *other == k) {
                        Some(&(_, id)) => id,
                        None => {
                            let id = self.new_link();
                            seen.push((k, id));
                            id
                        }
                    }),
            );
        }
        out
    }
}

/// The handle of every lane, in storage order -- the one definition of what
/// [`Lane::ord`] counts.
fn handles(lanes: &[LaneData]) -> Vec<Lane> {
    let (mut video, mut audio) = (0, 0);
    lanes
        .iter()
        .map(|l| match l.kind {
            LaneKind::Video => {
                video += 1;
                Lane::new(LaneKind::Video, video - 1)
            }
            LaneKind::Audio => {
                audio += 1;
                Lane::new(LaneKind::Audio, audio - 1)
            }
        })
        .collect()
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
/// The one thing an equalizer setting may not be: a value the text format
/// cannot write and read back as itself. Checked wherever params come in, so
/// what an edit produces is always what a save writes and a load reads.
fn finite(params: &EqParams) -> bool {
    params
        .bands
        .iter()
        .all(|b| b.freq_hz.is_finite() && b.gain_db.is_finite() && b.q.is_finite())
}

fn sorted_disjoint(clips: &[Clip]) -> bool {
    clips.iter().all(|c| c.out_frame > c.in_frame)
        && clips.windows(2).all(|w| w[0].end() <= w[1].start)
}

/// The grouping invariant, checked in release at [`Project::from_parts`] and
/// asserted by the tests after every edit: a link id names **at most one** clip
/// per lane, and every clip carrying it covers the same timeline span -- that is
/// all "these move together" can mean.
///
/// Deliberately *not* a pairing of two lanes: with N lanes a take may run on
/// `V1`, `V2` and `A1` at once, and the rule that makes an id meaningful is span
/// identity, not which lane (or which `ord`) the partner sits on. Two lanes are
/// the case where it reads as "the picture and its sound".
///
/// A link no other lane carries is legal and is not an error: lifting one half
/// of a group ([`Project::lift`]) leaves exactly that, and a save of that
/// timeline has to load again.
fn links_are_consistent(lanes: &[LaneData]) -> crate::Result<()> {
    let handles = handles(lanes);
    for (data, lane) in lanes.iter().zip(&handles) {
        for (i, c) in data.clips.iter().enumerate() {
            let Some(id) = c.link else { continue };
            if data.clips[..i].iter().any(|prev| prev.link == Some(id)) {
                return Err(
                    format!("link {id} names two clips in the {} lane", lane.label()).into(),
                );
            }
        }
    }
    // First lane to carry an id fixes the span every other lane must agree on.
    let mut seen: Vec<(u32, Lane, u32, u32)> = Vec::new();
    for (data, &lane) in lanes.iter().zip(&handles) {
        for c in &data.clips {
            let Some(id) = c.link else { continue };
            match seen.iter().find(|(other, ..)| *other == id) {
                Some(&(_, first, start, end)) => {
                    if (start, end) != (c.start, c.end()) {
                        return Err(format!(
                            "link {id} covers [{start}, {end}) in {} and [{}, {}) in {}",
                            first.label(),
                            c.start,
                            c.end(),
                            lane.label()
                        )
                        .into());
                    }
                }
                None => seen.push((id, lane, c.start, c.end())),
            }
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
            eq: None,
        }
    }

    /// A recognisable equalizer setting: one peak band, `n` telling two of them
    /// apart -- which is what makes the table's dedup and its prune visible.
    fn band_at(n: u32) -> EqParams {
        EqParams {
            bands: vec![crate::eq::Band {
                freq_hz: 1000.0,
                gain_db: f64::from(n) as f32 * 1.5,
                q: 0.707,
                kind: crate::eq::BandKind::Peak,
            }],
        }
    }

    /// The `V1`, `A1` lane list a two-lane load hands [`Project::from_parts`].
    fn two(video: Vec<Clip>, audio: Vec<Clip>) -> Vec<(LaneKind, Vec<Clip>)> {
        vec![(LaneKind::Video, video), (LaneKind::Audio, audio)]
    }

    /// Every lane with its group ids blanked, for comparing shape.
    fn shape(p: &Project) -> Vec<Vec<Clip>> {
        p.lanes()
            .into_iter()
            .map(|l| {
                p.lane(l)
                    .iter()
                    .map(|c| Clip { link: None, ..*c })
                    .collect()
            })
            .collect()
    }

    /// The *source frame* every timeline frame reads, per lane -- the traversal
    /// a player performs. Deliberately not the clip index: a split changes which
    /// clip a frame belongs to, and nothing else.
    fn traversal(p: &Project) -> Vec<Vec<Option<u32>>> {
        (0..p.timeline_frames() + 2)
            .map(|f| {
                p.lanes()
                    .into_iter()
                    .map(|l| p.map(l, f).map(|(_, source)| source))
                    .collect()
            })
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
        assert_eq!(p.lane(Lane::A1).len(), 1);
        assert_eq!(p.timeline_frames(), 150);
        assert_eq!(p.clip_spans(), vec![(0, 150)]);
        // grouped: video and audio carry the same link
        assert_eq!(p.clips()[0].link, p.lane(Lane::A1)[0].link);
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
        for l in p.lanes() {
            assert_eq!(p.lane(l).len(), 2, "{} split", l.label());
        }
        // The two sides are two groups, one per side, matching across the lanes.
        let (v, a) = (p.lane(Lane::V1), p.lane(Lane::A1));
        assert_eq!(v[0].link, a[0].link);
        assert_eq!(v[1].link, a[1].link);
        assert_ne!(v[0].link, v[1].link, "the halves are no longer one take");
        // A split changes no mapping, in either lane.
        assert_eq!(traversal(&p), before, "a split moves nothing");

        assert!(p.regroup(4), "the inverse rejoins them");
        assert_eq!(shape(&p), before_shape, "bit-exact back to one clip");
        assert_eq!(traversal(&p), before);
        assert_eq!(
            p.lane(Lane::V1)[0].link,
            p.lane(Lane::A1)[0].link,
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
        assert!(p.lift(Lane::V1, 1));
        assert!(p.lift(Lane::A1, 1));
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
        assert!(p.lift(Lane::V1, 1));
        assert!(p.lift(Lane::A1, 1));
        assert!(!p.regroup(3), "nothing starts at 3 any more");
        let mut p = three();
        for l in p.lanes() {
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
            p.span_at(Lane::V1, 6),
            Some(Span {
                start: 6,
                len: 3,
                from: Some((0, 6))
            })
        );
        assert_eq!(p.span_at(Lane::V1, 9), None);
    }

    /// The clip a copy would hand back: source `[100, 102)`, unrelated to
    /// anything in `three()` so it is recognisable wherever it lands.
    const PASTED: Clip = Clip {
        start: 0,
        in_frame: 100,
        out_frame: 102,
        source: 0,
        link: None,
        eq: None,
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
        assert_eq!(p.lane_spans(Lane::A1), vec![(0, 3), (3, 4)]);
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
        assert!(p.lift(Lane::A1, 1));
        assert_eq!(shape(&p)[0], video_before, "the picture never moved");
        assert_eq!(p.timeline_frames(), 9, "nor did the timeline get shorter");
        assert_eq!(p.lane_spans(Lane::A1), vec![(0, 3), (5, 4)]);
        // The hole is a gap: mapped as nothing, spanned as a gap of its length.
        assert_eq!(p.map(Lane::A1, 4), None);
        assert_eq!(
            p.span_at(Lane::A1, 3),
            Some(Span {
                start: 3,
                len: 2,
                from: None
            })
        );
        assert_eq!(p.map(Lane::V1, 4), Some((1, 4)), "video plays on");
        assert!(p.undo());
        assert_eq!(p.lane_spans(Lane::A1), vec![(0, 3), (3, 2), (5, 4)]);
    }

    #[test]
    fn a_trailing_gap_is_a_gap_to_the_end_of_the_timeline() {
        let mut p = three();
        assert!(p.lift(Lane::A1, 2));
        assert_eq!(p.timeline_frames(), 9, "video still runs to 9");
        assert_eq!(
            p.span_at(Lane::A1, 5),
            Some(Span {
                start: 5,
                len: 4,
                from: None
            }),
            "the audio lane holds silence to the end of the picture"
        );
        assert_eq!(p.span_at(Lane::A1, 9), None);
        // Every lane covers the timeline exactly, gaps included.
        for l in p.lanes() {
            let spans = p.spans_from(l, 0);
            assert_eq!(spans.iter().map(|s| s.len).sum::<u32>(), 9, "{}", l.label());
            assert_eq!(spans[0].start, 0);
        }
    }

    #[test]
    fn the_last_clip_of_the_last_lane_cannot_be_lifted() {
        let mut p = Project::single(FILE, 9);
        assert!(p.lift(Lane::A1, 0), "a silent timeline is fine");
        assert!(!p.lift(Lane::V1, 0), "an empty one is not");
        assert!(!p.lift(Lane::A1, 0), "index past the end");
        assert_eq!(p.timeline_frames(), 9);
    }

    #[test]
    fn place_overwrites_only_its_own_lane() {
        let mut p = three();
        let audio_before = shape(&p)[1].clone();
        // Straddles [3,5) and eats into [5,9): the first is trimmed, the second
        // is trimmed, nothing shifts.
        assert!(p.place(Lane::V1, 4, clip(0, 100, 102, 0)));
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
        assert!(p.place(Lane::V1, 4, clip(0, 100, 101, 0)));
        assert_eq!(
            shape(&p)[0],
            vec![clip(0, 0, 4, 0), clip(4, 100, 101, 0), clip(5, 5, 9, 0)]
        );
        // ...and placed past the end it makes a gap, which is the whole point.
        assert!(p.place(Lane::V1, 20, clip(0, 100, 102, 0)));
        assert_eq!(p.timeline_frames(), 22);
        assert_eq!(
            p.span_at(Lane::V1, 9),
            Some(Span {
                start: 9,
                len: 11,
                from: None
            })
        );
        assert!(!p.place(Lane::V1, 0, clip(0, 7, 7, 0)), "empty clip");
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
        assert!(p.lift(Lane::A1, 1)); // silence over timeline [3, 5)
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
        assert!(p.lift(Lane::A1, 0));
        assert!(p.lift(Lane::A1, 0));
        assert_eq!(p.segments_from(0, FPS), vec![(None, 0.0, 0.3)]);
    }

    /// `three()` plus a whole second source appended: clips [0,3) [3,5) [5,9)
    /// of source 0 then [0,4) of source 1.
    fn two_sources() -> Project {
        let mut p = three();
        let s = p.import(FILE2, 0);
        assert_eq!(s, 1);
        assert!(p.append_clip(s, 4));
        p
    }

    #[test]
    fn import_dedups_and_appends() {
        let mut p = Project::single(FILE, 9);
        assert_eq!(p.sources().len(), 1, "the opened file is source 0");
        assert_eq!(p.import(FILE, 0), 0, "reimporting the open file reuses 0");
        assert_eq!(p.import(FILE2, 0), 1);
        assert_eq!(p.import(FILE2, 0), 1, "second import of the same path");
        // ...but the same file on another audio stream is another source: it is
        // what a clip names, and a clip plays exactly one stream.
        assert_eq!(p.import(FILE2, 1), 2, "a second stream is a second source");
        assert_eq!(p.import(FILE2, 1), 2, "and dedups on the pair");
        assert_eq!(p.sources()[2].audio_stream, 1);
        assert!(p.append_clip(2, 4), "a stream entry is placeable");
        assert!(p.undo());
        assert_eq!(p.sources().len(), 3);
        assert!(!p.append_clip(3, 5), "unknown source index");
        assert_eq!(p.clips().len(), 1, "a refusal changes nothing");

        // Two spellings of one real file are one source: this is the case the
        // raw-path comparison above would get wrong.
        let here = concat!(env!("CARGO_MANIFEST_DIR"), "/src/project.rs");
        let detour = concat!(env!("CARGO_MANIFEST_DIR"), "/src/../src/project.rs");
        let mut p = Project::single(here, 9);
        assert_eq!(p.import(detour, 0), 0);
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
        let (sources, lanes, eq) = p.without_orphan_sources();
        assert_eq!(sources, vec![Source::new(FILE, 0)], "the orphan is gone");
        assert!(eq.is_empty(), "and a project with no equalizer writes none");
        assert_eq!(
            lanes,
            two(three().clips().to_vec(), three().lane(Lane::A1).to_vec()),
            "the clips are untouched"
        );

        // Three sources where only the middle one is orphaned: the survivors
        // renumber, and the clips follow.
        let mut p = Project::single(FILE, 9);
        assert_eq!(p.import(FILE2, 0), 1);
        assert_eq!(p.import("/nonexistent/c.mp4", 0), 2);
        assert!(p.append_clip(2, 4));
        let (sources, lanes, eq) = p.without_orphan_sources();
        assert_eq!(
            sources,
            vec![Source::new(FILE, 0), Source::new("/nonexistent/c.mp4", 0)]
        );
        assert_eq!(
            lanes[0].1.iter().map(|c| c.source).collect::<Vec<_>>(),
            [0, 1]
        );
        // ...and what comes out is loadable, with the same timeline.
        let reloaded = Project::from_parts(sources, lanes, eq).expect("from_parts");
        assert_eq!(reloaded.timeline_frames(), p.timeline_frames());
        assert_eq!(reloaded.sources().len(), 2);
    }

    /// A clip's equalizer is the clip's: it survives every edit that copies a
    /// placement, and it comes back with an undo.
    #[test]
    fn an_equalizer_follows_the_clip_through_split_copy_and_undo() {
        let mut p = three();
        assert!(p.eq_of(Lane::V1, 0).is_none(), "a fresh clip plays flat");
        assert!(p.set_eq(Lane::A1, 0, Some(band_at(2))));
        assert_eq!(p.eq_of(Lane::A1, 0), Some(&band_at(2)));
        assert!(
            p.eq_of(Lane::V1, 0).is_none(),
            "one clip, not the lane above"
        );

        // Cutting an EQ'd clip must not silence half of it: both halves inherit.
        assert!(p.split(1));
        assert_eq!(p.eq_of(Lane::A1, 0), Some(&band_at(2)));
        assert_eq!(p.eq_of(Lane::A1, 1), Some(&band_at(2)));
        assert!(p.eq_of(Lane::A1, 2).is_none(), "the clip after it is flat");

        // A clipboard copy carries it -- onto another lane, at that.
        let copied = p.lane(Lane::A1)[1];
        assert!(p.place(Lane::V1, 20, copied));
        assert_eq!(p.eq_of(Lane::V1, 4), Some(&band_at(2)));
        assert!(p.undo());
        assert_eq!(p.lane(Lane::V1).len(), 4, "the placement came back off");

        // ...and a change to one is one undo step, in both directions.
        assert!(p.set_eq(Lane::A1, 0, Some(band_at(5))));
        assert_eq!(p.eq_of(Lane::A1, 0), Some(&band_at(5)));
        assert!(p.undo());
        assert_eq!(p.eq_of(Lane::A1, 0), Some(&band_at(2)));
        assert!(p.set_eq(Lane::A1, 0, None), "and taking it off is an edit");
        assert!(p.eq_of(Lane::A1, 0).is_none());
        assert!(p.undo());
        assert_eq!(p.eq_of(Lane::A1, 0), Some(&band_at(2)));

        // Two refusals, neither of which costs an undo step.
        let mut nan = band_at(1);
        nan.bands[0].q = f32::INFINITY;
        assert!(!p.set_eq(Lane::A1, 99, Some(band_at(1))), "no clip there");
        assert!(!p.set_eq(Lane::A1, 0, Some(nan)), "not a finite band");
        assert_eq!(p.eq_of(Lane::A1, 0), Some(&band_at(2)));
        assert!(p.undo());
        assert_eq!(
            p.lane(Lane::A1).len(),
            3,
            "one step back is before the split"
        );
        assert!(p.undo());
        assert!(
            p.eq_of(Lane::A1, 0).is_none(),
            "and the next is before the setting: the refusals pushed none"
        );
    }

    /// The table is append-only within a session, exactly as the sources are, so
    /// an undone setting is still in it -- and a save is where it goes.
    #[test]
    fn an_undone_equalizer_is_pruned_when_the_project_is_saved() {
        let mut p = three();
        assert!(p.set_eq(Lane::V1, 0, Some(band_at(1))));
        assert!(p.set_eq(Lane::V1, 0, Some(band_at(2))));
        assert!(p.undo(), "back to the first setting");
        assert_eq!(p.eq_of(Lane::V1, 0), Some(&band_at(1)));

        let (_, lanes, eq) = p.without_orphan_sources();
        assert_eq!(eq, vec![band_at(1)], "what nothing plays is not written");
        assert_eq!(lanes[0].1[0].eq, Some(0), "and the survivor renumbers");
        assert_eq!(lanes[0].1[1].eq, None);

        // One curve on three clips is one entry: settings that are equal share.
        assert!(p.set_eq(Lane::V1, 1, Some(band_at(1))));
        assert!(p.set_eq(Lane::A1, 2, Some(band_at(1))));
        let (_, lanes, eq) = p.without_orphan_sources();
        assert_eq!(eq.len(), 1, "equal settings share their entry");
        assert_eq!(lanes[1].1[2].eq, Some(0));
        reloads(&p, "an equalizer that outlived an undo");
    }

    #[test]
    fn from_parts_has_no_history_and_checks_the_invariants() {
        let (sources, lanes, eq) = three().without_orphan_sources();
        let (video, audio) = (lanes[0].1.clone(), lanes[1].1.clone());
        let mut p =
            Project::from_parts(sources.clone(), lanes.clone(), eq.clone()).expect("valid parts");
        assert_eq!(p.clips(), three().clips());
        assert!(!p.undo(), "a loaded project has nothing to undo");
        assert!(p.split(4), "...and is editable from there");
        assert!(p.undo());

        // A lane may be empty; every lane may not, and neither may no lane.
        assert!(
            Project::from_parts(sources.clone(), two(video.clone(), Vec::new()), Vec::new())
                .is_ok()
        );
        assert!(
            Project::from_parts(sources.clone(), two(Vec::new(), Vec::new()), Vec::new()).is_err()
        );
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
                Project::from_parts(sources.clone(), two(clips.clone(), Vec::new()), Vec::new())
                    .is_err(),
                "{clips:?}"
            );
            assert!(
                Project::from_parts(
                    sources.clone(),
                    two(video.clone(), clips.clone()),
                    Vec::new()
                )
                .is_err()
            );
            // ...and on a third lane, which is checked by the same walk.
            assert!(
                Project::from_parts(
                    sources.clone(),
                    vec![
                        (LaneKind::Video, video.clone()),
                        (LaneKind::Audio, audio.clone()),
                        (LaneKind::Video, clips),
                    ],
                    Vec::new()
                )
                .is_err()
            );
        }
        // An equalizer index no table entry answers, and a table entry the file
        // format could not have written, are refused at this door too.
        let eqd = |i: u16| {
            vec![Clip {
                eq: Some(i),
                ..video[0]
            }]
        };
        assert!(Project::from_parts(sources.clone(), two(eqd(0), Vec::new()), Vec::new()).is_err());
        let mut nan = band_at(1);
        nan.bands[0].freq_hz = f32::NAN;
        assert!(Project::from_parts(sources.clone(), two(eqd(0), Vec::new()), vec![nan]).is_err());
        let loaded =
            Project::from_parts(sources.clone(), two(eqd(0), Vec::new()), vec![band_at(1)])
                .expect("an eq the table holds");
        assert_eq!(loaded.eq_of(Lane::V1, 0), Some(&band_at(1)));

        // Group ids survive a load, and the next split gets a fresh one.
        let mut p = Project::from_parts(sources, lanes, eq).expect("valid parts");
        assert!(p.split(4));
        assert!(p.clips().iter().all(|c| c.link.is_some()));
    }

    /// The grouping rules, at the untrusted door and after every edit that can
    /// touch a link. A link id is at most one clip per lane and, wherever it is
    /// carried, one span -- and no sequence of edits may produce otherwise,
    /// because what an edit produces is what a save writes and a load reads.
    ///
    /// Amended for the lane model: the two errors name their lane `V1`/`A1`
    /// rather than "video"/"audio", because with N lanes "the video lane" is no
    /// longer a lane. Same causes, same door, same claim.
    #[test]
    fn a_link_id_is_never_two_clips_of_one_lane() {
        let sources = vec![Source::new(FILE, 0)];
        let linked = |start, in_frame, out_frame, link| Clip {
            start,
            in_frame,
            out_frame,
            source: 0,
            link: Some(link),
            eq: None,
        };

        // The door: named errors, one per cause.
        let err = |video: Vec<Clip>, audio: Vec<Clip>| {
            Project::from_parts(sources.clone(), two(video, audio), Vec::new())
                .expect_err("refused")
                .to_string()
        };
        assert!(
            err(vec![linked(0, 0, 3, 7), linked(3, 3, 5, 7)], Vec::new())
                .contains("link 7 names two clips in the V1 lane"),
            "a duplicate id inside one lane is refused by name"
        );
        assert!(
            err(vec![linked(0, 0, 5, 2)], vec![linked(0, 0, 3, 2)])
                .contains("link 2 covers [0, 5) in V1 and [0, 3) in A1"),
            "a pair that does not cover one span is refused by name"
        );
        // A one-sided link is *not* an error: it is what a lift leaves behind.
        let mut p = Project::single(FILE, 9);
        assert!(p.lift(Lane::A1, 0));
        let (sources, lanes, eq) = p.clone().without_orphan_sources();
        assert!(
            Project::from_parts(sources, lanes, eq).is_ok(),
            "a lifted lane's project has to load again"
        );

        // The edits: nothing below may break either rule.
        let consistent = |p: &Project, what: &str| {
            assert!(
                links_are_consistent(&p.lanes).is_ok(),
                "{what}: {:?}",
                p.lanes
            );
        };
        let mut p = three();
        let copied = p.clips()[0];
        assert!(
            copied.link.is_some(),
            "the clipboard clip carries its group"
        );
        assert!(p.lift(Lane::V1, 0));
        assert!(p.place(Lane::V1, 0, copied));
        assert!(p.place(Lane::V1, 20, copied), "and again, further out");
        assert_eq!(
            p.clips().iter().filter(|c| c.link == copied.link).count(),
            0,
            "a placement belongs to no group, so it cannot duplicate one"
        );
        consistent(&p, "lift + place twice");
        assert!(p.place(Lane::A1, 4, copied), "over a linked clip's middle");
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

    /// What an edit produces is what a save writes, so *every* state the public
    /// API can reach has to load again. The two the verifier panel found where
    /// it did not: a split and a regroup handed one group id to two clips that
    /// were not the same span, because the lanes had been edited apart.
    #[test]
    fn every_reachable_state_reloads() {
        let mut p = Project::single(FILE, 150);
        assert!(p.place(Lane::A1, 3, clip(0, 0, 50, 0)));
        assert!(p.split(10), "the lanes are cut apart at 10");
        reloads(&p, "place on one lane, then split");

        let mut p = Project::single(FILE, 150);
        assert!(p.split(30));
        assert!(p.lift(Lane::V1, 0), "the picture goes, the sound stays");
        assert!(p.regroup(30), "only the audio lane can rejoin");
        assert!(p.split(60));
        reloads(&p, "lift, regroup one lane, split");

        // Two audio streams of one file, both placed: two source entries with
        // one path between them, which is the state a stream picked out of the
        // library reaches -- and the state a save has to write and read back.
        let mut p = Project::single(FILE, 150);
        let second = p.import(FILE, 1);
        assert_eq!(second, 1, "the same file on another stream is a new source");
        assert!(p.append_clip(second, 150));
        assert!(p.split(200));
        reloads(
            &p,
            "a second audio stream of the same file, placed and split",
        );
        let (sources, ..) = p.without_orphan_sources();
        assert_eq!(
            sources.iter().map(|s| s.audio_stream).collect::<Vec<_>>(),
            [0, 1],
            "both entries survive the orphan prune: both are played"
        );
    }

    /// The same claim, swept: random op sequences off the public surface, the
    /// project reloaded after every one of them. A failure prints its seed, and
    /// the seed replays the whole sequence.
    #[test]
    fn random_edit_sequences_reload() {
        for seed in 0..200u64 {
            let mut rng = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let mut next = move |n: u32| {
                // xorshift64*: no dependency, and the seed replays it exactly.
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng >> 33) as u32 % n
            };
            let mut p = Project::single(FILE, 40);
            for step in 0..24 {
                let frame = next(44);
                // A clipboard copy carries the group id of the take it came from
                // -- the input that made a placement duplicate an id.
                let copied = *p
                    .lane(if next(2) == 0 { Lane::V1 } else { Lane::A1 })
                    .get(next(4) as usize)
                    .unwrap_or(&clip(0, 0, 5, 0));
                let lane = if next(2) == 0 { Lane::V1 } else { Lane::A1 };
                let idx = next(4) as usize;
                let _ = match next(11) {
                    0 => p.split(frame),
                    1 => p.regroup(frame),
                    2 => p.lift(lane, idx),
                    3 => p.place(lane, frame, copied),
                    4 => p.paste(frame, copied),
                    5 => p.ripple_delete(frame, next(9)),
                    6 => p.delete(idx),
                    7 => p.undo(),
                    // An equalizer put on and taken off in the mix: what a clip
                    // carries has to reload with it, and the entries an undo
                    // orphans must not come back out of the prune.
                    8 => p.set_eq(lane, idx, Some(band_at(next(4)))),
                    9 => p.set_eq(lane, idx, None),
                    _ => p.append_clip(0, 1 + next(9)),
                };
                reloads(&p, &format!("seed {seed}, step {step}"));
            }
        }
    }

    /// The save/load round trip a project must survive: the parts a save writes,
    /// handed back to the constructor a load goes through -- every lane of them,
    /// and the reloaded timeline has to be the same lanes in the same order.
    fn reloads(p: &Project, what: &str) {
        let (sources, lanes, eq) = p.clone().without_orphan_sources();
        match Project::from_parts(sources, lanes, eq) {
            Err(e) => panic!("{what}: saved but would not load: {e}\n{:?}", p.lanes),
            Ok(back) => {
                assert_eq!(back.lanes(), p.lanes(), "{what}: the lane list changed");
                // Placements, not whole clips: an orphan prune renumbers the
                // sources a clip names, which is the point of it.
                let spans = |p: &Project| -> Vec<Vec<(u32, u32)>> {
                    p.lanes().into_iter().map(|l| p.lane_spans(l)).collect()
                };
                assert_eq!(spans(&back), spans(p), "{what}: the placements changed");
                // The prune renumbers the eq table too, so what has to survive
                // is the settings themselves, clip by clip.
                let eqs = |p: &Project| -> Vec<Vec<Option<EqParams>>> {
                    p.lanes()
                        .into_iter()
                        .map(|l| {
                            (0..p.lane(l).len())
                                .map(|i| p.eq_of(l, i).cloned())
                                .collect()
                        })
                        .collect()
                };
                assert_eq!(eqs(&back), eqs(p), "{what}: the equalizers changed");
            }
        }
    }

    /// The two invariants [`Project::from_parts`] checks that are about *lanes*
    /// -- each one sorted and disjoint, and one span per group id -- asserted
    /// directly, where the point is the invariant rather than the round trip.
    fn invariants_hold(p: &Project, what: &str) {
        for lane in p.lanes() {
            assert!(
                sorted_disjoint(p.lane(lane)),
                "{what}: {} is out of order or overlaps itself: {:?}",
                lane.label(),
                p.lane(lane)
            );
        }
        if let Err(e) = links_are_consistent(&p.lanes) {
            panic!("{what}: {e}\n{:?}", p.lanes);
        }
    }

    /// A lane is a lane: `V2` is added, counted, reached by `(kind, ord)`, and
    /// edited without any other lane hearing about it. A handle naming a lane
    /// that is not there reads as empty and refuses to mutate.
    #[test]
    fn a_third_lane_is_a_peer() {
        let mut p = three();
        assert_eq!(p.lane_count(LaneKind::Video), 1);
        assert_eq!(p.lane_count(LaneKind::Audio), 1);
        let v2 = p.add_lane(LaneKind::Video);
        assert_eq!(v2, Lane::new(LaneKind::Video, 1));
        assert_eq!(v2.label(), "V2");
        assert_eq!(p.lane_count(LaneKind::Video), 2);
        assert_eq!(p.lanes(), vec![Lane::V1, Lane::A1, v2]);
        assert!(p.lane(v2).is_empty(), "a new lane holds nothing");

        // A handle for a lane that is not there: empty, and no mutation takes.
        let v3 = Lane::new(LaneKind::Video, 2);
        assert!(p.lane(v3).is_empty());
        assert!(!p.place(v3, 0, clip(0, 0, 3, 0)), "no lane, no placement");
        assert!(!p.lift(v3, 0));
        assert_eq!(p.lane_count(LaneKind::Video), 2, "and none was made");

        // Placing on V2 moves nothing else, and the lane can outrun the rest.
        let before = shape(&p);
        assert!(p.place(v2, 4, clip(0, 20, 32, 0)));
        assert_eq!(shape(&p)[0], before[0], "V1 never moved");
        assert_eq!(shape(&p)[1], before[1], "A1 never moved");
        assert_eq!(p.lane_spans(v2), vec![(4, 12)]);
        assert_eq!(
            p.timeline_frames(),
            16,
            "the last lane to end sets the length"
        );
        assert_eq!(p.map(v2, 5), Some((0, 21)));
        assert_eq!(p.map(v2, 3), None, "V2's own leading gap");
        assert_eq!(
            p.span_at(v2, 0),
            Some(Span {
                start: 0,
                len: 4,
                from: None
            })
        );
        // Every lane still covers the whole timeline, gaps included.
        for l in p.lanes() {
            let spans = p.spans_from(l, 0);
            assert_eq!(
                spans.iter().map(|s| s.len).sum::<u32>(),
                16,
                "{}",
                l.label()
            );
        }
        // ...and playback still reads V1 and A1, whatever else is there.
        assert_eq!(p.clips(), p.lane(Lane::V1));
        assert_eq!(p.map_timeline(4), p.map(Lane::V1, 4));
        let segments = p.segments_from(0, FPS);
        assert_eq!(segments[..3], three().segments_from(0, FPS)[..]);
        assert_eq!(
            segments[3],
            (None, 0.0, 7.0 / FPS),
            "A1 keeps the clock counting over the picture V2 added"
        );
        invariants_hold(&p, "place on V2");

        // A lift is that lane's business too, and the lane list undoes.
        assert!(p.lift(v2, 0));
        assert!(p.lane(v2).is_empty());
        assert_eq!(p.timeline_frames(), 9, "V2 no longer runs the longest");
        assert!(p.undo());
        assert_eq!(p.lane_spans(v2), vec![(4, 12)]);
        assert!(p.undo());
        assert!(p.lane(v2).is_empty(), "the placement came off V2");
        assert!(p.undo(), "and one more step takes the lane itself back");
        assert_eq!(p.lane_count(LaneKind::Video), 1);
        assert_eq!(shape(&p), shape(&three()));
    }

    /// The grouping rule across more than two lanes: a link id is one *span*,
    /// on however many lanes carry it -- not a video/audio pair and not a
    /// pairing by `ord`. So a split shares an id with every lane whose half
    /// lines up, and gives its own to every lane whose half does not.
    #[test]
    fn a_group_id_is_one_span_across_every_lane() {
        let mut p = Project::single(FILE, 9);
        let v2 = p.add_lane(LaneKind::Video);
        assert!(p.place(v2, 0, clip(0, 0, 9, 0)), "three lanes, one span");
        assert!(p.split(4));
        let side = |p: &Project, i: usize| -> Vec<Option<u32>> {
            p.lanes().into_iter().map(|l| p.lane(l)[i].link).collect()
        };
        let (left, right) = (side(&p, 0), side(&p, 1));
        assert!(
            left[0].is_some() && left.iter().all(|id| *id == left[0]),
            "one id for three left halves: {left:?}"
        );
        assert!(right.iter().all(|id| *id == right[0]), "{right:?}");
        assert_ne!(left[0], right[0], "the halves are no longer one take");
        invariants_hold(&p, "split across three lanes");

        // V2 ends early: same start, so the left halves are one take; different
        // end, so the right halves cannot be.
        let mut p = Project::single(FILE, 9);
        let v2 = p.add_lane(LaneKind::Video);
        assert!(p.place(v2, 0, clip(0, 0, 6, 0)));
        assert!(p.split(3));
        assert_eq!(p.lane(Lane::V1)[0].link, p.lane(v2)[0].link, "same [0, 3)");
        assert_ne!(
            p.lane(Lane::V1)[1].link,
            p.lane(v2)[1].link,
            "[3,9) vs [3,6)"
        );
        assert_eq!(p.lane(Lane::V1)[1].link, p.lane(Lane::A1)[1].link);
        invariants_hold(&p, "split where one lane ends early");

        // The inverse rejoins all three, and groups them the same way.
        assert!(p.regroup(3));
        assert_eq!(p.lane(Lane::V1)[0].link, p.lane(Lane::A1)[0].link);
        assert_ne!(
            p.lane(Lane::V1)[0].link,
            p.lane(v2)[0].link,
            "V2 rejoins into a span of its own"
        );
        invariants_hold(&p, "regroup across three lanes");

        // ...and the rule is enforced, not merely produced: two video lanes
        // carrying one id over two spans is refused, by lane name.
        let linked = |out_frame, link| Clip {
            start: 0,
            in_frame: 0,
            out_frame,
            source: 0,
            link: Some(link),
            eq: None,
        };
        let lanes = vec![
            LaneData {
                kind: LaneKind::Video,
                clips: vec![linked(3, 4)],
            },
            LaneData {
                kind: LaneKind::Video,
                clips: vec![linked(5, 4)],
            },
        ];
        assert_eq!(
            links_are_consistent(&lanes).unwrap_err().to_string(),
            "link 4 covers [0, 3) in V1 and [0, 5) in V2"
        );
    }

    /// A group is a set of clips over one span, at most one per lane: a picture
    /// may be grouped with sound in *any* audio lane -- there is no paired ord,
    /// and the lanes in between may be empty -- and the two ways to break that
    /// name the lane they broke it on. Lanes built by hand, because the rule is
    /// checked at the untrusted door and no edit can produce these.
    #[test]
    fn a_group_may_span_any_lane() {
        let lane = |kind, clips: Vec<Clip>| LaneData { kind, clips };
        let one = |start: u32, end: u32, link| Clip {
            start,
            in_frame: start,
            out_frame: end,
            source: 0,
            link: Some(link),
            eq: None,
        };
        // V1, A1, V2, A2 -- one take over [0, 4) on all four.
        let kinds = [
            LaneKind::Video,
            LaneKind::Audio,
            LaneKind::Video,
            LaneKind::Audio,
        ];
        let build = |clips: [Vec<Clip>; 4]| -> Vec<LaneData> {
            kinds.iter().zip(clips).map(|(&k, c)| lane(k, c)).collect()
        };
        let take = || vec![one(0, 4, 7)];
        assert!(links_are_consistent(&build([take(), take(), take(), take()])).is_ok());
        // The same take on V1 + A2 only, the lanes between it empty: legal, the
        // way a lifted half is legal.
        let across = build([take(), Vec::new(), Vec::new(), take()]);
        assert!(links_are_consistent(&across).is_ok(), "V1 grouped with A2");
        // Disagreeing about the span is not, and the message names both lanes.
        let apart = build([take(), Vec::new(), Vec::new(), vec![one(0, 6, 7)]]);
        assert_eq!(
            links_are_consistent(&apart).unwrap_err().to_string(),
            "link 7 covers [0, 4) in V1 and [0, 6) in A2"
        );
        // ...and one id twice in one lane is still that lane's error, by name.
        let twice = build([
            Vec::new(),
            Vec::new(),
            vec![one(0, 4, 7), one(4, 8, 7)],
            Vec::new(),
        ]);
        assert_eq!(
            links_are_consistent(&twice).unwrap_err().to_string(),
            "link 7 names two clips in the V2 lane"
        );
    }

    /// Every lane a save writes comes out, in display order and with its kind --
    /// an empty one included, since it is state a front-end shows -- and a
    /// two-lane project still writes exactly the parts it always did.
    #[test]
    fn a_save_writes_every_lane_it_has() {
        let mut p = three();
        let before = p.without_orphan_sources();
        assert_eq!(before.1.len(), 2, "V1 and A1");
        let v2 = p.add_lane(LaneKind::Video);
        assert!(p.place(v2, 4, clip(0, 20, 32, 0)));
        let a2 = p.add_lane(LaneKind::Audio);
        let (sources, lanes, eq) = p.without_orphan_sources();
        assert_eq!(
            lanes.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            [
                LaneKind::Video,
                LaneKind::Audio,
                LaneKind::Video,
                LaneKind::Audio
            ],
            "display order, not video-then-audio"
        );
        assert_eq!(lanes[2].1, p.lane(v2), "V2's clips are written");
        assert!(lanes[3].1.is_empty(), "and the empty A2 is still a lane");
        // ...and all four load again as the same four.
        let back = Project::from_parts(sources, lanes, eq).expect("four lanes load");
        assert_eq!(back.lanes(), p.lanes());
        assert_eq!(back.lane(a2), p.lane(a2));

        // Taking the lanes back leaves exactly what a two-lane save wrote.
        for _ in 0..3 {
            assert!(p.undo());
        }
        assert_eq!(
            p.without_orphan_sources(),
            before,
            "byte for byte the same parts a two-lane save always wrote"
        );
    }

    /// [`random_edit_sequences_reload`]'s sweep on a timeline that starts with
    /// three lanes and grows more: every lane's own order and the one
    /// span-per-id rule after every op, *and* the reload the v4 format restored
    /// as the oracle -- with `add_lane` in the op mix, so what is reloaded is a
    /// project of any number of lanes.
    #[test]
    fn random_edit_sequences_keep_many_lanes_consistent() {
        for seed in 0..200u64 {
            let mut rng = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let mut next = move |n: u32| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng >> 33) as u32 % n
            };
            let mut p = Project::single(FILE, 40);
            let v2 = p.add_lane(LaneKind::Video);
            assert!(p.place(v2, 5, clip(0, 0, 20, 0)));
            let lanes = [Lane::V1, Lane::A1, v2];
            for step in 0..24 {
                let frame = next(44);
                let copied = *p
                    .lane(lanes[next(3) as usize])
                    .get(next(4) as usize)
                    .unwrap_or(&clip(0, 0, 5, 0));
                let lane = lanes[next(3) as usize];
                let idx = next(4) as usize;
                let _ = match next(12) {
                    10 => p.set_eq(lane, idx, Some(band_at(next(4)))),
                    11 => p.set_eq(lane, idx, None),
                    0 => p.split(frame),
                    1 => p.regroup(frame),
                    2 => p.lift(lane, idx),
                    3 => p.place(lane, frame, copied),
                    4 => p.paste(frame, copied),
                    5 => p.ripple_delete(frame, next(9)),
                    6 => p.delete(idx),
                    7 => p.undo(),
                    8 => {
                        p.add_lane(if next(2) == 0 {
                            LaneKind::Video
                        } else {
                            LaneKind::Audio
                        });
                        true
                    }
                    _ => p.append_clip(0, 1 + next(9)),
                };
                invariants_hold(&p, &format!("seed {seed}, step {step}"));
                reloads(&p, &format!("seed {seed}, step {step}"));
            }
        }
    }
}
