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
//!   except by what a *caller* reads -- and what playback and export read is the
//!   whole stack: [`Project::composite_span_at`] resolves the video lanes to the
//!   one picture that shows, [`Project::audio_segments_from`] hands every audio
//!   lane over to be summed.
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
//!
//! Its colour grade is a third such table ([`Clip::color`],
//! [`Project::set_color`]) and behaves in every way like the equalizer one --
//! shared by equal settings, inherited by both halves of a split, pruned only at
//! a save. An index rather than the four floats themselves for the same reason:
//! twenty clips graded alike name one entry, and the file writes it once.

use std::path::{Path, PathBuf};

use crate::color::ColorParams;
use crate::eq::EqParams;
use crate::scale::FitPolicy;

/// How fast a clip plays, in **thousandths of real time**: 1000 is the speed it
/// was shot at, 2000 twice that, 500 half. An integer and not an `f32` for
/// [`Clip`]'s sake -- a clip is `Copy` *and* `Eq`, a float is neither exactly
/// comparable nor exactly writable, and `.edith` has to read back the very
/// number that was set. A thousandth is finer than any card can drag and coarser
/// than any rounding anyone can hear.
///
/// The rate alone: a speeded clip is resampled, so its pitch moves with it (the
/// tape effect). Nothing here preserves pitch and nothing here plays backwards
/// -- [`Speed::MIN`] is a quarter speed, [`Speed::MAX`] four times.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Speed(u16);

impl Speed {
    /// Real time -- what every clip is until something says otherwise, and the
    /// value every path in this engine short-circuits on.
    pub const NORMAL: Speed = Speed(1000);
    pub const MIN: Speed = Speed(250);
    pub const MAX: Speed = Speed(4000);

    /// Clamped into `[MIN, MAX]`, which is what makes a zero (or a reverse)
    /// unrepresentable rather than merely refused: nothing downstream divides
    /// by a speed that could be nothing.
    pub fn from_permille(permille: u16) -> Self {
        Speed(permille.clamp(Self::MIN.0, Self::MAX.0))
    }

    pub fn permille(self) -> u16 {
        self.0
    }

    pub fn is_normal(self) -> bool {
        self == Self::NORMAL
    }

    /// As a multiplier, for the sound worker and for a label.
    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 1000.
    }

    /// How many *timeline* frames `source` source frames occupy at this rate --
    /// the clip's footprint. Rounded to the nearest frame and **never zero**: a
    /// clip that occupied no frames would be an empty placement, which the lane
    /// invariant forbids and which nothing could ever click on again.
    pub fn frames(self, source: u32) -> u32 {
        let n = (u64::from(source) * 1000 + u64::from(self.0) / 2) / u64::from(self.0);
        n.clamp(1, u64::from(u32::MAX)) as u32
    }

    /// The mapping itself: which source frame of a clip the `offset`th timeline
    /// frame of it plays. Floored, so the first timeline frame is the clip's own
    /// in-point at every rate.
    pub fn source_at(self, offset: u32) -> u32 {
        (u64::from(offset) * u64::from(self.0) / 1000).min(u64::from(u32::MAX)) as u32
    }

    /// Its inverse, for a decoder's frames on their way back out: which timeline
    /// frame of the clip the `offset`th *source* frame of it lands on.
    ///
    /// **Ceil, and that is the whole point of the pair.** Playback stamps every
    /// decoded frame with this and shows the newest whose stamp has come due, so
    /// what is on screen at timeline offset `d` is the last source frame with
    /// `timeline_at(s) <= d` -- and with the floor above and the ceil here that
    /// frame is exactly [`source_at`](Speed::source_at)`(d)`, the frame an
    /// export encodes there, at every rate (`speed_maps_both_ways`). Rounding
    /// the two the same way would put the preview a frame off the export at some
    /// speeds and a whole held frame off at slow ones -- drift that no test of
    /// either side alone would catch.
    pub fn timeline_at(self, offset: u32) -> u32 {
        (u64::from(offset) * 1000)
            .div_ceil(u64::from(self.0))
            .min(u64::from(u32::MAX)) as u32
    }

    /// The longest source range that still *fits* in `frames` timeline frames at
    /// this rate -- and `None` when **nothing** does, which at a quarter speed is
    /// any room narrower than the four timeline frames one source frame occupies.
    ///
    /// What a trim commits and what a hole-punch leaves behind: rounding could
    /// otherwise hand back a range wider than the room, and a clip that outgrew
    /// its room would overlap its neighbour -- the one thing a lane may never do.
    /// `None` is not an error, it is "no remainder": the caller drops that piece
    /// (see [`clear`]) or refuses the edit ([`Project::trim`]). At real time this
    /// is `frames` itself, so nothing about an unspeeded edit changes.
    pub fn fit(self, frames: u32) -> Option<u32> {
        if frames == 0 || self.frames(1) > frames {
            return None;
        }
        let mut src = self.source_at(frames).max(1);
        // At most a step or two: `frames` is monotone in `src` and the rounding
        // above can only overshoot by one source frame's worth.
        while src > 1 && self.frames(src) > frames {
            src -= 1;
        }
        Some(src)
    }

    /// How many timeline frames `source` source frames of head (or tail) are
    /// worth -- what [`Project::trim_room`] measures a wall in. Zero for none of
    /// them, which is why this is not [`frames`](Speed::frames): a clip is never
    /// empty, but the room in front of one may well be.
    pub fn room(self, source: u32) -> u32 {
        match source {
            0 => 0,
            n => self.frames(n),
        }
    }

    /// How many timeline frames from `offset` on show the **same** source frame:
    /// one at real time and faster, more when the clip is slowed. What an export
    /// encodes without decoding the same picture twice, and the count that makes
    /// its frame walk exactly as long as the span.
    pub fn repeats(self, offset: u32, len: u32) -> u32 {
        let want = self.source_at(offset);
        (offset..len)
            .take_while(|&t| self.source_at(t) == want)
            .count() as u32
    }

    /// Where in the source a cut `offset` timeline frames into a clip of `len`
    /// source frames falls -- and `None` when this rate cannot address that
    /// frame at all.
    ///
    /// At half speed a clip of ten source frames is twenty timeline frames long
    /// and every source frame is on screen twice, so a cut between the two
    /// showings of one frame is not a cut in the *file*: taking it would leave
    /// two halves whose lengths no longer add up to the clip that was cut --
    /// a hole in the lane, or an overlap of the next clip. Refused instead, so
    /// the model stays exact and the front-end can say why. At real time every
    /// interior frame answers, which is the path a project without speeds is on.
    pub fn split_at(self, len: u32, offset: u32) -> Option<u32> {
        let src = self.source_at(offset);
        (src > 0
            && src < len
            && self.frames(src) == offset
            && self.frames(len - src) == self.frames(len) - offset)
            .then_some(src)
    }
}

impl Default for Speed {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl std::fmt::Display for Speed {
    /// `2.00x` -- what a card prints and what a clip box is marked with.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.2}x", self.as_f64())
    }
}

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
    /// Index into the project's colour table
    /// ([`Project::color_of`]/[`Project::set_color`]), or `None` for a clip that
    /// plays ungraded. An index for [`Clip::eq`]'s reason, and with the same
    /// promises: `Copy`, append-only within a session, never dangling.
    pub color: Option<u16>,
    /// How this clip's picture meets a project canvas of another shape
    /// ([`crate::scale::FitPolicy`]). Inline rather than a table index like the
    /// two above: it is one byte with no parameters, so there is nothing for a
    /// table to share, and a clip that is the project's own size never consults
    /// it at all. [`FitPolicy::Fit`] is the default -- the whole picture, bars
    /// where the aspect does not agree.
    pub fit: FitPolicy,
    /// How fast it plays ([`Speed`]). Inline like [`Clip::fit`] and unlike the
    /// eq and colour indexes: it is two bytes with nothing to share, and a table
    /// would put a level of indirection between the mapping and the one number
    /// every frame of it is divided by.
    ///
    /// It does **not** move `in_frame`/`out_frame`: those stay the source range
    /// the clip plays, so a trim and a split still address the file. What it
    /// changes is how many *timeline* frames that range is spread over
    /// ([`Clip::frames`]).
    pub speed: Speed,
}

impl Clip {
    /// Source frame count; `>= 1` by the never-empty invariant. What the clip
    /// reads out of its file -- not how long it is on the timeline, which is
    /// [`Clip::frames`] and differs the moment it is speeded.
    pub fn len(&self) -> u32 {
        self.out_frame - self.in_frame
    }

    /// Timeline frames it occupies: its source range at its own speed, never
    /// zero (see [`Speed::frames`]). Every placement question -- where it ends,
    /// what it overlaps, how wide its box is -- is asked of this and not of
    /// [`Clip::len`].
    pub fn frames(&self) -> u32 {
        self.speed.frames(self.len())
    }

    /// One past the last timeline frame it covers.
    pub fn end(&self) -> u32 {
        self.start + self.frames()
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

/// Which end of a clip a [`Project::trim`] moves: the one the pointer grabbed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    /// Its first timeline frame; moving it changes where in the source the clip
    /// starts reading, so the picture behind the edge slides with it.
    Start,
    /// One past its last timeline frame; moving it only says how much of the
    /// source to keep.
    End,
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
    /// The rate the clip this came off plays at ([`Clip::speed`]);
    /// [`Speed::NORMAL`] for a gap, which has no source to run fast. `len` is
    /// timeline frames whatever it says -- the speed is how many *source* frames
    /// those cover ([`Span::source_len`]) and how a decoder's own frame numbers
    /// come back to the timeline.
    pub speed: Speed,
}

impl Span {
    /// One past the last timeline frame it covers.
    pub fn end(&self) -> u32 {
        self.start + self.len
    }

    /// How many source frames it reads: what a decoder is asked for and how wide
    /// an audio window is. `len` at real time, and the one arithmetic both the
    /// picture and the sound go through, so they cannot disagree about where a
    /// speeded clip ends.
    pub fn source_len(&self) -> u32 {
        match self.from {
            // The last timeline frame's own source frame is the last one needed
            // -- never one more, which at a slow rate would read a frame the
            // next clip owns and at a fast one would decode a picture nothing
            // shows.
            Some(_) => self.speed.source_at(self.len.saturating_sub(1)) + 1,
            None => self.len,
        }
    }

    /// Where a source frame of this span lands on the timeline: the stamp
    /// playback puts on a decoded frame, through the ceil half of the pair
    /// ([`Speed::timeline_at`]). `in_frame` is the span's own first source
    /// frame, so a frame before it stamps at the span's start.
    pub fn timeline_at(&self, source_frame: u32) -> u32 {
        let base = self.from.map_or(0, |(_, in_frame)| in_frame);
        self.start + self.speed.timeline_at(source_frame.saturating_sub(base))
    }
}

/// What one speeded audio segment asks of the worker: how many source frames to
/// consume per output frame, and how many seconds of timeline it has to fill.
/// [`Project::audio_speeds_from`] builds it; `None` there means real time and no
/// resampling at all.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stretch {
    /// Source frames per output frame -- the speed as a multiplier.
    pub step: f64,
    /// What the segment owes the timeline, which is what it is padded or
    /// trimmed to.
    pub timeline_secs: f64,
}

/// What a save writes and a load takes back: the sources, every lane in
/// display order with its kind, and the equalizer and colour tables their clips
/// index into -- [`Project::without_orphan_sources`] out,
/// [`Project::from_parts`] in.
pub type Parts = (
    Vec<Source>,
    Vec<(LaneKind, Vec<Clip>)>,
    Vec<EqParams>,
    Vec<ColorParams>,
);

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
    /// The same, for [`Clip::color`].
    color: Vec<ColorParams>,
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
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };
        Self {
            // A file is opened on its first audio stream: nothing has picked
            // one yet, and every file with audio at all has that one.
            sources: vec![Source::new(path, 0)],
            lanes: LaneData::two_lanes(vec![clip], vec![clip]),
            eq: Vec::new(),
            color: Vec::new(),
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
    /// no lanes at all, an empty clip, a clip naming a source (or an equalizer,
    /// or a colour) that is not there, a clip whose end overflows, a lane that is
    /// unsorted or self-overlapping, and the grouping rules of [`Clip::link`]
    /// below.
    ///
    /// A project whose lanes are all *empty* is not among them: an emptied
    /// timeline is a project like any other -- it plays black and silent, it
    /// saves, and it loads back. What a project cannot be is laneless, because
    /// [`Project::lanes`] is what a front-end lays out and what every `Lane`
    /// handle indexes into.
    pub fn from_parts(
        sources: Vec<Source>,
        lanes: Vec<(LaneKind, Vec<Clip>)>,
        eq: Vec<EqParams>,
        color: Vec<ColorParams>,
    ) -> crate::Result<Self> {
        if let Some(bad) = eq.iter().position(|p| !finite(p)) {
            return Err(format!("eq {bad} holds a band that is not a finite number").into());
        }
        if let Some(bad) = color.iter().position(|p| !color_finite(p)) {
            return Err(format!("color {bad} holds a value that is not a finite number").into());
        }
        if lanes.is_empty() {
            return Err("no lanes at all: that is not a project".into());
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
                if c.start.checked_add(c.frames()).is_none() {
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
                if c.color.is_some_and(|i| usize::from(i) >= color.len()) {
                    return Err(format!(
                        "{name} clip at {} names color {} of {}",
                        c.start,
                        c.color.unwrap_or_default(),
                        color.len()
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
            color,
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
    /// An empty lane is a lane like any other -- so is a whole project of them
    /// ([`Project::from_parts`]); nothing plays until something is placed.
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
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };
        for data in &mut self.lanes {
            data.clips.push(clip);
        }
        true
    }

    /// Drops a source entry -- the file a library row *is* -- and renumbers
    /// every clip that indexed past it, which is why this is more than a
    /// `Vec::remove`.
    ///
    /// Refused, changing nothing, while any clip still plays from it: the
    /// refusal names the lanes and how many clips each holds, so a caller can
    /// say what has to be deleted first. Refused too for the last entry left,
    /// because a project that names no file cannot be reopened
    /// (`PlaybackSession::open_project`) and the timeline's audio parameters
    /// are read off source 0.
    ///
    /// ponytail: this retires the undo stack. `history` holds lanes alone, so a
    /// snapshot older than the removal can name the very source being removed
    /// (delete a file's clips, then remove the file) and restoring it would
    /// point a clip at an entry that is gone. The upgrade path is snapshotting
    /// `sources` beside `lanes` in [`snapshot`](Project::snapshot) -- which the
    /// "indexes are forever" rule ([`append_clip`](Project::append_clip), and
    /// the reason [`without_orphan_sources`](Project::without_orphan_sources)
    /// exists) currently rules out.
    pub fn remove_source(&mut self, idx: usize) -> crate::Result<()> {
        let Some(source) = self.sources.get(idx) else {
            return Err(format!("there is no source {idx} to remove").into());
        };
        let name = source.path.display().to_string();
        // Asked before the clips are counted: with one file left the answer is
        // the same whatever plays, and "the only file" is the more useful half
        // of it.
        if self.sources.len() == 1 {
            return Err(format!("{name} is the only file this project names").into());
        }
        let used: Vec<String> = handles(&self.lanes)
            .into_iter()
            .zip(&self.lanes)
            .filter_map(
                |(lane, data)| match data.clips.iter().filter(|c| c.source == idx).count() {
                    0 => None,
                    1 => Some(format!("{} (1 clip)", lane.label())),
                    n => Some(format!("{} ({n} clips)", lane.label())),
                },
            )
            .collect();
        if !used.is_empty() {
            return Err(format!("{name} still plays on {}", used.join(", ")).into());
        }
        self.sources.remove(idx);
        for c in self.lanes.iter_mut().flat_map(|l| &mut l.clips) {
            if c.source > idx {
                c.source -= 1;
            }
        }
        self.history.clear();
        Ok(())
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
    /// v6 `.edith` holds (empty lanes included) and what [`Project::from_parts`]
    /// takes back.
    ///
    /// The equalizer and colour tables are pruned the same way and for the same
    /// reason: an undone [`set_eq`](Project::set_eq) or
    /// [`set_color`](Project::set_color) leaves settings nothing plays behind,
    /// and this is the one moment they can go -- the indexes that survive it are
    /// only the ones a clip names.
    ///
    /// One exception, and it is the emptied timeline's: a project no clip plays
    /// from still keeps source 0. It is the file a session is scaffolded from --
    /// its frame rate is the timeline's and is written nowhere else -- so a save
    /// that pruned it would write a project that cannot be loaded back at all.
    pub fn without_orphan_sources(&self) -> Parts {
        let mut moved = vec![None; self.sources.len()];
        let mut sources = Vec::new();
        let mut moved_eq = vec![None; self.eq.len()];
        let mut eq = Vec::new();
        let mut moved_color = vec![None; self.color.len()];
        let mut color = Vec::new();
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
            if let Some(old) = c.color.map(usize::from) {
                c.color = Some(match moved_color[old] {
                    Some(new) => new,
                    None => {
                        color.push(self.color[old]);
                        let new = (color.len() - 1) as u16;
                        moved_color[old] = Some(new);
                        new
                    }
                });
            }
        }
        if sources.is_empty() {
            sources.extend(self.sources.first().cloned());
        }
        (
            sources,
            lanes.into_iter().map(|l| (l.kind, l.clips)).collect(),
            eq,
            color,
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

    /// How the clip at `idx` of `lane` is graded, or `None` for one that plays
    /// ungraded (and for an index that is not there) -- what the renderer and
    /// the export ask before they call [`crate::color::apply_yuv`].
    pub fn color_of(&self, lane: Lane, idx: usize) -> Option<&ColorParams> {
        self.color
            .get(usize::from(self.lane(lane).get(idx)?.color?))
    }

    /// Give the clip at `idx` of `lane` this colour grade, or `None` to take it
    /// off. [`Project::set_eq`]'s twin in every respect: one undo step, `false`
    /// (and no history) for an index that is not there or a value that is not
    /// finite, and equal settings share a table entry.
    pub fn set_color(&mut self, lane: Lane, idx: usize, params: Option<ColorParams>) -> bool {
        self.write_color(lane, idx, params, true)
    }

    /// [`set_color`](Self::set_color) without the undo step: the samples *inside*
    /// one pointer drag, whose first write (a plain `set_color`) already took the
    /// snapshot the gesture rolls back to. A drag across a slider is one undo,
    /// not one per pixel -- and undoing it lands where the hand picked it up.
    pub fn set_color_live(&mut self, lane: Lane, idx: usize, params: Option<ColorParams>) -> bool {
        self.write_color(lane, idx, params, false)
    }

    fn write_color(
        &mut self,
        lane: Lane,
        idx: usize,
        params: Option<ColorParams>,
        snapshot: bool,
    ) -> bool {
        if idx >= self.lane(lane).len() {
            return false;
        }
        let slot = match params {
            None => None,
            Some(params) => {
                if !color_finite(&params) {
                    return false;
                }
                Some(match self.color.iter().position(|p| *p == params) {
                    Some(at) => at as u16,
                    // ponytail: the 65535th *distinct* grade of one session is
                    // refused rather than silently aliased, exactly as `set_eq`
                    // refuses the 65535th curve; same upgrade path.
                    None if self.color.len() >= usize::from(u16::MAX) => return false,
                    None => {
                        self.color.push(params);
                        (self.color.len() - 1) as u16
                    }
                })
            }
        };
        if snapshot {
            self.snapshot();
        }
        self.lane_mut(lane).expect("checked above")[idx].color = slot;
        true
    }

    /// How fast the clip at `idx` of `lane` plays. [`Speed::NORMAL`] for an
    /// index that is not there, which is what every clip starts at anyway.
    pub fn speed_of(&self, lane: Lane, idx: usize) -> Speed {
        self.lane(lane).get(idx).map_or(Speed::NORMAL, |c| c.speed)
    }

    /// Sets it, for the clip **and its whole group**: a link is one span on
    /// however many lanes ([`links_are_consistent`]), so a picture sped up away
    /// from its sound would be a group no save could load. One snapshot, so the
    /// group's change is one [`Project::undo`].
    ///
    /// The source range is untouched -- a speed says how many timeline frames
    /// that range is spread over ([`Clip::frames`]), which is why a trim and a
    /// split still address the file afterwards.
    ///
    /// Refused, changing nothing and costing no undo step, when the clip would
    /// grow into the one after it on its own lane: slowing a clip down makes it
    /// wider, and a lane may not overlap itself. The refusal *names* the clip in
    /// the way -- by lane and timeline frame -- because "it did not fit" is not
    /// something a user can go and fix. Also for an index that is not there.
    pub fn set_speed(&mut self, lane: Lane, idx: usize, speed: Speed) -> crate::Result<()> {
        self.write_speed(lane, idx, speed, true)
    }

    /// [`set_speed`](Self::set_speed) without the undo step: the samples *inside*
    /// one pointer drag, whose first write (a plain `set_speed`) already took the
    /// snapshot the gesture rolls back to. A drag across the bar is one undo,
    /// not one per rate it passed through -- exactly as
    /// [`set_color_live`](Self::set_color_live) is for a slider.
    pub fn set_speed_live(&mut self, lane: Lane, idx: usize, speed: Speed) -> crate::Result<()> {
        self.write_speed(lane, idx, speed, false)
    }

    fn write_speed(
        &mut self,
        lane: Lane,
        idx: usize,
        speed: Speed,
        snapshot: bool,
    ) -> crate::Result<()> {
        let Some(members) = self.group_of(lane, idx) else {
            return Err(format!("there is no clip {idx} on {}", lane.label()).into());
        };
        let labels = handles(&self.lanes);
        let mut span: Option<(Lane, u32, u32)> = None;
        for &(l, i) in &members {
            let clips = &self.lanes[l].clips;
            let mut moved = clips[i];
            moved.speed = speed;
            if let Some(next) = clips.get(i + 1)
                && moved.end() > next.start
            {
                let name = labels[l].label();
                return Err(format!(
                    "at {speed} that clip would run to frame {} and the next {name} clip starts at {}: \
                     move it along, or trim this one first",
                    moved.end(),
                    next.start
                )
                .into());
            }
            // A link is **one span on however many lanes**
            // ([`links_are_consistent`]), and one rate over two halves does not
            // guarantee that: two clips of *different source lengths* can occupy
            // the same timeline frames at their old rates (rounding collapses
            // neighbouring lengths onto one footprint at 4x), and re-rating them
            // pulls those footprints apart. Refused by name rather than written
            // -- a group whose halves disagree about their span is a project
            // that would not load again.
            match span {
                None => span = Some((labels[l], moved.start, moved.end())),
                Some((first, start, end)) if (start, end) != (moved.start, moved.end()) => {
                    return Err(format!(
                        "at {speed} the {} half would cover [{}, {}) and the {} half [{start}, {end}): \
                         they are one take and a take is one span -- detach them first",
                        labels[l].label(),
                        moved.start,
                        moved.end(),
                        first.label()
                    )
                    .into());
                }
                Some(_) => {}
            }
        }
        if snapshot {
            self.snapshot();
        }
        for (l, i) in members {
            self.lanes[l].clips[i].speed = speed;
            debug_assert!(sorted_disjoint(&self.lanes[l].clips));
        }
        debug_assert!(links_are_consistent(&self.lanes).is_ok());
        Ok(())
    }

    /// How the clip at `idx` of `lane` meets a project canvas of another shape.
    /// [`FitPolicy::Fit`] for an index that is not there, which is the default
    /// every clip starts at anyway.
    pub fn fit_of(&self, lane: Lane, idx: usize) -> FitPolicy {
        self.lane(lane)
            .get(idx)
            .map_or(FitPolicy::default(), |c| c.fit)
    }

    /// Sets it. One undo step like every other edit, and `false` (with no
    /// history) for an index that is not there. Unlike an eq or a grade there is
    /// no "off": every clip has a policy, and `Fit` is what "off" would mean.
    pub fn set_fit(&mut self, lane: Lane, idx: usize, fit: FitPolicy) -> bool {
        if idx >= self.lane(lane).len() {
            return false;
        }
        self.snapshot();
        self.lane_mut(lane).expect("checked above")[idx].fit = fit;
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
        // Timeline frames, not source ones: a box on a lane is as wide as the
        // clip is long *there*, which a speed halves or quadruples.
        self.lane(lane)
            .iter()
            .map(|c| (c.start, c.frames()))
            .collect()
    }

    /// Timeline frame -> `(clip index, source frame)` in `lane`. `None` in a gap
    /// and past the end -- [`Project::span_at`] is the version that tells those
    /// two apart.
    pub fn map(&self, lane: Lane, timeline_frame: u32) -> Option<(usize, u32)> {
        let clips = self.lane(lane);
        let idx = at(clips, timeline_frame)?;
        Some((idx, source_frame(&clips[idx], timeline_frame)))
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
                from: Some((clips[idx].source, source_frame(&clips[idx], timeline_frame))),
                speed: clips[idx].speed,
            },
            None => Span {
                start: timeline_frame,
                speed: Speed::NORMAL,
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

    /// What the *composite* shows from `timeline_frame` on -- the answer
    /// [`span_at`](Project::span_at) gives for one lane, resolved across every
    /// video lane by one rule: **the topmost lane with a clip there wins**, and
    /// topmost is the *last* video lane in display order, so `V2` covers `V1`
    /// (the usual overlay convention). A frame no video lane covers is a gap,
    /// which is black however many lanes are above or below it.
    ///
    /// No blending, so exactly one lane is visible at a time and the composite
    /// is itself a span list -- which is why one decoder plays it, the same one
    /// a single-lane timeline uses, rather than N running at once.
    ///
    /// The span is cut short wherever a *higher* lane's clip starts, because
    /// from that frame on a different lane wins; that is what makes walking
    /// [`composite_spans_from`](Project::composite_spans_from) correct.
    ///
    /// ponytail: alpha, opacity or any blend mode makes this untrue -- two
    /// lanes would then be visible in one frame and the caller needs a decoder
    /// per lane plus a compositor. Upgrade path is a `Vec<Span>` per frame
    /// (this same walk, per lane) feeding N decoders.
    pub fn composite_span_at(&self, timeline_frame: u32) -> Option<Span> {
        let total = self.timeline_frames();
        if timeline_frame >= total {
            return None;
        }
        let video = self.video_lanes();
        // The last covering lane is the winner; only the lanes above it can
        // interrupt what it shows.
        let winner = video
            .iter()
            .rposition(|&lane| at(self.lane(lane), timeline_frame).is_some());
        let (from, speed, until) = match winner {
            Some(i) => {
                let clips = self.lane(video[i]);
                let idx = at(clips, timeline_frame).expect("just found above");
                (
                    Some((clips[idx].source, source_frame(&clips[idx], timeline_frame))),
                    clips[idx].speed,
                    clips[idx].end(),
                )
            }
            // Nothing covers it: black until something above the floor starts.
            None => (None, Speed::NORMAL, total),
        };
        let takeover = video[winner.map_or(0, |i| i + 1)..]
            .iter()
            .filter_map(|&lane| self.lane(lane).iter().find(|c| c.start > timeline_frame))
            .map(|c| c.start)
            .min();
        Some(Span {
            start: timeline_frame,
            len: takeover.map_or(until, |t| t.min(until)) - timeline_frame,
            from,
            speed,
        })
    }

    /// The video lanes in display order, topmost last -- the order the composite
    /// rule counts in.
    fn video_lanes(&self) -> Vec<Lane> {
        self.lanes()
            .into_iter()
            .filter(|l| l.kind == LaneKind::Video)
            .collect()
    }

    /// *Which* clip the composite shows at `timeline_frame`, named: the winner
    /// of [`composite_span_at`](Project::composite_span_at)'s own rule and its
    /// index on that lane. `None` over a gap and past the end. What a front-end
    /// asks to act on "the clip on screen" without re-deriving the rule.
    pub fn composite_clip_at(&self, timeline_frame: u32) -> Option<(Lane, usize)> {
        let video = self.video_lanes();
        let winner = video
            .iter()
            .rposition(|&lane| at(self.lane(lane), timeline_frame).is_some())?;
        Some((video[winner], at(self.lane(video[winner]), timeline_frame)?))
    }

    /// Which clip of *one* lane `timeline_frame` falls on, or `None` over a gap
    /// and past that lane's end. [`composite_clip_at`](Project::composite_clip_at)
    /// answers "the clip on screen"; this answers it per lane, which is what
    /// walking the lanes for something to select needs.
    pub fn lane_clip_at(&self, lane: Lane, timeline_frame: u32) -> Option<usize> {
        at(self.lane(lane), timeline_frame)
    }

    /// How the composite is graded at `timeline_frame`, so the picture playback
    /// converts and the one an export encodes are graded from one answer.
    /// `None` over a gap (black is black) and for a clip nobody has graded,
    /// which is the byte-identical path everywhere.
    pub fn composite_color_at(&self, timeline_frame: u32) -> Option<&ColorParams> {
        let (lane, idx) = self.composite_clip_at(timeline_frame)?;
        self.color_of(lane, idx)
    }

    /// How the composite meets the project canvas at `timeline_frame`, so the
    /// picture playback composes and the one an export encodes are placed from
    /// one answer. `Fit` over a gap, where black is already the canvas.
    pub fn composite_fit_at(&self, timeline_frame: u32) -> FitPolicy {
        self.composite_clip_at(timeline_frame)
            .map_or(FitPolicy::default(), |(lane, idx)| self.fit_of(lane, idx))
    }

    /// [`spans_from`](Project::spans_from) over the composite: every stretch the
    /// viewer sees from `timeline_frame` to the end, in order. What an export
    /// encodes and what playback walks at a clip boundary.
    pub fn composite_spans_from(&self, timeline_frame: u32) -> Vec<Span> {
        let mut out = Vec::new();
        let mut t = timeline_frame;
        while let Some(span) = self.composite_span_at(t) {
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
            // Where the cut lands *in the file*, which is the clip's own rate
            // away from where it lands on the timeline. Both halves keep the
            // speed, and `splittable` has already refused any frame at which
            // the two would not add up to the one being cut.
            tail.in_frame = split_source(&tail, timeline_frame).expect("splittable said so");
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

    /// Take the clip at `idx` of `lane` out of its group: every clip carrying
    /// its id -- on however many lanes -- is handed an id of its own, so from
    /// here on each half moves, trims and is deleted alone. The music video
    /// whose sound is to be cut against its picture starts here.
    ///
    /// An id of its own rather than none at all: a half no other lane is grouped
    /// with is exactly what a [`lift`](Project::lift) leaves behind and is legal
    /// (see [`links_are_consistent`]), and it is what a front-end already draws
    /// as detached. `None` would instead say "was never part of a take", which
    /// is what a one-lane [`place`](Project::place) means.
    ///
    /// Metadata only, like a [`split`](Project::split): no mapping changes, so
    /// nothing has to be reseeked. One snapshot, so one [`Project::undo`] puts
    /// the group back. Refused (`false`, nothing changed) for an index that is
    /// not there, a clip in no group at all, and one whose group has no other
    /// half -- that one is already detached, and a refusal must not cost an undo
    /// step.
    pub fn ungroup(&mut self, lane: Lane, idx: usize) -> bool {
        let Some(id) = self.lane(lane).get(idx).and_then(|c| c.link) else {
            return false;
        };
        let members = self
            .lanes
            .iter()
            .flat_map(|l| &l.clips)
            .filter(|c| c.link == Some(id))
            .count();
        if members < 2 {
            return false;
        }
        self.snapshot();
        // Drawn before the walk: `new_link` takes the whole project, and the
        // walk holds the lanes.
        let mut fresh = (0..members).map(|_| self.new_link()).collect::<Vec<_>>();
        for data in &mut self.lanes {
            for c in data.clips.iter_mut().filter(|c| c.link == Some(id)) {
                c.link = fresh.pop();
            }
        }
        debug_assert!(links_are_consistent(&self.lanes).is_ok());
        true
    }

    /// Put two clips on two lanes into one group, so what moves one moves the
    /// other again -- the undo of a [`ungroup`](Project::ungroup) by hand, and
    /// how a picture is regrouped with sound it was never opened with. Whatever
    /// either of them was grouped with comes along: those clips cover this same
    /// span already, so the result is one group and not two overlapping ones.
    ///
    /// Same frames or nothing: a group id names **one span** on however many
    /// lanes ([`links_are_consistent`] refuses to load anything else), so two
    /// clips that do not cover the same frames cannot be one take, and the
    /// refusal says which bounds to trim to. Kinds are *not* checked: a take may
    /// run on `V1` and `V2` at once, and picture-with-sound is the case people
    /// mean, not the rule.
    ///
    /// Metadata only, one snapshot. The error says what is wrong, because
    /// `false` would not: a bad index, one lane twice (a group is at most one
    /// clip per lane), spans that disagree, and a pair that is already one take
    /// -- nothing changes, and none of them costs an undo step.
    pub fn group(&mut self, a: Lane, a_idx: usize, b: Lane, b_idx: usize) -> crate::Result<()> {
        if a == b {
            return Err(format!(
                "a group is one clip per lane: pick the clip to group with on another track, not a second one on {}",
                a.label()
            )
            .into());
        }
        let clip = |p: &Self, lane: Lane, idx: usize| -> crate::Result<Clip> {
            p.lane(lane)
                .get(idx)
                .copied()
                .ok_or_else(|| format!("there is no clip {idx} on {}", lane.label()).into())
        };
        let (x, y) = (clip(self, a, a_idx)?, clip(self, b, b_idx)?);
        if (x.start, x.end()) != (y.start, y.end()) {
            return Err(format!(
                "{} covers [{}, {}) and {} covers [{}, {}): trim them to matching bounds first",
                a.label(),
                x.start,
                x.end(),
                b.label(),
                y.start,
                y.end()
            )
            .into());
        }
        if x.link.is_some() && x.link == y.link {
            return Err("those two are one take already".into());
        }
        self.snapshot();
        let id = self.new_link();
        // Every clip either of them was grouped with, by the same rule: all of
        // them cover this span, so one id over the lot stays consistent.
        let old = [x.link, y.link];
        for data in &mut self.lanes {
            for c in data.clips.iter_mut().filter(|c| c.link.is_some()) {
                if old.contains(&c.link) {
                    c.link = Some(id);
                }
            }
        }
        // And the two themselves, which a `place` may have left in no group.
        self.lane_mut(a).expect("read above")[a_idx].link = Some(id);
        self.lane_mut(b).expect("read above")[b_idx].link = Some(id);
        debug_assert!(links_are_consistent(&self.lanes).is_ok());
        Ok(())
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

    /// Move the clip at `idx` of `from` onto `to`, keeping the timeline frames
    /// it covers: the drag that rearranges takes across tracks. One snapshot, so
    /// one [`Project::undo`] puts it back where it was.
    ///
    /// Its group id travels with it, and that is not a desync: a link means
    /// "these cover one span on however many lanes" and names no lane at all
    /// (see [`links_are_consistent`]), so a picture moved from `V1` to `V2` is
    /// still the same take as the sound under it. The span never changes here --
    /// only which lane draws it.
    ///
    /// Refused, changing nothing, for a lane that is not there, an index that is
    /// not there, a move onto the lane it is already on, a move across *kinds*
    /// (a picture cannot play on an audio lane, and the save it wrote would not
    /// open again), and a landing that would touch another clip -- a move that
    /// overwrote what it landed on would destroy a take the pointer never named,
    /// which is what [`place`](Project::place) may do and a drag may not.
    pub fn move_to_lane(&mut self, from: Lane, idx: usize, to: Lane) -> bool {
        if from.kind != to.kind || from == to || self.index(to).is_none() {
            return false;
        }
        let Some(clip) = self.lane(from).get(idx).copied() else {
            return false;
        };
        if self
            .lane(to)
            .iter()
            .any(|c| c.start < clip.end() && clip.start < c.end())
        {
            return false;
        }
        self.snapshot();
        self.lane_mut(from).expect("checked above").remove(idx);
        let clips = self.lane_mut(to).expect("checked above");
        let at = clips.partition_point(|c| c.start < clip.start);
        clips.insert(at, clip);
        debug_assert!(sorted_disjoint(clips));
        true
    }

    /// Move one `edge` of the clip at `idx` of `lane` to timeline frame `to` --
    /// the drag on a clip's end that makes it play more or less of its source.
    /// The rest of the lane stays exactly where it is (nothing ripples), so what
    /// a shortened clip leaves behind is a gap and what a lengthened one takes
    /// is room that was already empty. One snapshot, so a whole drag is one
    /// [`Project::undo`] -- commit once, at the release, rather than per pointer
    /// sample. Changes the timeline->source mapping: the caller must reseek.
    ///
    /// [`Edge::Start`] moves the in-point with it: the frames that stay play
    /// exactly what they played before, which is what makes this a trim and not
    /// a slip.
    ///
    /// `to` is **clamped**, never refused -- a hand pulling an edge past what is
    /// legal means "as far as it goes", and stopping the box there is the
    /// affordance. The walls are: one frame of clip always survives, an edge
    /// never crosses the neighbouring clip on its own lane, an in-point never
    /// walks back past the source's first frame, and an out-point never runs
    /// past `source_frames[clip.source]` -- the caller's table of how long each
    /// source actually is ([`Project`] does not know, and a clip ending past its
    /// file's last frame is a save that will not open again; see
    /// `PlaybackSession::trim_clip`, which fills it in). A source with no entry
    /// there may not grow at all.
    ///
    /// Linked clips trim *together*, to one clamped edge: a link is one span on
    /// however many lanes ([`links_are_consistent`]), so a picture trimmed away
    /// from its sound would be a group no save could load. The room is therefore
    /// what every member of the group has -- the tightest wall wins.
    ///
    /// `false`, changing nothing and costing no undo step, for an index that is
    /// not there and for an edge that is already where it was asked to go.
    pub fn trim(
        &mut self,
        lane: Lane,
        idx: usize,
        edge: Edge,
        to: u32,
        source_frames: &[u32],
    ) -> bool {
        let (Some((lo, hi)), Some(clip)) = (
            self.trim_room(lane, idx, edge, source_frames),
            self.lane(lane).get(idx).copied(),
        ) else {
            return false;
        };
        let to = to.clamp(lo, hi);
        let at = match edge {
            Edge::Start => clip.start,
            Edge::End => clip.end(),
        };
        if to == at {
            return false;
        }
        // Worked out before anything moves: at a slow rate a room can be too
        // narrow to hold even one source frame ([`Speed::fit`]), and a member
        // that cannot fit must refuse the whole gesture rather than be given a
        // range wider than its room -- which is the overlap the lane invariant
        // exists to forbid. The walls below leave that room, so this is the
        // backstop for a project that came in through another door.
        let members = self.group_of(lane, idx).expect("checked above");
        let mut fitted = Vec::with_capacity(members.len());
        for &(l, i) in &members {
            let c = self.lanes[l].clips[i];
            let room = match edge {
                // The frames that stay play what they played, so what survives
                // is measured from the *end*.
                Edge::Start => c.end() - to,
                Edge::End => to - c.start,
            };
            match c.speed.fit(room) {
                // Not clamped to the clip's current length: an out-point trim
                // *grows* the range, and the source wall in `hi` is what bounds
                // that. The in-point's own clamp is at the write below.
                Some(keep) => fitted.push(keep),
                None => return false,
            }
        }
        self.snapshot();
        for (&(l, i), keep) in members.iter().zip(fitted) {
            let c = &mut self.lanes[l].clips[i];
            match edge {
                Edge::Start => {
                    // Non-negative by `lo`, which is what keeps the in-point on
                    // the source.
                    c.in_frame = c.out_frame - keep.min(c.out_frame);
                    c.start = to;
                }
                // Never wider than the room the edge was clamped to: see
                // [`Speed::fit`].
                Edge::End => c.out_frame = c.in_frame + keep,
            }
            debug_assert!(sorted_disjoint(&self.lanes[l].clips));
        }
        true
    }

    /// How far that edge may travel, `(first, last)` timeline frame inclusive --
    /// the walls [`trim`](Project::trim) clamps to, without moving anything.
    /// What a front-end drawing the box *during* a drag asks, so the live width
    /// is the width the release will commit and an edge stops under the pointer
    /// rather than snapping back. `None` for an index that is not there.
    pub fn trim_room(
        &self,
        lane: Lane,
        idx: usize,
        edge: Edge,
        source_frames: &[u32],
    ) -> Option<(u32, u32)> {
        let (mut lo, mut hi) = (u32::MIN, u32::MAX);
        for (l, i) in self.group_of(lane, idx)? {
            let clips = &self.lanes[l].clips;
            let c = clips[i];
            let (member_lo, member_hi) = match edge {
                Edge::Start => (
                    // Back to the source's own first frame -- as many *timeline*
                    // frames as that head is worth at the clip's rate, which at
                    // real time is the head itself -- and never over the clip in
                    // front of it. Saturating because a clip may hold *more*
                    // head than the timeline has room for -- a ripple delete
                    // slides a clip back to frame 0 with its in-point wherever
                    // the cut left it -- and frame 0 is the other wall.
                    c.start
                        .saturating_sub(c.speed.room(c.in_frame))
                        .max(i.checked_sub(1).map_or(0, |p| clips[p].end())),
                    // One *frame of clip* always survives, and at a rate below
                    // real time one frame of clip is several frames of timeline
                    // ([`Speed::room`]): an edge dragged closer than that would
                    // ask for a range no source frame fits in. At real time this
                    // is `end - 1`, unchanged.
                    c.end() - c.speed.room(1),
                ),
                Edge::End => (
                    // ...and the same wall from the other end: the shortest this
                    // clip can be is the room one source frame of it takes.
                    c.start + c.speed.room(1),
                    // Out to whatever is left of the source -- again in timeline
                    // frames, at this clip's rate -- and never over the clip
                    // behind it.
                    c.start
                        .saturating_add(
                            c.speed.room(
                                source_frames
                                    .get(c.source)
                                    .copied()
                                    .unwrap_or(c.out_frame)
                                    .saturating_sub(c.in_frame),
                            ),
                        )
                        .min(clips.get(i + 1).map_or(u32::MAX, |n| n.start)),
                ),
            };
            lo = lo.max(member_lo);
            hi = hi.min(member_hi);
        }
        // `hi.max(lo)`: for a clip the invariants hold for, the range always
        // contains the edge's own place, and a caller's wrong `source_frames`
        // must not become an empty range (or a panicking `clamp`) here.
        Some((lo, hi.max(lo)))
    }

    /// The clips that move as one with the clip at `idx` of `lane` -- itself and
    /// whatever carries its link on the other lanes -- as `(lane storage index,
    /// clip index)` pairs. `None` for an index that is not there.
    fn group_of(&self, lane: Lane, idx: usize) -> Option<Vec<(usize, usize)>> {
        let clip = *self.lane(lane).get(idx)?;
        Some(match clip.link {
            Some(link) => self
                .lanes
                .iter()
                .enumerate()
                .filter_map(|(l, data)| {
                    Some((l, data.clips.iter().position(|c| c.link == Some(link))?))
                })
                .collect(),
            None => vec![(self.index(lane).expect("the clip was found on it"), idx)],
        })
    }

    /// Insert `clip` into the first lane of each kind at `timeline_frame` as one
    /// new group, pushing everything from there on later by its length in
    /// *every* lane -- the grouped, rippling paste a clipboard does. Mid-clip
    /// the clip it lands in is split around it; at or past the end of the
    /// timeline it is appended, because a paste means "put it here", not "put it
    /// here and leave black in front".
    /// Use [`place`](Project::place) to paste into one lane, or to make a gap.
    ///
    /// `V1` and `A1` and no other lane, because a take is one picture and one
    /// sound: copying it onto every lane there is would play the same audio
    /// twice over (and leave an mp4 export with two tracks to copy). A clip of a
    /// source with no picture ([`crate::is_audio`]) reaches `A1` only -- on a
    /// video lane it is a clip that decodes to nothing, and a save carrying one
    /// does not open again -- and a still image ([`crate::is_image`]) reaches
    /// `V1` only, for the mirror of that reason. The room is still opened
    /// everywhere, or the lanes it was not inserted into would slide out of step
    /// with the ones it was.
    ///
    /// Exactly one history snapshot, so one [`Project::undo`] takes it back.
    /// Changes the timeline->source mapping: the caller must reseek. Refused for
    /// an empty `clip` and for one naming a source that is not there (both of
    /// which the public fields let a caller build), and for a paste whose ripple
    /// would push a clip past the last frame there is -- nothing is changed by
    /// any of them.
    pub fn paste(&mut self, timeline_frame: u32, clip: Clip) -> bool {
        let Some(source) = self.sources.get(clip.source) else {
            return false;
        };
        if clip.out_frame <= clip.in_frame {
            return false;
        }
        let audio_only = crate::is_audio(&source.path);
        // ...and its mirror: a still is a picture and no sound, so a copy of one
        // reaches `V1` only. Pasted onto `A1` as well it is a PNG the audio
        // worker tries to demux -- which silences the *whole session*, not just
        // that clip -- and a save the engine's own `open_project` then refuses.
        let picture_only = crate::is_image(&source.path);
        let at = timeline_frame.min(self.timeline_frames());
        // The room it takes on the timeline, which its speed decides -- not the
        // source range it reads.
        let len = clip.frames();
        // Room for it: `open_room` adds `len` to every start from `at` on, and a
        // hand-written file may hold a clip that ends at the very last frame
        // (`edith::check` permits `start + len == u32::MAX`). Refused here
        // rather than wrapped there -- a wrapped start lands *behind* the clip
        // it belongs after, which is an overlap no save will read back.
        if at.checked_add(len).is_none()
            || self
                .lanes
                .iter()
                .flat_map(|l| &l.clips)
                .any(|c| c.end() > at && c.end().checked_add(len).is_none())
        {
            return false;
        }
        // ...and room *to open*: the paste splits whatever it lands inside of,
        // and a speeded clip can only be cut where its own rate has a source
        // frame ([`Speed::split_at`]). Refused rather than pasted over the top
        // of one, which is the overlap a lane may never hold. At real time every
        // frame answers, so this cannot refuse an unspeeded paste.
        let lands_inside = |clips: &[Clip]| clips.iter().any(|c| c.start < at && at < c.end());
        if self
            .lanes
            .iter()
            .any(|l| lands_inside(&l.clips) && splittable(&l.clips, at).is_none())
        {
            return false;
        }
        self.snapshot();
        let clip = Clip {
            start: at,
            link: Some(self.new_link()),
            ..clip
        };
        // Only lanes that can play the source: a file with no picture belongs on
        // an audio lane and nowhere else, exactly as `place_stream_at` decides
        // it for an import. On a video lane it would decode to nothing, and the
        // save it wrote would not open again.
        let kinds: &[LaneKind] = match (audio_only, picture_only) {
            (true, _) => &[LaneKind::Audio],
            (_, true) => &[LaneKind::Video],
            _ => &[LaneKind::Video, LaneKind::Audio],
        };
        let takes: Vec<usize> = kinds
            .iter()
            .filter_map(|&kind| self.index(Lane::new(kind, 0)))
            .collect();
        for (i, data) in self.lanes.iter_mut().enumerate() {
            open_room(&mut data.clips, at, len);
            if takes.contains(&i) {
                let idx = data.clips.partition_point(|c| c.start < at);
                data.clips.insert(idx, clip);
            }
            debug_assert!(sorted_disjoint(&data.clips));
        }
        true
    }

    /// Lift the clip at `idx` out of `lane`, leaving a gap: black frames or
    /// silence, and nothing else moves. Refused only for an out-of-range index
    /// (which a lane that is not there always is) -- lifting the last placement
    /// there is leaves an *empty* timeline, which is a state the project holds
    /// like any other and an undo brings back.
    pub fn lift(&mut self, lane: Lane, idx: usize) -> bool {
        if idx >= self.lane(lane).len() {
            return false;
        }
        self.snapshot();
        self.lane_mut(lane).expect("checked above").remove(idx);
        true
    }

    /// Cut the timeline frames `[at, at + len)` out of *every* lane and close
    /// the hole: everything after slides back by `len`. The rippling delete --
    /// [`lift`](Project::lift) is the one that leaves a gap. Refused only for an
    /// empty range; a delete that leaves nothing behind empties the timeline,
    /// which is a state like any other and one undo away.
    pub fn ripple_delete(&mut self, at: u32, len: u32) -> bool {
        if len == 0 {
            return false;
        }
        // Nothing reaches past `at`: there is nothing to cut and nothing to
        // slide back, and a delete that changes nothing must not cost an undo
        // step (see [`snapshot`](Project::snapshot)).
        if !self.lanes.iter().flat_map(|l| &l.clips).any(|c| c.end() > at) {
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
        self.delete_in(Lane::V1, idx)
    }

    /// [`delete`](Project::delete) for the lane the clip was picked on: the
    /// clip's own span is cut out of *every* lane, so a take deletes whole from
    /// whichever half of it was clicked. `false` for a bad index, and for a lane
    /// that is not there.
    pub fn delete_in(&mut self, lane: Lane, idx: usize) -> bool {
        let Some(clip) = self.lane(lane).get(idx).copied() else {
            return false;
        };
        self.ripple_delete(clip.start, clip.frames())
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
        self.lane_segments_from(Lane::A1, timeline_frame, fps)
    }

    /// [`segments_from`](Project::segments_from) for a named lane.
    pub fn lane_segments_from(
        &self,
        lane: Lane,
        timeline_frame: u32,
        fps: f64,
    ) -> Vec<(Option<usize>, f64, f64)> {
        if !(fps.is_finite() && fps > 0.0) {
            return Vec::new();
        }
        self.spans_from(lane, timeline_frame)
            .iter()
            .map(|span| match span.from {
                // A clip's window is in *source* seconds: where in the file it
                // reads from, which a delete before it never shifts. How many
                // source frames that is, is the span's own arithmetic -- at a
                // speed it is not the timeline length ([`Span::source_len`]).
                Some((source, in_frame)) => (
                    Some(source),
                    f64::from(in_frame) / fps,
                    f64::from(in_frame + span.source_len()) / fps,
                ),
                // A gap has no file, so all it can say is how long it is.
                None => (None, 0.0, f64::from(span.len) / fps),
            })
            .collect()
    }

    /// One play list per audio lane that *holds* something, in display order --
    /// what the mixer sums ([`crate::AudioSession::open_mixed_streams`]) and
    /// what an export asks how many lanes it would have to mix.
    ///
    /// Never empty: a timeline whose audio lanes are all empty (or which has no
    /// audio lane at all) still yields one all-gap list, so it plays silence
    /// against a master clock exactly as it did before there were lanes to
    /// count. Lanes with nothing on them are left out rather than mixed in as
    /// silence: they would cost a decode thread each and add zero.
    pub fn audio_segments_from(
        &self,
        timeline_frame: u32,
        fps: f64,
    ) -> Vec<Vec<(Option<usize>, f64, f64)>> {
        self.audio_lanes()
            .into_iter()
            .map(|lane| self.lane_segments_from(lane, timeline_frame, fps))
            .collect()
    }

    /// *Which* lanes [`audio_segments_from`](Project::audio_segments_from)
    /// builds a play list for, in the same order. Public because everything
    /// asked per audio lane -- which clip a segment's equalizer belongs to,
    /// which lane an mp4 export would copy -- has to read the very list the
    /// sound comes off, or the two answers drift apart (which is exactly how an
    /// `A2`-only project once exported silently, ledger).
    pub fn audio_lanes(&self) -> Vec<Lane> {
        let lanes: Vec<Lane> = self
            .lanes()
            .into_iter()
            .filter(|l| l.kind == LaneKind::Audio && !self.lane(*l).is_empty())
            .collect();
        match lanes.is_empty() {
            // The all-gap fallback: `A1`'s own (empty) list, so a timeline with
            // no sound still plays silence against the master clock.
            true => vec![Lane::A1],
            false => lanes,
        }
    }

    /// The rate each of [`audio_segments_from`](Project::audio_segments_from)'s
    /// segments plays at, and how long the timeline gives it: the same lanes in
    /// the same order, one entry per segment, `None` for a segment at real time
    /// -- which is every segment of a project nobody has speeded, and the path
    /// the audio worker leaves bit-identical.
    ///
    /// A parallel list for [`audio_eqs_from`](Project::audio_eqs_from)'s reason:
    /// the segment tuple is what every opener and a dozen tests speak, and a
    /// rate is not part of *which samples* a segment names.
    ///
    /// The seconds are what makes a resample exact: a segment's own window
    /// resolves to whole source frames, and dividing that by the rate would
    /// leave each clip a fraction of a frame short or long -- a drift that grows
    /// clip by clip. Told how much timeline it owes, the worker pads or trims
    /// the last few samples of each segment instead, so the sound stays locked
    /// to the picture however many speeded cuts it walks through.
    pub fn audio_speeds_from(&self, timeline_frame: u32, fps: f64) -> Vec<Vec<Option<Stretch>>> {
        self.audio_lanes()
            .into_iter()
            .map(|lane| self.lane_speeds_from(lane, timeline_frame, fps))
            .collect()
    }

    /// [`audio_speeds_from`](Project::audio_speeds_from) for one lane, matching
    /// [`lane_segments_from`](Project::lane_segments_from) entry for entry --
    /// same walk, same `fps` refusal.
    pub fn lane_speeds_from(
        &self,
        lane: Lane,
        timeline_frame: u32,
        fps: f64,
    ) -> Vec<Option<Stretch>> {
        if !(fps.is_finite() && fps > 0.0) {
            return Vec::new();
        }
        self.spans_from(lane, timeline_frame)
            .iter()
            .map(|span| {
                (!span.speed.is_normal()).then(|| Stretch {
                    step: span.speed.as_f64(),
                    timeline_secs: f64::from(span.len) / fps,
                })
            })
            .collect()
    }

    /// The equalizer each of [`audio_segments_from`](Project::audio_segments_from)'s
    /// segments plays through: the same lanes in the same order, one entry per
    /// segment, `None` where the segment is a gap or its clip plays flat.
    ///
    /// A parallel list rather than a fourth element of the segment tuple: that
    /// tuple is what every opener, the packet-copy path and a dozen tests speak,
    /// and an effect is not part of *which samples* a segment names. A short
    /// list therefore means "flat from here on" rather than a length mismatch --
    /// see [`crate::AudioSession::open_mixed_streams_eq`].
    pub fn audio_eqs_from(&self, timeline_frame: u32, fps: f64) -> Vec<Vec<Option<EqParams>>> {
        self.audio_lanes()
            .into_iter()
            .map(|lane| self.lane_eqs_from(lane, timeline_frame, fps))
            .collect()
    }

    /// [`audio_eqs_from`](Project::audio_eqs_from) for one lane, matching
    /// [`lane_segments_from`](Project::lane_segments_from) entry for entry --
    /// same walk, same `fps` refusal, so the two lists cannot come out different
    /// lengths.
    pub fn lane_eqs_from(
        &self,
        lane: Lane,
        timeline_frame: u32,
        fps: f64,
    ) -> Vec<Option<EqParams>> {
        if !(fps.is_finite() && fps > 0.0) {
            return Vec::new();
        }
        self.spans_from(lane, timeline_frame)
            .iter()
            // A span off a clip covers that clip's frames and no others, so its
            // own start names the clip -- the index a [`Span`] does not carry.
            .map(|span| {
                self.map(lane, span.start)
                    .and_then(|(idx, _)| self.eq_of(lane, idx).cloned())
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

/// Which source frame a clip plays at `timeline_frame`: its in-point plus the
/// offset *at the clip's own rate*. The one place the two frame spaces meet, so
/// a speed cannot be applied twice or forgotten in one of them.
fn source_frame(c: &Clip, timeline_frame: u32) -> u32 {
    // Clamped inside the clip's own range: rounding at a slow rate can put the
    // last timeline frame of a clip a frame past its out-point, and a source
    // frame outside the range is one the next clip owns.
    (c.in_frame + c.speed.source_at(timeline_frame - c.start)).min(c.out_frame - 1)
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
    // ...and, on a speeded clip, one its rate can actually address
    // ([`Speed::split_at`]): a cut between two showings of one source frame
    // would leave halves that no longer add up to the clip that was cut.
    (frame > clips[idx].start)
        .then_some(idx)
        .filter(|&idx| split_source(&clips[idx], frame).is_some())
}

/// Where in the source a split of `c` at `frame` cuts, or `None` when the clip's
/// rate cannot address that frame.
fn split_source(c: &Clip, frame: u32) -> Option<u32> {
    c.speed
        .split_at(c.len(), frame - c.start)
        .map(|src| c.in_frame + src)
}

/// Index of the first of the two clips a [`Project::regroup`] at `frame` would
/// rejoin: they must touch there, and be consecutive frames of one source.
fn joinable(clips: &[Clip], frame: u32) -> Option<usize> {
    let idx = clips.iter().position(|c| c.end() == frame)?;
    let next = clips.get(idx + 1)?;
    let joined = clips[idx].len() + next.len();
    (next.start == frame
        && next.source == clips[idx].source
        && next.in_frame == clips[idx].out_frame
        // ...at one rate, and one that puts the rejoined clip back in exactly
        // the room the two took up: what a split could have produced, which is
        // all this undoes.
        && next.speed == clips[idx].speed
        && clips[idx].speed.frames(joined) == clips[idx].frames() + next.frames())
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

/// The same for a colour grade: [`crate::color`] itself falls back to identity
/// on a non-finite value, but a value that cannot be written and read back as
/// itself has no business reaching the model.
fn color_finite(p: &ColorParams) -> bool {
    p.brightness.is_finite()
        && p.contrast.is_finite()
        && p.saturation.is_finite()
        && p.tint.is_finite()
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
        // The head that survives in front of the hole -- as much source as fits
        // in the room in front of it at this clip's own rate ([`Speed::fit`]),
        // which at real time is that room itself. Never *more*: a head that
        // outgrew its room would reach into the hole it is being cut out of,
        // which is exactly the overlap a slow clip punched at a frame its rate
        // cannot address used to leave (at 0.25x a single source frame is four
        // timeline frames, and three frames of room hold none of it). `None`
        // there means no remainder at all, and the piece simply goes.
        if let Some(keep) = c.speed.fit(start.saturating_sub(c.start))
            && c.start < start
        {
            out.push(Clip {
                out_frame: c.in_frame + keep.min(c.len()),
                link: None,
                ..c
            });
        }
        // ...and the tail behind it, which keeps reading up to where it would
        // have: measured from its out-point for the head's reason, so what it
        // occupies still ends where the whole clip did -- and dropped whole on
        // the same `None`.
        if let Some(keep) = c.speed.fit(c.end().saturating_sub(end))
            && c.end() > end
        {
            out.push(Clip {
                start: end,
                in_frame: c.out_frame - keep.min(c.len()),
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
        // The cut in *source* frames, at the clip's own rate. `splittable` has
        // already refused a frame the rate cannot address, which is why
        // [`Project::paste`] refuses one too rather than opening room inside a
        // clip that cannot be cut there.
        tail.in_frame = split_source(&tail, at).expect("splittable said so");
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
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
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

    /// A recognisable colour grade, `n` telling two of them apart -- [`band_at`]
    /// for the colour table.
    fn grade_at(n: u32) -> ColorParams {
        ColorParams {
            brightness: f64::from(n) as f32 * 0.05,
            ..Default::default()
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

    /// The music video's own path: the take comes apart into halves that carry
    /// ids of their own, one undo puts it back, and two clips over the same
    /// frames become one take again by hand.
    #[test]
    fn a_take_comes_apart_and_goes_back_together() {
        let mut p = Project::single(FILE, 9);
        let one = p.clips()[0].link.expect("a fresh project is one take");
        assert_eq!(p.lane(Lane::A1)[0].link, Some(one));

        // A third lane in the same group -- and the merge case with it: a
        // placement is in no group, and grouping it in leaves *one* group.
        let v2 = p.add_lane(LaneKind::Video);
        assert!(p.place(v2, 0, clip(0, 0, 9, 0)));
        assert!(p.lane(v2)[0].link.is_none(), "a placement joins no group");
        p.group(Lane::V1, 0, v2, 0)
            .expect("the same frames, one lane over");
        let id = p.clips()[0].link.expect("still a group");
        assert!(
            p.lanes().into_iter().all(|l| p.lane(l)[0].link == Some(id)),
            "one id over the three, not two groups sharing a span"
        );
        links_are_consistent(&p.lanes).expect("one id, one span");

        // Detached by the sound: every half of the group, not only the two.
        assert!(p.ungroup(Lane::A1, 0));
        let ids: Vec<Option<u32>> = p.lanes().into_iter().map(|l| p.lane(l)[0].link).collect();
        assert!(
            ids.iter().all(Option::is_some),
            "a half keeps an id of its own"
        );
        assert!(
            ids.iter()
                .enumerate()
                .all(|(i, a)| ids[..i].iter().all(|b| a != b)),
            "and no two halves are the same group any more: {ids:?}"
        );
        links_are_consistent(&p.lanes).expect("a lone id is legal");

        // One snapshot for the whole detach.
        assert!(p.undo());
        assert!(
            p.lanes().into_iter().all(|l| p.lane(l)[0].link == Some(id)),
            "one undo puts the whole group back"
        );

        // And back together by hand: the two the pointer named, and only them.
        assert!(p.ungroup(Lane::V1, 0));
        p.group(Lane::V1, 0, Lane::A1, 0)
            .expect("both cover [0, 9)");
        assert_eq!(p.lane(Lane::V1)[0].link, p.lane(Lane::A1)[0].link);
        assert_ne!(
            p.lane(v2)[0].link,
            p.lane(Lane::V1)[0].link,
            "the third half stayed detached"
        );
        links_are_consistent(&p.lanes).expect("one id, one span");
    }

    /// What a group may not be, and what a detach has nothing to take apart --
    /// none of which may cost an undo step.
    #[test]
    fn a_group_is_one_span_and_one_clip_per_lane() {
        let mut p = three();
        let before = shape(&p);
        let why = |r: crate::Result<()>| r.expect_err("refused").to_string();

        assert!(
            why(p.group(Lane::V1, 0, Lane::V1, 1)).contains("one clip per lane"),
            "two clips of one lane are never one take"
        );
        assert!(why(p.group(Lane::V1, 0, Lane::A1, 9)).contains("there is no clip 9 on A1"));
        let spans = why(p.group(Lane::V1, 0, Lane::A1, 1));
        assert!(
            spans.contains("V1 covers [0, 3) and A1 covers [3, 5)"),
            "{spans}"
        );
        assert!(
            spans.contains("trim them to matching bounds first"),
            "{spans}"
        );
        assert!(why(p.group(Lane::V1, 0, Lane::A1, 0)).contains("one take already"));

        // A half nothing else is grouped with is already detached, a placement
        // is in no group at all, and an index that is not there has nothing to
        // detach either.
        assert!(p.place(Lane::V1, 20, clip(20, 0, 3, 0)));
        assert!(!p.ungroup(Lane::V1, 3), "a placement is in no group");
        assert!(p.lift(Lane::A1, 0));
        assert!(!p.ungroup(Lane::V1, 0), "its group has no other half");
        assert!(!p.ungroup(Lane::V1, 9), "no clip there");

        // Two undos, one per edit: not one of the refusals pushed a snapshot.
        assert!(p.undo());
        assert!(p.undo());
        assert_eq!(shape(&p), before);
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
                from: Some((0, 6)),
                speed: Speed::NORMAL
            })
        );
        assert_eq!(p.span_at(Lane::V1, 9), None);
    }

    /// The fit policy is one byte *in* the clip rather than an index into a
    /// table like the eq and the grade: there is nothing for a table to share.
    /// It costs the clip nothing at all -- the fields before it (a `usize`
    /// source and two `Option<u16>`s) already left the struct padded to 40
    /// bytes, and the byte landed in that padding. This is the assert that says
    /// so: a clip that grew a word grew every undo snapshot and every clipboard
    /// copy with it.
    #[test]
    fn a_fit_policy_costs_the_clip_no_word() {
        assert_eq!(
            std::mem::size_of::<Clip>(),
            40,
            "Clip changed size: {} bytes",
            std::mem::size_of::<Clip>()
        );
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
        color: None,
        fit: FitPolicy::Fit,
        speed: Speed::NORMAL,
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

    /// The data-loss paste: a clip of a source with no picture used to land on
    /// `V1` as well as `A1`, where it decodes to nothing -- and the save that
    /// carried it was refused on the way back in ("a box with a larger size").
    #[test]
    fn a_paste_of_a_source_with_no_picture_stays_off_the_video_lane() {
        let mut p = three();
        let wav = p.import("/nonexistent/song.wav", 0);
        let copied = Clip {
            source: wav,
            ..PASTED
        };
        assert!(p.paste(3, copied));
        // The room is opened on every lane, or the two would slide apart...
        assert_eq!(p.timeline_frames(), 11);
        assert_eq!(
            shape(&p)[0],
            vec![clip(0, 0, 3, 0), clip(5, 3, 5, 0), clip(7, 5, 9, 0)],
            "the video lane got room and nothing else"
        );
        // ...but the clip itself only reaches the lane that can play it.
        assert_eq!(
            shape(&p)[1],
            vec![
                clip(0, 0, 3, 0),
                clip(3, 100, 102, wav),
                clip(5, 3, 5, 0),
                clip(7, 5, 9, 0)
            ]
        );
        // A source with a picture is unchanged: still the grouped pair.
        let mut p = three();
        assert!(p.paste(3, PASTED));
        assert_eq!(shape(&p)[0], shape(&p)[1]);
    }

    /// A hand-written file may hold a clip that ends at the very last frame
    /// (`edith::check` permits `start + len == u32::MAX`), and the ripple used
    /// to wrap its start round to the front of the lane: an overlap in debug, a
    /// project no reload would accept in release.
    #[test]
    fn a_paste_that_would_run_past_the_last_frame_is_refused() {
        let far = clip(u32::MAX - 90, 0, 90, 0);
        let parts = |c: Clip| {
            Project::from_parts(
                vec![Source::new(FILE, 0)],
                two(vec![c], vec![c]),
                Vec::new(),
                Vec::new(),
            )
            .expect("valid parts")
        };
        let mut p = parts(far);
        assert!(!p.paste(0, PASTED), "no room for the two frames it adds");
        assert_eq!(shape(&p)[0], vec![far], "a refusal changes nothing");
        assert!(!p.undo(), "...and pushes no history either");
        assert!(
            !p.paste(u32::MAX, PASTED),
            "appending has nowhere to go either: the timeline ends at the last frame"
        );
        // With room for exactly those frames the same paste is taken.
        let mut p = parts(clip(u32::MAX - 92, 0, 90, 0));
        assert!(p.paste(0, PASTED));
        assert_eq!(p.lane(Lane::V1)[1].end(), u32::MAX);
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
        assert!(p.delete(0), "and the last remaining clip goes too");
        assert_eq!(p.clips().len(), 0);
        assert_eq!(p.timeline_frames(), 0, "the timeline is emptiable");
        assert!(!p.delete(0), "there is nothing left to delete");
        assert!(p.undo(), "one gesture, one undo");
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
                from: None,
                speed: Speed::NORMAL
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
                from: None,
                speed: Speed::NORMAL
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

    /// The timeline is emptiable: the last placement comes off like any other,
    /// the project holds "nothing on any lane" as a state, and one undo per
    /// gesture brings it back.
    #[test]
    fn the_last_clip_of_the_last_lane_lifts_and_undoes() {
        let mut p = Project::single(FILE, 9);
        assert!(p.lift(Lane::A1, 0), "a silent timeline is fine");
        assert!(p.lift(Lane::V1, 0), "and an empty one is a timeline too");
        assert_eq!(p.timeline_frames(), 0);
        assert_eq!(p.composite_span_at(0), None, "nothing to show");
        assert!(p.audio_segments_from(0, FPS).len() == 1, "one all-gap list");
        assert!(!p.lift(Lane::A1, 0), "index past the end");
        assert!(p.undo(), "and the last lift comes back");
        assert_eq!(p.lane_spans(Lane::V1), vec![(0, 9)]);
        // A saved-and-loaded empty timeline is a project like any other, and it
        // still names the file its frame rate came from.
        assert!(p.lift(Lane::V1, 0));
        let (sources, lanes, eq, color) = p.without_orphan_sources();
        assert_eq!(sources.len(), 1, "source 0 survives an emptied timeline");
        let back = Project::from_parts(sources, lanes, eq, color).expect("an empty project loads");
        assert_eq!(back.timeline_frames(), 0);
        assert_eq!(back.lanes().len(), 2, "and it kept its lanes");
    }

    /// What a keyboard selection walks: one answer per lane, gaps included --
    /// the composite's own answer is one lane's and cannot say what the audio
    /// lane holds under the same playhead.
    #[test]
    fn lane_clip_at_answers_for_each_lane_separately() {
        let mut p = Project::single(FILE, 9);
        assert!(p.split(3), "both lanes split");
        assert_eq!(p.lane_clip_at(Lane::V1, 0), Some(0));
        assert_eq!(p.lane_clip_at(Lane::V1, 3), Some(1));
        assert_eq!(p.lane_clip_at(Lane::A1, 3), Some(1));
        assert_eq!(p.lane_clip_at(Lane::V1, 9), None, "past the end");
        // A gap on one lane leaves the other's clip selectable there.
        assert!(p.lift(Lane::A1, 0));
        assert_eq!(p.lane_clip_at(Lane::A1, 0), None, "the gap holds nothing");
        assert_eq!(p.lane_clip_at(Lane::A1, 3), Some(0), "indices moved with it");
        assert_eq!(p.lane_clip_at(Lane::V1, 0), Some(0));
        // A lane that is not there is not a panic.
        assert_eq!(p.lane_clip_at(Lane::new(LaneKind::Video, 1), 0), None);
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
                from: None,
                speed: Speed::NORMAL
            })
        );
        assert!(!p.place(Lane::V1, 0, clip(0, 7, 7, 0)), "empty clip");
    }

    /// The drag between tracks: the clip keeps its frames, one undo takes it
    /// back, and every way it can be refused leaves the project untouched.
    #[test]
    fn move_to_lane_keeps_the_frames_and_refuses_the_rest() {
        let v2 = Lane::new(LaneKind::Video, 1);
        let mut p = three();
        assert_eq!(p.add_lane(LaneKind::Video), v2);
        let before = shape(&p);

        assert!(p.move_to_lane(Lane::V1, 1, v2), "V1's middle clip moves up");
        assert_eq!(shape(&p)[0], vec![clip(0, 0, 3, 0), clip(5, 5, 9, 0)]);
        assert_eq!(shape(&p)[2], vec![clip(3, 3, 5, 0)], "same frames on V2");
        assert_eq!(shape(&p)[1], before[1], "the audio lane is untouched");
        assert_eq!(p.timeline_frames(), 9, "a move is not an insert");
        // One snapshot: a single undo, and the lane list survives it.
        assert!(p.undo());
        assert_eq!(shape(&p), before);

        // A lane that is not there, the lane it is already on, an index that is
        // not there, and a move across kinds: all refused, nothing changed.
        let history = p.history.len();
        for (from, idx, to) in [
            (Lane::V1, 1, Lane::new(LaneKind::Video, 7)),
            (Lane::V1, 1, Lane::V1),
            (Lane::V1, 9, v2),
            (Lane::V1, 1, Lane::A1),
            (Lane::A1, 1, v2),
        ] {
            assert!(!p.move_to_lane(from, idx, to), "{from:?} {idx} -> {to:?}");
        }
        assert_eq!(shape(&p), before);
        assert_eq!(p.history.len(), history, "a refusal snapshots nothing");

        // Landing on another clip is refused rather than overwriting it: the
        // pointer named the lane, never the take already sitting there.
        assert!(p.place(v2, 3, clip(0, 100, 101, 0)), "V2 holds [3,4)");
        assert!(!p.move_to_lane(Lane::V1, 1, v2), "[3,5) would land on it");
        assert!(p.move_to_lane(Lane::V1, 0, v2), "[0,3) merely abuts it");
        assert_eq!(shape(&p)[0], vec![clip(3, 3, 5, 0), clip(5, 5, 9, 0)]);
        assert_eq!(shape(&p)[2], vec![clip(0, 0, 3, 0), clip(3, 100, 101, 0)]);
    }

    /// How long `FILE` is, which is what a trim's tail is allowed to reach:
    /// [`three`] cuts up all nine of its frames.
    const SRC: &[u32] = &[9];

    /// The drag on a clip's tail: it plays less (or more) of its source, nothing
    /// else on the lane moves, and every wall stops the edge rather than
    /// refusing the gesture.
    #[test]
    fn trimming_the_tail_stops_at_every_wall() {
        let mut p = three();

        // Pulled in: a gap opens behind it and the clip after it stays put.
        assert!(p.trim(Lane::V1, 1, Edge::End, 4, SRC));
        assert_eq!(
            shape(&p)[0],
            vec![clip(0, 0, 3, 0), clip(3, 3, 4, 0), clip(5, 5, 9, 0)]
        );
        assert_eq!(p.timeline_frames(), 9, "a trim ripples nothing");
        assert_eq!(p.map(Lane::V1, 4), None, "the frame it gave up is a gap");

        // ...and back out, no further than the clip behind it.
        assert!(p.trim(Lane::V1, 1, Edge::End, 8, SRC));
        assert_eq!(shape(&p)[0][1], clip(3, 3, 5, 0), "stopped at the neighbour");
        assert!(!p.trim(Lane::V1, 1, Edge::End, 8, SRC), "already at the wall");

        // One frame of clip always survives, however far back the pointer went.
        assert!(p.trim(Lane::V1, 1, Edge::End, 0, SRC));
        assert_eq!(shape(&p)[0][1], clip(3, 3, 4, 0));

        // The last clip has no neighbour, so what stops it is the file: nine
        // frames is nine frames.
        assert!(p.trim(Lane::V1, 2, Edge::End, 7, SRC));
        assert!(p.trim(Lane::V1, 2, Edge::End, 9999, SRC));
        assert_eq!(shape(&p)[0][2], clip(5, 5, 9, 0), "back to the whole take");
        assert!(!p.trim(Lane::V1, 2, Edge::End, 9999, SRC), "and no further");
        // A source nobody told us the length of may not grow at all -- an
        // out-point past the end of a file is a save that will not open again.
        assert!(p.trim(Lane::V1, 2, Edge::End, 7, &[]));
        assert!(!p.trim(Lane::V1, 2, Edge::End, 9, &[]), "length unknown");
    }

    /// The head pulled in takes the in-point with it -- what stays plays what it
    /// always played -- and stops at the source's own first frame.
    #[test]
    fn trimming_the_head_moves_the_in_point() {
        let mut p = three();
        assert!(p.trim(Lane::V1, 0, Edge::Start, 2, SRC));
        assert_eq!(shape(&p)[0][0], clip(2, 2, 3, 0), "two frames off the front");
        assert_eq!(p.map(Lane::V1, 2), Some((0, 2)), "frame 2 still plays 2");
        assert_eq!(p.map(Lane::V1, 1), None, "and the front is a gap");
        assert!(p.trim(Lane::V1, 0, Edge::Start, 0, SRC), "back out again");
        assert_eq!(shape(&p)[0][0], clip(0, 0, 3, 0));
        // One frame survives here too.
        assert!(p.trim(Lane::V1, 0, Edge::Start, 9, SRC));
        assert_eq!(shape(&p)[0][0], clip(2, 2, 3, 0));

        // A clip that does not begin at its source's first frame: its head goes
        // back to source frame 0 and not one frame further.
        let mut p = Project::single(FILE, 9);
        assert!(p.place(Lane::V1, 5, clip(0, 1, 4, 0)));
        assert!(p.lift(Lane::V1, 0), "nothing in front of it to stop it");
        assert!(p.trim(Lane::V1, 0, Edge::Start, 0, SRC));
        assert_eq!(shape(&p)[0][0], clip(4, 0, 4, 0), "one frame of head left");
    }

    /// A clip whose in-point is *past* its own start -- what a ripple delete
    /// leaves, the piece in front of it gone and this one slid back to frame 0
    /// carrying the in-point the cut gave it. Its head has more source behind it
    /// than the timeline has room for, and the timeline's own first frame is the
    /// wall: asking is not an overflow, and the answer is 0.
    #[test]
    fn a_ripple_closed_clip_can_still_be_head_trimmed() {
        let mut p = Project::single(FILE, 9);
        assert!(p.split(5));
        assert!(p.delete_in(Lane::V1, 0), "the first piece goes");
        assert_eq!(shape(&p)[0], vec![clip(0, 5, 9, 0)], "start 0, in-point 5");

        assert_eq!(
            p.trim_room(Lane::V1, 0, Edge::Start, SRC),
            Some((0, 3)),
            "back to frame 0 at most, and one frame of clip always survives"
        );
        assert!(!p.trim(Lane::V1, 0, Edge::Start, 0, SRC), "already there");
        assert!(p.trim(Lane::V1, 0, Edge::Start, 2, SRC));
        assert_eq!(shape(&p)[0], vec![clip(2, 7, 9, 0)], "the in-point followed");
        assert!(p.trim(Lane::V1, 0, Edge::Start, 0, SRC), "and back out");
        assert_eq!(shape(&p)[0], vec![clip(0, 5, 9, 0)]);
    }

    /// Linked halves trim as one: a link is one span on however many lanes, so
    /// the sound follows the picture's edge -- and the tightest wall of the two
    /// is what stops both.
    #[test]
    fn linked_halves_trim_together() {
        let mut p = three();
        let link = p.lane(Lane::V1)[2].link.expect("a split hands out ids");
        assert_eq!(p.lane(Lane::A1)[2].link, Some(link), "both halves grouped");

        assert!(p.trim(Lane::V1, 2, Edge::Start, 7, SRC));
        assert_eq!(shape(&p)[0][2], clip(7, 7, 9, 0));
        assert_eq!(shape(&p)[1][2], clip(7, 7, 9, 0), "the sound followed");
        assert!(p.trim(Lane::A1, 2, Edge::End, 8, SRC), "either half drags it");
        assert_eq!(shape(&p)[0][2], clip(7, 7, 8, 0), "and the picture follows");
        links_are_consistent(&p.lanes).expect("one id per lane, one span");

        // Something in the way on the *audio* lane stops the picture's tail as
        // well: the group can only go as far as its tightest member.
        assert!(p.place(Lane::A1, 9, clip(0, 0, 1, 0)));
        assert!(p.trim(Lane::V1, 2, Edge::End, 9999, SRC));
        assert_eq!(shape(&p)[0][2], clip(7, 7, 9, 0), "stopped by A1's clip");
        assert_eq!(shape(&p)[1][2], clip(7, 7, 9, 0));
        links_are_consistent(&p.lanes).expect("still one span");
    }

    /// A whole drag is one undo step -- the front-end commits once, at the
    /// release -- and an edge asked to stay where it is costs none at all.
    #[test]
    fn a_trim_is_one_undo_step() {
        let mut p = three();
        let before = shape(&p);
        let history = p.history.len();

        assert!(p.trim(Lane::V1, 1, Edge::End, 4, SRC));
        assert_eq!(p.history.len(), history + 1, "one snapshot per gesture");
        assert!(!p.trim(Lane::V1, 1, Edge::End, 4, SRC), "already there");
        assert!(!p.trim(Lane::V1, 9, Edge::End, 4, SRC), "no such clip");
        assert!(!p.trim(Lane::new(LaneKind::Video, 7), 0, Edge::End, 4, SRC));
        assert_eq!(p.history.len(), history + 1, "a refusal snapshots nothing");

        assert!(p.undo());
        assert_eq!(shape(&p), before, "both halves back, in one step");
    }

    /// A moved half stays in its group: a link names a span, not a lane, so the
    /// picture on `V2` is still the same take as the sound under it on `A1`.
    #[test]
    fn move_to_lane_keeps_the_group() {
        let v2 = Lane::new(LaneKind::Video, 1);
        let mut p = three();
        p.add_lane(LaneKind::Video);
        let link = p.lane(Lane::V1)[1].link.expect("a split hands out ids");
        assert_eq!(p.lane(Lane::A1)[1].link, Some(link), "both halves grouped");
        assert!(p.move_to_lane(Lane::V1, 1, v2));
        assert_eq!(p.lane(v2)[0].link, Some(link), "the id travelled with it");
        links_are_consistent(&p.lanes).expect("one id per lane, one span");
        assert_eq!(
            p.lane(v2)[0].start,
            p.lane(Lane::A1)[1].start,
            "and still covers the same span as its sound"
        );
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
        assert!(p.undo());
        assert_eq!(shape(&p), shape(&three()));
        // One that takes everything is a delete like any other: the timeline
        // empties, and the undo it cost brings it back whole.
        assert!(p.ripple_delete(0, 100), "the timeline is emptiable");
        assert_eq!(p.timeline_frames(), 0);
        assert!(
            !p.ripple_delete(0, 100),
            "and a delete with nothing left to take is not an undo step"
        );
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
        let (sources, lanes, eq, color) = p.without_orphan_sources();
        assert_eq!(sources, vec![Source::new(FILE, 0)], "the orphan is gone");
        assert!(eq.is_empty(), "and a project with no equalizer writes none");
        assert!(color.is_empty(), "nor a colour it never graded with");
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
        let (sources, lanes, eq, color) = p.without_orphan_sources();
        assert_eq!(
            sources,
            vec![Source::new(FILE, 0), Source::new("/nonexistent/c.mp4", 0)]
        );
        assert_eq!(
            lanes[0].1.iter().map(|c| c.source).collect::<Vec<_>>(),
            [0, 1]
        );
        // ...and what comes out is loadable, with the same timeline.
        let reloaded = Project::from_parts(sources, lanes, eq, color).expect("from_parts");
        assert_eq!(reloaded.timeline_frames(), p.timeline_frames());
        assert_eq!(reloaded.sources().len(), 2);
    }

    /// Taking a file out of the library: refused by name -- lanes and counts --
    /// while a clip still plays from it, and once nothing does, the entry goes
    /// and every clip that indexed past it is renumbered onto the short list.
    #[test]
    fn remove_source_refuses_what_plays_and_renumbers_what_is_left() {
        // Sources 0 (FILE, three clips) and 1 (FILE2, one take on both lanes),
        // plus a second stream of FILE as source 2 with a take of its own.
        let mut p = two_sources();
        assert_eq!(p.import(FILE, 1), 2, "another stream is another source");
        assert!(p.append_clip(2, 4));

        let refusal = p
            .remove_source(1)
            .expect_err("a source with clips on it cannot go")
            .to_string();
        assert!(refusal.contains(FILE2), "names the file: {refusal}");
        assert!(
            refusal.contains("V1 (1 clip)") && refusal.contains("A1 (1 clip)"),
            "names the lanes holding it: {refusal}"
        );
        assert!(refusal.contains("still plays"), "{refusal}");
        assert_eq!(p.sources().len(), 3, "a refusal changes nothing");
        assert_eq!(shape(&p)[0][3].source, 1);

        // Delete FILE2's take -- the whole span, on every lane -- and the entry
        // is free to go. Source 2's clips become source 1's.
        assert!(p.delete_in(Lane::V1, 3));
        assert!(!p.clips().iter().any(|c| c.source == 1), "nothing plays it");
        p.remove_source(1).expect("nothing plays FILE2 any more");
        assert_eq!(
            p.sources(),
            [Source::new(FILE, 0), Source::new(FILE, 1)],
            "the middle entry is gone"
        );
        assert_eq!(
            shape(&p)[0].iter().map(|c| c.source).collect::<Vec<_>>(),
            [0, 0, 0, 1],
            "the clips past it renumbered"
        );
        assert_eq!(
            shape(&p)[1].iter().map(|c| c.source).collect::<Vec<_>>(),
            [0, 0, 0, 1],
            "on every lane"
        );
        // What is left is a project that still loads, which is the whole point
        // of renumbering rather than leaving a hole.
        let (sources, lanes, eq, color) = p.without_orphan_sources();
        Project::from_parts(sources, lanes, eq, color).expect("from_parts");
    }

    /// The two edges of a removal: it retires the undo stack (the ponytail on
    /// [`Project::remove_source`]), and the last entry standing cannot go --
    /// source 0 is where a reopened project reads its frame rate.
    #[test]
    fn remove_source_retires_undo_and_keeps_the_last_file() {
        let mut p = two_sources();
        assert!(p.delete_in(Lane::V1, 3), "FILE2's take goes first");
        assert!(!p.history.is_empty(), "there is something to undo");
        p.remove_source(1).expect("nothing plays FILE2");
        assert!(
            p.history.is_empty(),
            "a snapshot naming the removed source must not survive it"
        );
        assert!(!p.undo(), "and so there is nothing left to undo");

        // The last file standing stays, whatever plays from it:
        // `PlaybackSession::open_project` refuses a project that names no
        // sources at all, and the timeline's frame rate lives in source 0.
        assert_eq!(p.sources().len(), 1);
        let refusal = p
            .remove_source(0)
            .expect_err("the only source must stay")
            .to_string();
        assert!(refusal.contains("only file"), "{refusal}");
        assert_eq!(p.sources().len(), 1);
        assert!(
            p.remove_source(1).is_err(),
            "and an index that is not there is refused, not panicked on"
        );
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

        let (_, lanes, eq, _) = p.without_orphan_sources();
        assert_eq!(eq, vec![band_at(1)], "what nothing plays is not written");
        assert_eq!(lanes[0].1[0].eq, Some(0), "and the survivor renumbers");
        assert_eq!(lanes[0].1[1].eq, None);

        // One curve on three clips is one entry: settings that are equal share.
        assert!(p.set_eq(Lane::V1, 1, Some(band_at(1))));
        assert!(p.set_eq(Lane::A1, 2, Some(band_at(1))));
        let (_, lanes, eq, _) = p.without_orphan_sources();
        assert_eq!(eq.len(), 1, "equal settings share their entry");
        assert_eq!(lanes[1].1[2].eq, Some(0));
        reloads(&p, "an equalizer that outlived an undo");
    }

    /// [`an_equalizer_follows_the_clip_through_split_copy_and_undo`]'s twin: a
    /// grade is the clip's, through every edit that copies a placement.
    #[test]
    fn a_colour_follows_the_clip_through_split_copy_and_undo() {
        let mut p = three();
        assert!(
            p.color_of(Lane::V1, 0).is_none(),
            "a fresh clip is ungraded"
        );
        assert!(p.set_color(Lane::V1, 0, Some(grade_at(2))));
        assert_eq!(p.color_of(Lane::V1, 0), Some(&grade_at(2)));
        assert!(
            p.color_of(Lane::A1, 0).is_none(),
            "one clip, not the lane below"
        );
        assert!(
            p.eq_of(Lane::V1, 0).is_none(),
            "and a grade is not an equalizer"
        );

        // Cutting a graded clip must not grade half of it: both halves inherit.
        assert!(p.split(1));
        assert_eq!(p.color_of(Lane::V1, 0), Some(&grade_at(2)));
        assert_eq!(p.color_of(Lane::V1, 1), Some(&grade_at(2)));
        assert!(
            p.color_of(Lane::V1, 2).is_none(),
            "the clip after it is ungraded"
        );

        // A clipboard copy carries it -- onto another lane, at that.
        let copied = p.lane(Lane::V1)[1];
        assert!(p.place(Lane::A1, 20, copied));
        assert_eq!(p.color_of(Lane::A1, 4), Some(&grade_at(2)));
        assert!(p.undo());
        assert_eq!(p.lane(Lane::A1).len(), 4, "the placement came back off");

        // ...and a change to one is one undo step, in both directions.
        assert!(p.set_color(Lane::V1, 0, Some(grade_at(5))));
        assert_eq!(p.color_of(Lane::V1, 0), Some(&grade_at(5)));
        assert!(p.undo());
        assert_eq!(p.color_of(Lane::V1, 0), Some(&grade_at(2)));
        assert!(
            p.set_color(Lane::V1, 0, None),
            "and taking it off is an edit"
        );
        assert!(p.color_of(Lane::V1, 0).is_none());
        assert!(p.undo());
        assert_eq!(p.color_of(Lane::V1, 0), Some(&grade_at(2)));

        // Two refusals, neither of which costs an undo step.
        let nan = ColorParams {
            contrast: f32::NAN,
            ..grade_at(1)
        };
        assert!(
            !p.set_color(Lane::V1, 99, Some(grade_at(1))),
            "no clip there"
        );
        assert!(!p.set_color(Lane::V1, 0, Some(nan)), "not a finite value");
        assert_eq!(p.color_of(Lane::V1, 0), Some(&grade_at(2)));
        assert!(p.undo());
        assert_eq!(
            p.lane(Lane::V1).len(),
            3,
            "one step back is before the split"
        );
        assert!(p.undo());
        assert!(
            p.color_of(Lane::V1, 0).is_none(),
            "and the next is before the grade: the refusals pushed none"
        );
    }

    /// A slider drag is one gesture: the press snapshots, every sample after it
    /// only regrades, and the single undo lands on what the clip was *before*
    /// the hand touched it -- not one step back down the drag.
    #[test]
    fn a_whole_colour_drag_undoes_in_one_step() {
        let mut p = three();
        assert!(p.set_color(Lane::V1, 0, Some(grade_at(1))), "the press");
        for n in 2..=8 {
            assert!(p.set_color_live(Lane::V1, 0, Some(grade_at(n))), "a sample");
        }
        assert_eq!(p.color_of(Lane::V1, 0), Some(&grade_at(8)));
        assert!(p.undo());
        assert!(
            p.color_of(Lane::V1, 0).is_none(),
            "one undo is the whole gesture, back to ungraded"
        );

        // The live write refuses what the snapshotting one refuses, and a
        // refusal still costs no history either way.
        assert!(!p.set_color_live(Lane::V1, 99, Some(grade_at(1))));
        assert!(!p.set_color_live(
            Lane::V1,
            0,
            Some(ColorParams {
                contrast: f32::NAN,
                ..grade_at(1)
            })
        ));
        assert!(p.undo());
        assert_eq!(
            p.lane(Lane::V1).len(),
            2,
            "the next step back is the split before the drag: no sample pushed one"
        );
    }

    /// The colour table is append-only within a session, exactly as the eq one
    /// is -- and a save is where an undone grade goes.
    #[test]
    fn an_undone_colour_is_pruned_when_the_project_is_saved() {
        let mut p = three();
        assert!(p.set_color(Lane::V1, 0, Some(grade_at(1))));
        assert!(p.set_color(Lane::V1, 0, Some(grade_at(2))));
        assert!(p.undo(), "back to the first grade");
        assert_eq!(p.color_of(Lane::V1, 0), Some(&grade_at(1)));

        let (_, lanes, _, color) = p.without_orphan_sources();
        assert_eq!(
            color,
            vec![grade_at(1)],
            "what nothing plays is not written"
        );
        assert_eq!(lanes[0].1[0].color, Some(0), "and the survivor renumbers");
        assert_eq!(lanes[0].1[1].color, None);

        // One grade on three clips is one entry: settings that are equal share.
        assert!(p.set_color(Lane::V1, 1, Some(grade_at(1))));
        assert!(p.set_color(Lane::A1, 2, Some(grade_at(1))));
        let (_, lanes, _, color) = p.without_orphan_sources();
        assert_eq!(color.len(), 1, "equal grades share their entry");
        assert_eq!(lanes[1].1[2].color, Some(0));
        reloads(&p, "a colour that outlived an undo");

        // An equalizer and a grade on one clip are two independent tables.
        assert!(p.set_eq(Lane::V1, 0, Some(band_at(3))));
        let (_, lanes, eq, color) = p.without_orphan_sources();
        assert_eq!((eq.len(), color.len()), (1, 1));
        assert_eq!((lanes[0].1[0].eq, lanes[0].1[0].color), (Some(0), Some(0)));
    }

    /// Not a claim about correctness but about cost: a clip is copied on every
    /// paste, every undo snapshot and every lane clone, so its size is worth
    /// knowing. The eq index fit the padding a `Copy` clip already had; the
    /// colour one did not, and the struct grew a word.
    #[test]
    fn a_clip_is_still_a_small_copy() {
        assert_eq!(
            std::mem::size_of::<Clip>(),
            40,
            "Clip changed size -- 32 before the colour index, 40 after"
        );
    }

    #[test]
    fn from_parts_has_no_history_and_checks_the_invariants() {
        let (sources, lanes, eq, color) = three().without_orphan_sources();
        let (video, audio) = (lanes[0].1.clone(), lanes[1].1.clone());
        let mut p = Project::from_parts(sources.clone(), lanes.clone(), eq.clone(), color.clone())
            .expect("valid parts");
        assert_eq!(p.clips(), three().clips());
        assert!(!p.undo(), "a loaded project has nothing to undo");
        assert!(p.split(4), "...and is editable from there");
        assert!(p.undo());

        // A lane may be empty, and so may every lane -- an emptied timeline is a
        // project. No lane at all is not one: there would be nothing to place on.
        assert!(
            Project::from_parts(
                sources.clone(),
                two(video.clone(), Vec::new()),
                Vec::new(),
                Vec::new(),
            )
            .is_ok()
        );
        assert!(
            Project::from_parts(
                sources.clone(),
                two(Vec::new(), Vec::new()),
                Vec::new(),
                Vec::new(),
            )
            .is_ok()
        );
        assert!(Project::from_parts(sources.clone(), Vec::new(), Vec::new(), Vec::new()).is_err());
        let bad: [Vec<Clip>; 5] = [
            vec![clip(0, 0, 3, 1)],                   // source that is not there
            vec![clip(0, 3, 3, 0)],                   // empty clip
            vec![clip(3, 0, 3, 0), clip(0, 3, 5, 0)], // out of order
            vec![clip(0, 0, 5, 0), clip(3, 3, 5, 0)], // overlapping
            vec![clip(u32::MAX - 1, 0, 3, 0)],        // end past the last frame
        ];
        for clips in bad {
            assert!(
                Project::from_parts(
                    sources.clone(),
                    two(clips.clone(), Vec::new()),
                    Vec::new(),
                    Vec::new(),
                )
                .is_err(),
                "{clips:?}"
            );
            assert!(
                Project::from_parts(
                    sources.clone(),
                    two(video.clone(), clips.clone()),
                    Vec::new(),
                    Vec::new(),
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
                    Vec::new(),
                    Vec::new(),
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
        assert!(
            Project::from_parts(
                sources.clone(),
                two(eqd(0), Vec::new()),
                Vec::new(),
                Vec::new(),
            )
            .is_err()
        );
        let mut nan = band_at(1);
        nan.bands[0].freq_hz = f32::NAN;
        assert!(
            Project::from_parts(
                sources.clone(),
                two(eqd(0), Vec::new()),
                vec![nan],
                Vec::new(),
            )
            .is_err()
        );
        let loaded = Project::from_parts(
            sources.clone(),
            two(eqd(0), Vec::new()),
            vec![band_at(1)],
            Vec::new(),
        )
        .expect("an eq the table holds");
        assert_eq!(loaded.eq_of(Lane::V1, 0), Some(&band_at(1)));

        // The same two refusals for a colour, at the same door.
        let graded = |i: u16| {
            vec![Clip {
                color: Some(i),
                fit: FitPolicy::default(),
                speed: Speed::NORMAL,
                ..video[0]
            }]
        };
        assert!(
            Project::from_parts(
                sources.clone(),
                two(graded(0), Vec::new()),
                Vec::new(),
                Vec::new(),
            )
            .is_err(),
            "a colour index no entry answers"
        );
        assert!(
            Project::from_parts(
                sources.clone(),
                two(graded(0), Vec::new()),
                Vec::new(),
                vec![ColorParams {
                    tint: f32::NAN,
                    ..grade_at(1)
                }],
            )
            .is_err(),
            "a value the file format could not have written"
        );
        let loaded = Project::from_parts(
            sources.clone(),
            two(graded(0), Vec::new()),
            Vec::new(),
            vec![grade_at(1)],
        )
        .expect("a colour the table holds");
        assert_eq!(loaded.color_of(Lane::V1, 0), Some(&grade_at(1)));

        // Group ids survive a load, and the next split gets a fresh one.
        let mut p = Project::from_parts(sources, lanes, eq, color).expect("valid parts");
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
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };

        // The door: named errors, one per cause.
        let err = |video: Vec<Clip>, audio: Vec<Clip>| {
            Project::from_parts(sources.clone(), two(video, audio), Vec::new(), Vec::new())
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
        let (sources, lanes, eq, color) = p.clone().without_orphan_sources();
        assert!(
            Project::from_parts(sources, lanes, eq, color).is_ok(),
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

    /// The pairing the whole model rests on: [`Speed::source_at`] (floor) is
    /// what an export encodes at a timeline frame, [`Speed::timeline_at`] (ceil)
    /// is the stamp playback puts on a decoded source frame -- and the *last*
    /// source frame the pump is still holding at a timeline frame has to be
    /// exactly the one the export encoded there, at every rate. Anything else is
    /// A/V drift that grows with the clip, and no test of either side alone
    /// would see it.
    ///
    /// (Grafted from the rival build of this feature, whose floor/ceil pairing
    /// is a better answer than the floor/floor this had: floor/floor put the
    /// preview a whole held frame off the export at slow rates.)
    #[test]
    fn speed_maps_both_ways() {
        for permille in (250..=4000).step_by(1) {
            let speed = Speed::from_permille(permille);
            assert_eq!(speed.permille(), permille, "no clamping in range");
            for d in 0..200u32 {
                let want = speed.source_at(d);
                // What the pump shows at `d` is the last frame whose stamp has
                // come due -- and it is `want`, the frame the export encodes
                // there. Slowed, that is the frame it is still holding; sped up,
                // it is the newest of the several that arrived.
                let held = (0..=want + 8)
                    .filter(|&s| speed.timeline_at(s) <= d)
                    .next_back();
                assert_eq!(held, Some(want), "{speed} at timeline frame {d}");
            }
            for n in 1..300u32 {
                // A footprint is never zero: a clip occupying no timeline frame
                // is a placement the lane invariants cannot hold.
                let frames = speed.frames(n);
                assert!(frames >= 1, "{speed} of {n} frames");
                // ...and its last timeline frame never reads past the clip.
                assert!(
                    speed.source_at(frames - 1) < n,
                    "{speed}: the last timeline frame of {n} reads past the clip"
                );
                // The trim inverse never overshoots the room it was given, and
                // answers for every room a footprint can be.
                let fit = speed.fit(frames).expect("a clip fits its own footprint");
                assert!(speed.frames(fit) <= frames, "{speed}: fit({frames}) grew");
                // A room narrower than one source frame's own footprint holds
                // nothing, and says so rather than rounding something into it.
                assert_eq!(speed.fit(speed.room(1) - 1), None, "{speed}: too narrow");
                assert_eq!(speed.room(0), 0, "{speed}: no head is no room");
            }
        }
        // Real time is the identity map, byte for byte.
        for d in 0..1000 {
            assert_eq!(Speed::NORMAL.source_at(d), d);
            assert_eq!(Speed::NORMAL.timeline_at(d), d);
            assert_eq!(Speed::NORMAL.frames(d.max(1)), d.max(1));
            assert_eq!(Speed::NORMAL.fit(d.max(1)), Some(d.max(1)));
            assert_eq!(Speed::NORMAL.repeats(d, d + 1), 1);
        }
        // The one door: no zero, no reverse, nothing past the ends.
        assert_eq!(Speed::from_permille(0), Speed::MIN);
        assert_eq!(Speed::from_permille(249), Speed::MIN);
        assert_eq!(Speed::from_permille(4001), Speed::MAX);
        assert_eq!(Speed::from_permille(1000), Speed::NORMAL);
        assert_eq!(Speed::NORMAL.to_string(), "1.00x");
        assert_eq!(Speed::from_permille(2500).to_string(), "2.50x");
    }

    /// The mapping a speed changes, and the one it does not: a 2x clip reads two
    /// source frames per timeline frame and still starts at its own in-point,
    /// and a half-speed one shows every source frame twice.
    #[test]
    fn a_rate_maps_timeline_frames_onto_source_frames() {
        let mut p = Project::single(FILE, 40);
        p.set_speed(Lane::V1, 0, Speed::from_permille(2000))
            .expect("room enough");
        assert_eq!(p.timeline_frames(), 20, "40 source frames at 2x");
        assert_eq!(
            p.map(Lane::V1, 0),
            Some((0, 0)),
            "and it starts where it did"
        );
        assert_eq!(p.map(Lane::V1, 1), Some((0, 2)));
        assert_eq!(p.map(Lane::V1, 19), Some((0, 38)));
        assert_eq!(p.map(Lane::V1, 20), None, "and ends where the clip does");
        // The span a decoder is handed: as many source frames as it will read,
        // which is the last shown frame's own and not one more -- timeline frame
        // 19 shows source frame 38, so 39 frames are read and the 40th is a
        // picture nothing would display.
        let span = p.span_at(Lane::V1, 0).expect("a span");
        assert_eq!((span.len, span.source_len()), (20, 39));
        assert_eq!(span.timeline_at(38), 19, "and it is stamped where it shows");
        // ...and the same clip the other way round.
        let mut p = Project::single(FILE, 40);
        p.set_speed(Lane::V1, 0, Speed::from_permille(500))
            .expect("room enough");
        assert_eq!(p.timeline_frames(), 80);
        assert_eq!(p.map(Lane::V1, 0), Some((0, 0)));
        assert_eq!(p.map(Lane::V1, 1), Some((0, 0)), "each frame shows twice");
        assert_eq!(p.map(Lane::V1, 2), Some((0, 1)));
        assert_eq!(
            p.map(Lane::V1, 79),
            Some((0, 39)),
            "and the last timeline frame is still inside the source range"
        );
    }

    /// A trim of a speeded clip is still a trim of its *source*, and the edge
    /// still stops at the wall: the room a rate leaves is measured in timeline
    /// frames and the range it commits in source ones.
    #[test]
    fn trimming_a_speeded_clip_stays_inside_its_room() {
        let mut p = Project::single(FILE, 40);
        p.set_speed(Lane::V1, 0, Speed::from_permille(2000))
            .expect("room enough");
        // A second clip butted against the first, so the wall is a real one.
        assert!(p.place(Lane::V1, 20, clip(0, 0, 10, 0)));
        let room = p.trim_room(Lane::V1, 0, Edge::End, &[40]).expect("a clip");
        assert_eq!(room, (1, 20), "out to the neighbour and no further");
        assert!(
            !p.trim(Lane::V1, 0, Edge::End, 30, &[40]),
            "clamped to the wall, which is where the edge already is"
        );
        let c = p.clips()[0];
        assert_eq!(c.end(), 20, "so nothing moved");
        assert_eq!(c.out_frame, 40, "and it is still the whole source range");
        // ...and in: half the timeline frames is half the source range.
        assert!(p.trim(Lane::V1, 0, Edge::End, 10, &[40]));
        let c = p.clips()[0];
        assert_eq!((c.in_frame, c.out_frame), (0, 20), "in source frames");
        assert_eq!(c.end(), 10, "and ten timeline frames of them");
        assert!(sorted_disjoint(p.clips()));
    }

    /// The frames a rate cannot address: at half speed each source frame is on
    /// screen twice, and the cut between the two showings is not a cut in the
    /// file. Refused, so the two halves always add up to the clip that was cut.
    #[test]
    fn a_slow_clip_cuts_only_where_its_source_has_a_frame() {
        let mut p = Project::single(FILE, 10);
        p.set_speed(Lane::V1, 0, Speed::from_permille(500))
            .expect("room enough");
        assert_eq!(p.timeline_frames(), 20);
        assert!(
            !p.split(3),
            "an odd frame is between two showings of one frame"
        );
        assert_eq!(p.clips().len(), 1, "and nothing was cut");
        assert!(p.split(4), "an even one is a frame boundary");
        let halves = p.clips().to_vec();
        assert_eq!(
            (halves[0].end(), halves[1].start, halves[1].end()),
            (4, 4, 20),
            "the halves meet, and still end where the clip did"
        );
        assert_eq!(halves[0].out_frame, halves[1].in_frame, "in source frames");
        assert!(p.regroup(4), "and the inverse puts it back");
        assert_eq!(p.clips().len(), 1);
    }

    /// The same claim, swept: random op sequences off the public surface, the
    /// project reloaded after every one of them. A failure prints its seed, and
    /// the seed replays the whole sequence.
    #[test]
    fn random_edit_sequences_reload() {
        // Two thousand, not two hundred: the overlap a hole punched into a slow
        // clip at a frame its rate cannot address leaves behind sat in ~0.7% of
        // sequences, and a 200-seed sweep walked straight past it (it took a
        // judge's 2000-seed run to surface `place` at all). A sweep this cheap
        // has no business being the narrow one.
        for seed in 0..2000u64 {
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
                // -- the input that made a placement duplicate an id -- and, one
                // time in three, a rate of its own: a *speeded clip placed over
                // a speeded clip* is the shape whose remainders have to be run
                // through `Speed::fit`, and it is not a shape the lanes hand out
                // on their own often enough to be sure of.
                let mut copied = *p
                    .lane(if next(2) == 0 { Lane::V1 } else { Lane::A1 })
                    .get(next(4) as usize)
                    .unwrap_or(&clip(0, 0, 5, 0));
                if next(3) == 0 {
                    copied.speed = Speed::from_permille(250 + next(16) as u16 * 250);
                }
                let lane = if next(2) == 0 { Lane::V1 } else { Lane::A1 };
                let idx = next(4) as usize;
                let _ = match next(15) {
                    // ...and a second `place`, so a punch lands inside a clip
                    // far more often than one op in fourteen would put it there.
                    14 => p.place(lane, frame, copied),
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
                    // ...and a colour grade in the same mix, for the same
                    // reason: it rides on the clip and its orphans must go.
                    10 => p.set_color(lane, idx, Some(grade_at(next(4)))),
                    11 => p.set_color(lane, idx, None),
                    // ...and a rate in with them, which is the one of the three
                    // that moves *placements*: a clip that grew has to be
                    // refused rather than overlap, and one that shrank leaves a
                    // lane that still loads. Both are what `reloads` asks.
                    12 => p
                        .set_speed(lane, idx, Speed::from_permille(250 + next(16) as u16 * 250))
                        .is_ok(),
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
        let (sources, lanes, eq, color) = p.clone().without_orphan_sources();
        match Project::from_parts(sources, lanes, eq, color) {
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
                // The colour table renumbers in the same prune, so the same
                // claim: the params a clip names, not the index it names them by.
                let colors = |p: &Project| -> Vec<Vec<Option<ColorParams>>> {
                    p.lanes()
                        .into_iter()
                        .map(|l| {
                            (0..p.lane(l).len())
                                .map(|i| p.color_of(l, i).copied())
                                .collect()
                        })
                        .collect()
                };
                assert_eq!(colors(&back), colors(p), "{what}: the colours changed");
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
                from: None,
                speed: Speed::NORMAL
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
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
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
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
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
        let (sources, lanes, eq, color) = p.without_orphan_sources();
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
        let back = Project::from_parts(sources, lanes, eq, color).expect("four lanes load");
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
        for seed in 0..2000u64 {
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
                let _ = match next(13) {
                    10 => p.set_eq(lane, idx, Some(band_at(next(4)))),
                    11 => p.set_eq(lane, idx, None),
                    // A rate, which on many lanes is the group rule as well:
                    // every clip carrying the link moves together or none of
                    // them does, and `invariants_hold` is what says so.
                    12 => p
                        .set_speed(lane, idx, Speed::from_permille(250 + next(16) as u16 * 250))
                        .is_ok(),
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

    /// The compositing rule, in one walk: `V2` covers `V1` while it has a clip
    /// there, `V1` shows again on either side of it, and a frame no video lane
    /// covers is a gap however many lanes there are.
    #[test]
    fn the_composite_is_the_topmost_lane_with_a_clip() {
        let sources = vec![Source::new(FILE, 0), Source::new(FILE2, 0)];
        let lanes = vec![
            // V1: the whole 30 frames of source 0.
            (LaneKind::Video, vec![clip(0, 0, 30, 0)]),
            // V2: source 1 over the middle third.
            (LaneKind::Video, vec![clip(10, 5, 15, 1)]),
            (LaneKind::Audio, vec![clip(0, 0, 30, 0)]),
        ];
        let p = Project::from_parts(sources, lanes, vec![], vec![]).expect("valid parts");
        assert_eq!(
            p.composite_spans_from(0),
            vec![
                Span {
                    start: 0,
                    len: 10,
                    from: Some((0, 0)),
                    speed: Speed::NORMAL
                },
                Span {
                    start: 10,
                    len: 10,
                    from: Some((1, 5)),
                    speed: Speed::NORMAL
                },
                Span {
                    start: 20,
                    len: 10,
                    from: Some((0, 20)),
                    speed: Speed::NORMAL
                },
            ],
            "V2 takes over for its own span and hands V1 back"
        );
        // A mid-span ask resolves the same way, which is what a seek does.
        assert_eq!(
            p.composite_span_at(15),
            Some(Span {
                start: 15,
                len: 5,
                from: Some((1, 10)),
                speed: Speed::NORMAL
            })
        );
        assert_eq!(p.composite_span_at(30), None, "past the end");

        // A hole in V1 under V2's clip: black between the two, and black after
        // both run out (the audio lane is still holding the timeline open).
        let sources = vec![Source::new(FILE, 0), Source::new(FILE2, 0)];
        let holed = vec![
            (LaneKind::Video, vec![clip(0, 0, 5, 0)]),
            (LaneKind::Video, vec![clip(10, 0, 10, 1)]),
            (LaneKind::Audio, vec![clip(0, 0, 30, 0)]),
        ];
        let p = Project::from_parts(sources, holed, vec![], vec![]).expect("valid parts");
        assert_eq!(
            p.composite_spans_from(0),
            vec![
                Span {
                    start: 0,
                    len: 5,
                    from: Some((0, 0)),
                    speed: Speed::NORMAL
                },
                Span {
                    start: 5,
                    len: 5,
                    from: None,
                    speed: Speed::NORMAL
                },
                Span {
                    start: 10,
                    len: 10,
                    from: Some((1, 0)),
                    speed: Speed::NORMAL
                },
                Span {
                    start: 20,
                    len: 10,
                    from: None,
                    speed: Speed::NORMAL
                },
            ]
        );

        // One video lane: the composite *is* that lane, span for span -- the
        // promise every existing two-lane project rests on.
        let p = Project::single(FILE, 30);
        assert_eq!(p.composite_spans_from(0), p.spans_from(Lane::V1, 0));
        assert_eq!(p.composite_span_at(7), p.span_at(Lane::V1, 7));
    }

    /// Every audio lane holding something gets a play list, and only those.
    #[test]
    fn audio_segments_cover_every_lane_that_holds_something() {
        let sources = vec![Source::new(FILE, 0), Source::new(FILE2, 0)];
        let lanes = vec![
            (LaneKind::Video, vec![clip(0, 0, 30, 0)]),
            (LaneKind::Audio, vec![clip(0, 0, 30, 0)]),
            (LaneKind::Audio, vec![clip(15, 0, 15, 1)]),
            (LaneKind::Audio, Vec::new()),
        ];
        let p = Project::from_parts(sources, lanes, vec![], vec![]).expect("valid parts");
        let lists = p.audio_segments_from(0, FPS);
        assert_eq!(lists.len(), 2, "the empty lane is not mixed in");
        assert_eq!(lists[0], p.segments_from(0, FPS), "A1 is unchanged");
        // A2 is a gap up to frame 15, then the whole of source 1.
        assert_eq!(lists[1], vec![(None, 0.0, 0.5), (Some(1), 0.0, 0.5)]);

        // No audio lane holds anything: one all-gap list, so such a timeline
        // still plays silence against a master clock.
        let p = Project::from_parts(
            vec![Source::new(FILE, 0)],
            vec![
                (LaneKind::Video, vec![clip(0, 0, 30, 0)]),
                (LaneKind::Audio, Vec::new()),
            ],
            vec![],
            vec![],
        )
        .expect("valid parts");
        assert_eq!(p.audio_segments_from(0, FPS), vec![vec![(None, 0.0, 1.0)]]);
    }
}
