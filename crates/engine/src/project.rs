//! Edit list: the timeline as a list of source ranges. Pure data, no I/O.
//!
//! A [`Project`] is an ordered list of [`Clip`]s, each a half-open `[in, out)`
//! range of *source* frames. Playing the timeline means playing those ranges
//! back to back, so timeline frame `t` is found by walking the clips and
//! consuming their lengths; the mapping is [`Project::map_timeline`] and it is
//! the only place that conversion lives.
//!
//! Two frame spaces meet here and are never mixed: *source* frames index the
//! decoded file, *timeline* frames index what the viewer sees. Both are 0-based
//! (sample ids in the demuxer are 1-based; that conversion stays in `decode`).
//!
//! Editing is metadata only. [`Project::cut`] splits a clip in two without
//! changing any timeline->source mapping, so a running decoder stays correct
//! across a cut; [`Project::delete`] does change the mapping and the caller
//! must reseek. Every successful edit snapshots the clip list, so
//! [`Project::undo`] is an exact restore.
//!
//! A clip names its file by *index* into [`Project::sources`], which is
//! append-only: an index handed out once stays valid forever, so a clip on the
//! clipboard or inside an undo snapshot can never dangle. An index -- not an
//! `Arc<Path>` -- because [`Clip`] is `Copy`, which is what makes copy/paste a
//! plain assignment.

use std::path::{Path, PathBuf};

/// A half-open `[in_frame, out_frame)` range of frames of source
/// [`source`](Clip::source). Never empty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Clip {
    pub in_frame: u32,
    pub out_frame: u32,
    /// Index into [`Project::sources`].
    pub source: usize,
}

impl Clip {
    /// Frame count; `>= 1` by the never-empty invariant.
    pub fn len(&self) -> u32 {
        self.out_frame - self.in_frame
    }
}

/// The edit list plus its undo history.
#[derive(Clone, Debug)]
pub struct Project {
    /// Append-only: never popped, never reordered. See the module docs.
    sources: Vec<PathBuf>,
    clips: Vec<Clip>,
    /// Snapshots pushed *before* each successful edit; `undo` pops one.
    history: Vec<Vec<Clip>>,
}

impl Project {
    /// One clip covering the whole of `path` -- the state of a freshly opened
    /// video, where the timeline is the source. `frame_count` of 0 would break
    /// the never-empty invariant, so it is clamped to one frame.
    pub fn single(path: impl AsRef<Path>, frame_count: u32) -> Self {
        Self {
            sources: vec![canonical(path.as_ref())],
            clips: vec![Clip {
                in_frame: 0,
                out_frame: frame_count.max(1),
                source: 0,
            }],
            history: Vec::new(),
        }
    }

    /// A project rebuilt from a saved edit list -- the load half of
    /// [`crate::veproj`]. History is *not* saved, so [`Project::undo`] is
    /// `false` until the first edit of the new session. `None` when the parts
    /// would break an invariant every other constructor keeps: no clips, an
    /// empty clip, or a clip naming a source that is not there.
    pub fn from_parts(sources: Vec<PathBuf>, clips: Vec<Clip>) -> Option<Self> {
        if clips.is_empty()
            || clips
                .iter()
                .any(|c| c.source >= sources.len() || c.out_frame <= c.in_frame)
        {
            return None;
        }
        Some(Self {
            sources: sources.iter().map(|s| canonical(s)).collect(),
            clips,
            history: Vec::new(),
        })
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// The files the clips index into, in import order; index 0 is the file the
    /// project was opened with.
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }

    /// Index for `path`, appending it if it is new. Deduped by
    /// `fs::canonicalize`, so the same file reached by two paths imports once.
    /// The clip list is untouched -- see [`Project::append_clip`] -- so this
    /// pushes no history: a source entry alone changes nothing playable.
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

    /// Append a whole-file clip of `source` to the end of the timeline. One
    /// history snapshot, so an import is one undo step -- and undoing it leaves
    /// the (harmless) source entry behind, because indexes are forever.
    /// Refused for an unknown source index.
    pub fn append_clip(&mut self, source: usize, frame_count: u32) -> bool {
        if source >= self.sources.len() {
            return false;
        }
        self.history.push(self.clips.clone());
        self.clips.push(Clip {
            in_frame: 0,
            out_frame: frame_count.max(1),
            source,
        });
        true
    }

    /// The sources a clip actually names, with the clips reindexed onto them
    /// -- what a save writes. Indexes are forever *inside* a session (see
    /// [`Project::append_clip`]), so an undone import leaves an orphan source
    /// entry behind; writing that orphan to a project file would let a file
    /// nothing plays refuse a future load. New indexes are assigned in order of
    /// first use, so the same project always emits the same bytes.
    pub fn without_orphan_sources(&self) -> (Vec<PathBuf>, Vec<Clip>) {
        let mut moved = vec![None; self.sources.len()];
        let mut sources = Vec::new();
        let mut clips = self.clips.clone();
        for c in &mut clips {
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
        (sources, clips)
    }

    /// Length of the timeline in frames.
    pub fn timeline_frames(&self) -> u32 {
        self.clips.iter().map(Clip::len).sum()
    }

    /// `(timeline_start, len)` per clip, in order -- what a UI lane needs to
    /// lay clips out without redoing the cumulative sum.
    pub fn clip_spans(&self) -> Vec<(u32, u32)> {
        let mut start = 0;
        self.clips
            .iter()
            .map(|c| {
                let span = (start, c.len());
                start += c.len();
                span
            })
            .collect()
    }

    /// Timeline frame -> `(clip index, source frame)`. `None` past the end.
    pub fn map_timeline(&self, timeline_frame: u32) -> Option<(usize, u32)> {
        let mut start = 0;
        for (i, c) in self.clips.iter().enumerate() {
            if timeline_frame < start + c.len() {
                return Some((i, c.in_frame + (timeline_frame - start)));
            }
            start += c.len();
        }
        None
    }

    /// Split the clip containing `timeline_frame` so that frame becomes the
    /// first frame of a new clip. Refused (`false`, no change) at timeline 0,
    /// at an existing clip boundary, and at or past the end -- all three would
    /// produce an empty clip. Metadata only: no mapping changes.
    pub fn cut(&mut self, timeline_frame: u32) -> bool {
        let Some((idx, source)) = self.map_timeline(timeline_frame) else {
            return false;
        };
        if source == self.clips[idx].in_frame {
            return false; // start of a clip: nothing to split off
        }
        self.history.push(self.clips.clone());
        let out = self.clips[idx].out_frame;
        self.clips[idx].out_frame = source;
        self.clips.insert(
            idx + 1,
            Clip {
                in_frame: source,
                out_frame: out,
                ..self.clips[idx]
            },
        );
        true
    }

    /// Insert `clip` so that it starts at `timeline_frame`. Mid-clip the
    /// containing clip is split there and `clip` goes between the halves; at an
    /// existing boundary (0 included) it goes in front of the clip that starts
    /// there; at or past the end of the timeline it is appended -- a paste past
    /// the end clamps rather than refuses, because there is nothing between the
    /// end and it. Refused only for an empty `clip`, which the never-empty
    /// invariant forbids but the public fields still allow a caller to build.
    ///
    /// Exactly one history snapshot, so one [`Project::undo`] takes it back --
    /// hence the splice rather than a `cut` plus an insert. Changes the
    /// timeline->source mapping: the caller must reseek.
    pub fn paste(&mut self, timeline_frame: u32, clip: Clip) -> bool {
        if clip.out_frame <= clip.in_frame {
            return false;
        }
        self.history.push(self.clips.clone());
        match self.map_timeline(timeline_frame) {
            Some((idx, source)) if source > self.clips[idx].in_frame => {
                let out = self.clips[idx].out_frame;
                self.clips[idx].out_frame = source;
                self.clips.splice(
                    idx + 1..idx + 1,
                    [
                        clip,
                        Clip {
                            in_frame: source,
                            out_frame: out,
                            ..self.clips[idx]
                        },
                    ],
                );
            }
            Some((idx, _)) => self.clips.insert(idx, clip),
            None => self.clips.push(clip),
        }
        true
    }

    /// Remove a clip and close the gap. Refused for an out-of-range index and
    /// for the last remaining clip (an empty timeline is undefined this slice).
    /// Changes the timeline->source mapping: the caller must reseek.
    pub fn delete(&mut self, idx: usize) -> bool {
        if idx >= self.clips.len() || self.clips.len() == 1 {
            return false;
        }
        self.history.push(self.clips.clone());
        self.clips.remove(idx);
        true
    }

    /// Restore the clip list from before the last successful edit. `false` when
    /// there is nothing left to undo.
    pub fn undo(&mut self) -> bool {
        match self.history.pop() {
            Some(prev) => {
                self.clips = prev;
                true
            }
            None => false,
        }
    }

    /// Half-open `(source, start, end)` segments in *source* seconds, from
    /// `timeline_frame` to the end of the timeline -- the play list for the
    /// audio worker. The first entry is partial when the position is mid-clip.
    /// Empty when the position is past the end or `fps` is not usable.
    pub fn segments_from(&self, timeline_frame: u32, fps: f64) -> Vec<(usize, f64, f64)> {
        if !(fps.is_finite() && fps > 0.0) {
            return Vec::new();
        }
        let Some((idx, source)) = self.map_timeline(timeline_frame) else {
            return Vec::new();
        };
        let head = self.clips[idx];
        let mut segs = vec![(
            head.source,
            source as f64 / fps,
            head.out_frame as f64 / fps,
        )];
        segs.extend(
            self.clips[idx + 1..]
                .iter()
                .map(|c| (c.source, c.in_frame as f64 / fps, c.out_frame as f64 / fps)),
        );
        segs
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

    /// Source clips [0,3) [3,5) [5,9) -- the ledger's off-by-one fixture. Built
    /// through `cut` so the constructor path is exercised too.
    fn three() -> Project {
        let mut p = Project::single(FILE, 9);
        assert!(p.cut(3));
        assert!(p.cut(5));
        assert_eq!(
            p.clips(),
            [
                Clip {
                    source: 0,
                    in_frame: 0,
                    out_frame: 3
                },
                Clip {
                    source: 0,
                    in_frame: 3,
                    out_frame: 5
                },
                Clip {
                    source: 0,
                    in_frame: 5,
                    out_frame: 9
                },
            ]
        );
        p
    }

    #[test]
    fn single_is_the_whole_file() {
        let p = Project::single(FILE, 150);
        assert_eq!(p.clips().len(), 1);
        assert_eq!(p.timeline_frames(), 150);
        assert_eq!(p.clip_spans(), vec![(0, 150)]);
        // degenerate mapping: timeline == source
        assert_eq!(p.map_timeline(0), Some((0, 0)));
        assert_eq!(p.map_timeline(149), Some((0, 149)));
        assert_eq!(p.map_timeline(150), None);
        // never-empty invariant survives a bogus frame count
        assert_eq!(Project::single(FILE, 0).timeline_frames(), 1);
    }

    #[test]
    fn cut_mid_clip_splits() {
        let mut p = Project::single(FILE, 9);
        assert!(p.cut(4));
        assert_eq!(
            p.clips(),
            [
                Clip {
                    source: 0,
                    in_frame: 0,
                    out_frame: 4
                },
                Clip {
                    source: 0,
                    in_frame: 4,
                    out_frame: 9
                },
            ]
        );
        assert_eq!(p.timeline_frames(), 9);
    }

    #[test]
    fn cut_refused_at_zero_boundary_and_end() {
        let mut p = three();
        let before = p.clips().to_vec();
        assert!(!p.cut(0), "timeline 0 has nothing before it");
        assert!(!p.cut(3), "existing boundary");
        assert!(!p.cut(5), "existing boundary");
        assert!(!p.cut(9), "one past the last frame");
        assert!(!p.cut(1_000));
        assert_eq!(p.clips(), before.as_slice());
        // one undo lands before the *second* cut, so the refusals pushed nothing
        assert!(p.undo());
        assert_eq!(p.clips().len(), 2, "refused cuts push no history");
    }

    #[test]
    fn cut_never_changes_the_mapping() {
        let p = three();
        let before: Vec<_> = (0..9).map(|f| p.map_timeline(f).unwrap().1).collect();
        let mut after = p.clone();
        assert!(after.cut(7));
        let now: Vec<_> = (0..9).map(|f| after.map_timeline(f).unwrap().1).collect();
        assert_eq!(before, now);
    }

    #[test]
    fn map_timeline_sweeps_every_boundary() {
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
    }

    /// The clip a copy would hand back: source `[100, 102)`, unrelated to
    /// anything in `three()` so it is recognisable wherever it lands.
    const PASTED: Clip = Clip {
        source: 0,
        in_frame: 100,
        out_frame: 102,
    };

    #[test]
    fn paste_mid_clip_splits_around_it() {
        let mut p = three();
        assert!(p.paste(6, PASTED)); // inside [5,9), one frame in
        assert_eq!(p.timeline_frames(), 11);
        assert_eq!(
            p.clips(),
            [
                Clip {
                    source: 0,
                    in_frame: 0,
                    out_frame: 3
                },
                Clip {
                    source: 0,
                    in_frame: 3,
                    out_frame: 5
                },
                Clip {
                    source: 0,
                    in_frame: 5,
                    out_frame: 6
                },
                PASTED,
                Clip {
                    source: 0,
                    in_frame: 6,
                    out_frame: 9
                },
            ]
        );
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

        // At the end and past it both append -- there is nothing to sit before.
        let mut p = three();
        assert!(p.paste(9, PASTED));
        assert_eq!(p.clip_spans(), vec![(0, 3), (3, 2), (5, 4), (9, 2)]);
        assert!(p.paste(1_000, PASTED));
        assert_eq!(p.timeline_frames(), 13);

        // An empty clip is the one thing refused, and it pushes no history.
        let mut p = three();
        assert!(!p.paste(
            4,
            Clip {
                source: 0,
                in_frame: 7,
                out_frame: 7
            }
        ));
        assert_eq!(p.clips().len(), 3);
    }

    #[test]
    fn paste_undoes_in_one_step() {
        let mut p = three();
        let before = p.clips().to_vec();
        assert!(p.paste(6, PASTED));
        assert!(p.undo());
        assert_eq!(p.clips(), before.as_slice(), "a paste is one undo step");
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
    fn delete_closes_the_gap() {
        let mut p = three();
        assert!(p.delete(1));
        assert_eq!(p.timeline_frames(), 7);
        assert_eq!(p.clip_spans(), vec![(0, 3), (3, 4)]);
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

    #[test]
    fn undo_restores_exactly() {
        let mut p = three();
        let after_cuts = p.clips().to_vec();
        assert!(p.delete(1));
        assert!(p.undo());
        assert_eq!(p.clips(), after_cuts.as_slice());

        // walk all the way back through both cuts, then run dry
        assert!(p.undo());
        assert_eq!(p.clips().len(), 2);
        assert!(p.undo());
        assert_eq!(
            p.clips(),
            [Clip {
                source: 0,
                in_frame: 0,
                out_frame: 9
            }]
        );
        assert!(!p.undo(), "empty history");
        assert_eq!(
            p.clips(),
            [Clip {
                source: 0,
                in_frame: 0,
                out_frame: 9
            }]
        );
    }

    #[test]
    fn segments_from_trims_the_first_entry() {
        let p = three();
        // mid-clip: partial first segment, whole clips after it
        assert_eq!(
            p.segments_from(4, FPS),
            vec![(0, 4.0 / 30.0, 5.0 / 30.0), (0, 5.0 / 30.0, 9.0 / 30.0)]
        );
        // on a boundary: nothing partial
        assert_eq!(
            p.segments_from(3, FPS),
            vec![(0, 3.0 / 30.0, 5.0 / 30.0), (0, 5.0 / 30.0, 9.0 / 30.0)]
        );
        // from the top: one entry per clip
        assert_eq!(p.segments_from(0, FPS).len(), 3);
        // last clip only
        assert_eq!(p.segments_from(8, FPS), vec![(0, 8.0 / 30.0, 0.3)]);
        // past the end / unusable fps
        assert!(p.segments_from(9, FPS).is_empty());
        assert!(p.segments_from(0, 0.0).is_empty());
        assert!(p.segments_from(0, f64::NAN).is_empty());
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
        assert_eq!(
            p.clips()[3],
            Clip {
                in_frame: 0,
                out_frame: 4,
                source: 1
            }
        );
        assert_eq!(p.map_timeline(9), Some((3, 0)), "source 1 starts at 9");
        assert!(p.undo());
        assert_eq!(p.clips(), three().clips(), "one step back to one source");
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
        // A cut inside the second source splits into two source-1 clips.
        assert!(p.cut(11));
        assert_eq!(
            p.clips()[3],
            Clip {
                in_frame: 0,
                out_frame: 2,
                source: 1
            }
        );
        assert_eq!(
            p.clips()[4],
            Clip {
                in_frame: 2,
                out_frame: 4,
                source: 1
            }
        );

        // A source-1 clip pasted into source-0 territory keeps its file, and so
        // does the source-0 half that got split off around it.
        let copied = p.clips()[4];
        assert!(p.paste(1, copied));
        assert_eq!(p.clips()[1], copied);
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
                (0, 4.0 / 30.0, 5.0 / 30.0),
                (0, 5.0 / 30.0, 9.0 / 30.0),
                (1, 0.0, 4.0 / 30.0),
            ]
        );
        // Mid-clip in source 1: only that source is left.
        assert_eq!(p.segments_from(10, FPS), vec![(1, 1.0 / 30.0, 4.0 / 30.0)]);
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
        assert_eq!(segs, vec![(0, 3.0 / 25.0, 0.2), (0, 0.2, 9.0 / 25.0)]);
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
        let (sources, clips) = p.without_orphan_sources();
        assert_eq!(sources, vec![PathBuf::from(FILE)], "the orphan is gone");
        assert_eq!(clips, three().clips(), "the clips are untouched");

        // Three sources where only the middle one is orphaned: the survivors
        // renumber, and the clips follow.
        let mut p = Project::single(FILE, 9);
        assert_eq!(p.import(FILE2), 1);
        assert_eq!(p.import("/nonexistent/c.mp4"), 2);
        assert!(p.append_clip(2, 4));
        let (sources, clips) = p.without_orphan_sources();
        assert_eq!(
            sources,
            vec![PathBuf::from(FILE), PathBuf::from("/nonexistent/c.mp4")]
        );
        assert_eq!(clips.iter().map(|c| c.source).collect::<Vec<_>>(), [0, 1]);
        // ...and what comes out is loadable, with the same timeline.
        let reloaded = Project::from_parts(sources, clips).expect("from_parts");
        assert_eq!(reloaded.timeline_frames(), p.timeline_frames());
        assert_eq!(reloaded.sources().len(), 2);
    }

    #[test]
    fn from_parts_has_no_history_and_checks_the_invariants() {
        let (sources, clips) = three().without_orphan_sources();
        let mut p = Project::from_parts(sources.clone(), clips.clone()).expect("valid parts");
        assert_eq!(p.clips(), three().clips());
        assert!(!p.undo(), "a loaded project has nothing to undo");
        assert!(p.cut(4), "...and is editable from there");
        assert!(p.undo());

        assert!(Project::from_parts(sources.clone(), Vec::new()).is_none());
        assert!(
            Project::from_parts(
                sources.clone(),
                vec![Clip {
                    source: 1,
                    in_frame: 0,
                    out_frame: 3
                }]
            )
            .is_none(),
            "clip names a source that is not there"
        );
        assert!(
            Project::from_parts(
                sources,
                vec![Clip {
                    source: 0,
                    in_frame: 3,
                    out_frame: 3
                }]
            )
            .is_none(),
            "empty clip"
        );
    }
}
