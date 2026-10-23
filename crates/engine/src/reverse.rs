//! Walking a clip's frames backwards without holding the clip in memory.
//!
//! A video codec only decodes forwards: a picture needs the ones before it, back to
//! the last sync frame. So a reversed clip is served a *block* at a time -- decode a
//! stretch of the window forwards, hand it out last to first, then the stretch
//! before it -- and memory is bounded by the block, not the clip. The blocks are cut
//! on sync frames where the container says where they are (one GOP is then decoded
//! once, not once per picture), and capped at [`BLOCK_BUDGET`] bytes of decoded
//! pictures: a GOP longer than the cap is decoded again from its sync frame for each
//! of its blocks, which is slower but never larger.
//!
//! This module is only the plan -- which blocks, in which order. The decoding and
//! the pictures' type belong to the caller (playback walks BGRA canvases, export
//! I420 planes), so both serve a reversed clip by the one rule.

use std::path::Path;

/// What one block of decoded pictures may weigh. A 1080p BGRA canvas is 8 MB, so
/// this is thirty-odd of them; 4K takes eight.
pub(crate) const BLOCK_BUDGET: usize = 256 << 20;

/// The most pictures one block may hold when each is `bytes` big: never fewer than
/// two, so a block always makes progress, and never more than a hundred and twenty.
pub(crate) fn block_cap(bytes: usize) -> u32 {
    (BLOCK_BUDGET / bytes.max(1)).clamp(2, 120) as u32
}

/// Every frame of `path`'s picture a decoder may be started on, when that is cheap
/// to ask: an mp4 answers off its sync table, while a Matroska file costs a walk of
/// its clusters (seconds on a big film), so it answers nothing and its blocks are
/// cut without. Empty is always a legal answer; the plan is merely less tidy.
pub(crate) fn sync_frames(path: &Path) -> Vec<u32> {
    if crate::demux::is_matroska(path) {
        return Vec::new();
    }
    crate::demux::sync_points(path)
}

/// The walk down a window of source frames: [`Walk::next`] hands out the blocks
/// `[start, end)` from the window's last frame to its first.
pub(crate) struct Walk {
    sync: Vec<u32>,
    lo: u32,
    end: u32,
    cap: u32,
}

impl Walk {
    /// The blocks of the half-open window `lo..hi`, at most `cap` pictures each,
    /// cut on the frames of `sync` (sorted) where they can be.
    pub(crate) fn new(sync: Vec<u32>, lo: u32, hi: u32, cap: u32) -> Self {
        Self { sync, lo, end: hi.max(lo), cap: cap.max(1) }
    }

    /// Changes the cap for the blocks still to come: the first picture says how big
    /// the pictures really are.
    pub(crate) fn set_cap(&mut self, cap: u32) {
        self.cap = cap.max(1);
    }

    /// The next block `(start, end)`, below the one before it; `None` at the window's
    /// first frame. A block starts on the earliest sync frame that keeps it within the
    /// cap, so a GOP is whole in one block whenever it fits; with none that does, it
    /// is the `cap` frames below `end`, and the decoder runs up to them from the sync
    /// frame before.
    pub(crate) fn next(&mut self) -> Option<(u32, u32)> {
        if self.end <= self.lo {
            return None;
        }
        let end = self.end;
        let floor = self.lo.max(end.saturating_sub(self.cap));
        let at = self.sync.partition_point(|&s| s < floor);
        let start = match self.sync.get(at) {
            Some(&s) if s < end => s,
            _ => floor,
        };
        self.end = start;
        Some((start, end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(sync: &[u32], lo: u32, hi: u32, cap: u32) -> Vec<(u32, u32)> {
        let mut walk = Walk::new(sync.to_vec(), lo, hi, cap);
        std::iter::from_fn(|| walk.next()).collect()
    }

    #[test]
    fn blocks_tile_the_window_from_the_top_down() {
        let sync: Vec<u32> = (0..100).step_by(10).collect();
        let got = blocks(&sync, 3, 95, 25);
        // Contiguous, descending, covering exactly 3..95.
        assert_eq!(got.first().map(|b| b.1), Some(95));
        assert_eq!(got.last().map(|b| b.0), Some(3));
        for pair in got.windows(2) {
            assert_eq!(pair[1].1, pair[0].0, "no gap and no overlap between {pair:?}");
        }
        assert!(got.iter().all(|&(s, e)| e > s && e - s <= 25), "{got:?}");
    }

    #[test]
    fn a_block_starts_on_a_sync_frame_when_one_fits() {
        let sync = [0, 10, 20, 30, 40];
        // end 50, cap 25: the earliest sync frame in 25..50 is 30.
        assert_eq!(blocks(&sync, 0, 50, 25)[0], (30, 50));
        // Then 5..30 holds 10 and 20; the earliest in range is 10.
        assert_eq!(blocks(&sync, 0, 50, 25)[1], (10, 30));
    }

    #[test]
    fn a_gop_longer_than_the_cap_is_cut_by_the_cap() {
        let got = blocks(&[0, 100], 0, 100, 30);
        assert_eq!(got, vec![(70, 100), (40, 70), (10, 40), (0, 10)]);
    }

    #[test]
    fn no_sync_table_still_walks_in_capped_blocks() {
        assert_eq!(blocks(&[], 5, 20, 8), vec![(12, 20), (5, 12)]);
    }

    #[test]
    fn an_all_intra_file_does_not_make_blocks_of_one() {
        let sync: Vec<u32> = (0..64).collect();
        let got = blocks(&sync, 0, 64, 16);
        assert_eq!(got.len(), 4, "{got:?}");
    }

    #[test]
    fn empty_and_single_frame_windows() {
        assert!(blocks(&[], 7, 7, 4).is_empty());
        assert_eq!(blocks(&[], 7, 8, 4), vec![(7, 8)]);
    }

    #[test]
    fn the_cap_follows_the_picture_size() {
        assert_eq!(block_cap(1920 * 1080 * 4), 32);
        assert_eq!(block_cap(3840 * 2160 * 4), 8);
        assert_eq!(block_cap(usize::MAX), 2);
        assert_eq!(block_cap(1), 120);
    }
}
