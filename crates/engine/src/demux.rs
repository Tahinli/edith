//! MP4 demux: pulls the H.264 track out as Annex-B access units.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use mp4::{MediaType, Mp4Reader, Mp4Track};

use crate::audio::{edit_media_time, packet_at, stts_pairs};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoMeta {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub frame_count: u32,
}

pub struct Demuxer {
    reader: Mp4Reader<BufReader<File>>,
    track_id: u32,
    sample_count: u32,
    /// 1-based sample id of *frame 0*: the edit list can start the presentation
    /// a couple of frames into the media, exactly as it does for audio priming
    /// (`audio::priming_samples`), and frame indices count from there.
    first_sample: u32,
    /// Annex-B SPS+PPS, re-injected ahead of every sync sample.
    parameter_sets: Vec<u8>,
    /// `stss` entries, ascending 1-based sample ids. Empty means no `stss` box
    /// at all, i.e. every sample is a sync sample.
    sync_samples: Vec<u32>,
    next_sample: u32,
}

impl Demuxer {
    pub fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;

        let track = reader
            .tracks()
            .values()
            .find(|t| matches!(t.media_type(), Ok(MediaType::H264)))
            .ok_or("no H.264 video track in file")?;

        let track_id = track.track_id();
        let meta = VideoMeta {
            width: track.width() as u32,
            height: track.height() as u32,
            frame_rate: frame_rate(track),
            frame_count: 0,
        };
        let first_sample = first_frame_sample(stts_pairs(track), trim_ticks(track));
        let mut parameter_sets = Vec::new();
        parameter_sets.extend_from_slice(&START_CODE);
        parameter_sets.extend_from_slice(track.sequence_parameter_set()?);
        parameter_sets.extend_from_slice(&START_CODE);
        parameter_sets.extend_from_slice(track.picture_parameter_set()?);
        let sync_samples = track
            .trak
            .mdia
            .minf
            .stbl
            .stss
            .as_ref()
            .map(|stss| stss.entries.clone())
            .unwrap_or_default();

        let sample_count = reader.sample_count(track_id)?;
        // Where a seek to frame 0 would put the cursor: the samples the edit list
        // trims are still read, they are references for the ones that show.
        let next_sample = sync_at_or_before(&sync_samples, first_sample);
        Ok((
            VideoMeta {
                // The samples the edit list trims off the front are not frames of
                // the presentation, so they are not counted as ones.
                frame_count: sample_count.saturating_sub(first_sample - 1),
                ..meta
            },
            Self {
                reader,
                track_id,
                sample_count,
                first_sample,
                parameter_sets,
                sync_samples,
                next_sample,
            },
        ))
    }

    /// Next access unit in decode order, Annex-B framed. `None` at end of track.
    pub fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
        if self.next_sample > self.sample_count {
            return Ok(None);
        }
        let sample = self.reader.read_sample(self.track_id, self.next_sample)?;
        self.next_sample += 1;
        let Some(sample) = sample else {
            return Ok(None);
        };

        let mut au = Vec::with_capacity(self.parameter_sets.len() + sample.bytes.len() + 16);
        if sample.is_sync {
            au.extend_from_slice(&self.parameter_sets);
        }
        append_annex_b(&sample.bytes, &mut au)?;
        Ok(Some(au))
    }

    /// Rewinds/forwards the read cursor to the latest sync sample at or before
    /// display frame `frame` (0-based), which is the earliest point a decoder can
    /// start from and still reach it. Returns the display index of the first
    /// picture the caller will now be handed -- *negative* when the landing sync
    /// sample sits inside what the edit list trims, i.e. those pictures decode as
    /// references but are not frame 0 or later.
    pub fn seek_to_sync_at_or_before(&mut self, frame: u32) -> i64 {
        let target = frame
            .saturating_add(self.first_sample)
            .clamp(1, self.sample_count.max(1));
        let chosen = sync_at_or_before(&self.sync_samples, target);
        self.next_sample = chosen;
        i64::from(chosen) - i64::from(self.first_sample)
    }
}

/// Frames per second off the sample table. `mp4 0.14`'s own
/// [`Mp4Track::frame_rate`] divides the sample count by whole *milliseconds*
/// before the float cast (`track.rs:166`), so 24000/1001 reads back as a flat
/// 23.0 -- with the audio clock as master that is 4 % of drift, five minutes of
/// it by the end of a two-hour film.
fn frame_rate(track: &Mp4Track) -> f64 {
    fps_from_stts(stts_pairs(track), track.timescale())
}

/// Whole track over whole track: constant-delta tables (the common case) come
/// out as exactly `timescale / delta`, and a table that spreads 3753/3754 ticks
/// to average 3753.75 averages instead of truncating. All of it in `f64`, which
/// is the bug the caller above exists to avoid.
fn fps_from_stts(entries: impl IntoIterator<Item = (u32, u32)>, timescale: u32) -> f64 {
    let (samples, ticks) =
        entries
            .into_iter()
            .fold((0u64, 0u64), |(samples, ticks), (count, delta)| {
                (
                    samples + u64::from(count),
                    ticks + u64::from(count) * u64::from(delta),
                )
            });
    match ticks {
        // No timing in the header at all; `mp4`'s own answer for that is 0.0 too.
        0 => 0.0,
        ticks => samples as f64 * f64::from(timescale) / ticks as f64,
    }
}

/// What the edit list really trims off the front, in media ticks: its
/// `media_time` less the first sample's own composition offset. A stream with
/// B-frames carries a `ctts` delay, and every muxer writes `media_time` equal to
/// exactly that delay -- which is not a trim at all, it is the container saying
/// "sample 1 is still the first picture". Reading it as one drops real frames
/// (`test_high.mp4` loses two, and so did the film this bug came from). What is
/// left over after the delay is the genuine trim.
///
/// ponytail: empty edits are ignored and `media_time` is otherwise taken at face
/// value, which is exactly what [`crate::audio::edit_media_time`] gives the audio
/// track -- symmetry between the two is the point, not a full edit-list engine.
/// Their *empty* edits can differ (83 ms of video against 62 ms of audio in that
/// film, so the picture stays 21 ms -- half a frame -- early); honouring those is
/// the upgrade path, and it belongs to both tracks at once.
fn trim_ticks(track: &Mp4Track) -> Option<u64> {
    let delay = track
        .trak
        .mdia
        .minf
        .stbl
        .ctts
        .as_ref()
        .and_then(|ctts| ctts.entries.first())
        .map_or(0, |e| e.sample_offset.max(0) as u64);
    edit_media_time(track).map(|t| t.saturating_sub(delay))
}

/// 1-based id of the sample the presentation starts on, for a track trimmed by
/// `trim` media ticks ([`trim_ticks`]). `None` (no edit list) and zero are both
/// "no trim", i.e. the first sample.
fn first_frame_sample(entries: impl IntoIterator<Item = (u32, u32)>, trim: Option<u64>) -> u32 {
    trim.map_or(1, |t| packet_at(entries, t, 0).0)
}

/// Largest entry of the ascending sync table that is `<= sample_id`. An empty
/// table means every sample is a sync sample. When `sample_id` sits before the
/// first sync sample there is nothing decodable earlier, so that one wins.
fn sync_at_or_before(syncs: &[u32], sample_id: u32) -> u32 {
    if syncs.is_empty() {
        return sample_id;
    }
    match syncs.partition_point(|&s| s <= sample_id) {
        0 => syncs[0],
        i => syncs[i - 1],
    }
}

/// avcC length-prefixed NALs -> Annex-B. Assumes a 4-byte length prefix, which
/// mp4 0.14 does not expose; a wrong guess misparses immediately, hence the check.
///
/// ponytail: hardcoded 4-byte NAL length prefix (lengthSizeMinusOne == 3). 1/2-byte
/// prefixes exist in the wild; upgrade path is parsing the avcC box from
/// `track.trak` instead of assuming.
fn append_annex_b(mut src: &[u8], out: &mut Vec<u8>) -> crate::Result<()> {
    while !src.is_empty() {
        if src.len() < 4 {
            return Err(format!("truncated NAL length prefix: {} bytes left", src.len()).into());
        }
        let len = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;
        if len == 0 || len > src.len() - 4 {
            return Err(format!(
                "bad NAL length {len} with {} bytes remaining (not a 4-byte-prefixed avcC sample?)",
                src.len() - 4
            )
            .into());
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&src[4..4 + len]);
        src = &src[4 + len..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_two_nals() {
        let mut out = Vec::new();
        append_annex_b(&[0, 0, 0, 2, 0x65, 0xAA, 0, 0, 0, 1, 0x41], &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0xAA, 0, 0, 0, 1, 0x41]);
    }

    #[test]
    fn sync_lookup() {
        assert_eq!(sync_at_or_before(&[], 7), 7, "no stss: every sample syncs");
        assert_eq!(sync_at_or_before(&[1, 31, 61], 31), 31, "exact hit");
        assert_eq!(sync_at_or_before(&[1, 31, 61], 45), 31, "between syncs");
        assert_eq!(sync_at_or_before(&[5, 31], 2), 5, "before the first sync");
        assert_eq!(sync_at_or_before(&[1, 31, 61], 900), 61, "past the last");
    }

    /// The desync bug: NTSC rates must survive the sample table as themselves.
    #[test]
    fn ntsc_frame_rates_come_out_fractional() {
        // 24000/1001 on a 90 kHz clock is 3753.75 ticks a frame, so a muxer
        // spreads 3753/3754 and only the total is exact -- 4 frames, 15015 ticks.
        let ntsc = fps_from_stts([(1u32, 3753u32), (3, 3754)], 90_000);
        assert!(
            (ntsc - 24_000.0 / 1001.0).abs() < 1e-9,
            "23.976 fps read back as {ntsc} (mp4 0.14 says 23.0)"
        );
        // Constant delta stays an exact division, not an average.
        assert_eq!(fps_from_stts([(300u32, 3000u32)], 90_000), 30.0);
        assert_eq!(
            fps_from_stts([(120u32, 1001u32)], 30_000),
            30_000.0 / 1001.0
        );
        assert_eq!(fps_from_stts([(0u32, 0u32)], 90_000), 0.0, "no timing");
    }

    /// A video edit list trims the head of the media exactly as the audio one
    /// does; frame 0 is the first frame that is *shown*. The B-frame delay is
    /// the case that must **not** move it -- that one is a lie of a `media_time`
    /// and taking it literally throws two real frames away.
    #[test]
    fn a_video_edit_list_moves_frame_zero_but_a_ctts_delay_does_not() {
        let entries = [(1u32, 3753u32), (3, 3754)];
        // The film's own numbers: media_time 7507 at 90 kHz, and a first ctts
        // offset of 7507 to go with it -- nothing trimmed.
        assert_eq!(first_frame_sample(entries, Some(7507 - 7507)), 1);
        // Two frames genuinely cut off the front.
        assert_eq!(first_frame_sample(entries, Some(7507)), 3);
        assert_eq!(
            first_frame_sample(entries, None),
            1,
            "no edit list, no trim"
        );
        assert_eq!(first_frame_sample(entries, Some(0)), 1, "zero is no trim");
    }

    #[test]
    fn rejects_misparse() {
        assert!(append_annex_b(&[0, 0, 0, 9, 0x65], &mut Vec::new()).is_err());
        assert!(append_annex_b(&[0, 0, 0], &mut Vec::new()).is_err());
        assert!(append_annex_b(&[0, 0, 0, 0], &mut Vec::new()).is_err());
    }
}
