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

/// A half-open `[in_frame, out_frame)` range of source frames. Never empty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Clip {
    pub in_frame: u32,
    pub out_frame: u32,
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
    clips: Vec<Clip>,
    /// Snapshots pushed *before* each successful edit; `undo` pops one.
    history: Vec<Vec<Clip>>,
}

impl Project {
    /// One clip covering the whole file -- the state of a freshly opened video,
    /// where the timeline is the source. `frame_count` of 0 would break the
    /// never-empty invariant, so it is clamped to one frame.
    pub fn single(frame_count: u32) -> Self {
        Self {
            clips: vec![Clip {
                in_frame: 0,
                out_frame: frame_count.max(1),
            }],
            history: Vec::new(),
        }
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
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
            },
        );
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

    /// Half-open `(start, end)` segments in *source* seconds, from
    /// `timeline_frame` to the end of the timeline -- the play list for the
    /// audio worker. The first entry is partial when the position is mid-clip.
    /// Empty when the position is past the end or `fps` is not usable.
    pub fn segments_from(&self, timeline_frame: u32, fps: f64) -> Vec<(f64, f64)> {
        if !(fps.is_finite() && fps > 0.0) {
            return Vec::new();
        }
        let Some((idx, source)) = self.map_timeline(timeline_frame) else {
            return Vec::new();
        };
        let mut segs = vec![(source as f64 / fps, self.clips[idx].out_frame as f64 / fps)];
        segs.extend(
            self.clips[idx + 1..]
                .iter()
                .map(|c| (c.in_frame as f64 / fps, c.out_frame as f64 / fps)),
        );
        segs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 30.0;

    /// Source clips [0,3) [3,5) [5,9) -- the ledger's off-by-one fixture. Built
    /// through `cut` so the constructor path is exercised too.
    fn three() -> Project {
        let mut p = Project::single(9);
        assert!(p.cut(3));
        assert!(p.cut(5));
        assert_eq!(
            p.clips(),
            [
                Clip {
                    in_frame: 0,
                    out_frame: 3
                },
                Clip {
                    in_frame: 3,
                    out_frame: 5
                },
                Clip {
                    in_frame: 5,
                    out_frame: 9
                },
            ]
        );
        p
    }

    #[test]
    fn single_is_the_whole_file() {
        let p = Project::single(150);
        assert_eq!(p.clips().len(), 1);
        assert_eq!(p.timeline_frames(), 150);
        assert_eq!(p.clip_spans(), vec![(0, 150)]);
        // degenerate mapping: timeline == source
        assert_eq!(p.map_timeline(0), Some((0, 0)));
        assert_eq!(p.map_timeline(149), Some((0, 149)));
        assert_eq!(p.map_timeline(150), None);
        // never-empty invariant survives a bogus frame count
        assert_eq!(Project::single(0).timeline_frames(), 1);
    }

    #[test]
    fn cut_mid_clip_splits() {
        let mut p = Project::single(9);
        assert!(p.cut(4));
        assert_eq!(
            p.clips(),
            [
                Clip {
                    in_frame: 0,
                    out_frame: 4
                },
                Clip {
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
                in_frame: 0,
                out_frame: 9
            }]
        );
        assert!(!p.undo(), "empty history");
        assert_eq!(
            p.clips(),
            [Clip {
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
            vec![(4.0 / 30.0, 5.0 / 30.0), (5.0 / 30.0, 9.0 / 30.0)]
        );
        // on a boundary: nothing partial
        assert_eq!(
            p.segments_from(3, FPS),
            vec![(3.0 / 30.0, 5.0 / 30.0), (5.0 / 30.0, 9.0 / 30.0)]
        );
        // from the top: one entry per clip
        assert_eq!(p.segments_from(0, FPS).len(), 3);
        // last clip only
        assert_eq!(p.segments_from(8, FPS), vec![(8.0 / 30.0, 0.3)]);
        // past the end / unusable fps
        assert!(p.segments_from(9, FPS).is_empty());
        assert!(p.segments_from(0, 0.0).is_empty());
        assert!(p.segments_from(0, f64::NAN).is_empty());
    }

    #[test]
    fn segments_follow_deletes_and_fps() {
        let mut p = three();
        assert!(p.delete(0));
        // source seconds, not timeline: deleting the head does not shift them
        let segs = p.segments_from(0, 25.0);
        assert_eq!(segs, vec![(3.0 / 25.0, 0.2), (0.2, 9.0 / 25.0)]);
        let played: f64 = segs.iter().map(|(a, b)| b - a).sum();
        assert!(
            (played - p.timeline_frames() as f64 / 25.0).abs() < 1e-9,
            "segments must cover exactly the timeline: {played}"
        );
    }
}
