//! MP4 demux: pulls the video track out as access units the decoders take --
//! Annex-B for H.264 and HEVC, the sample bytes untouched for VP9 (an mp4 VP9
//! sample is a self-contained superframe already).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use mp4::{MediaType, Mp4Reader, Mp4Track};

use crate::audio::{edit_media_time, packet_at, stts_pairs};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// What the video track is coded with. Not a decoder choice by itself: only
/// H.264 has a software decoder here, which is what [`Codec::needs_plugin`]
/// says for the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Vp9,
}

impl Codec {
    /// How a refusal names it, and how a mismatch reads to a user.
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Vp9 => "VP9",
        }
    }

    /// Why a file can be refused outright: `rusty_h264` is the only software
    /// decoder in the project and there is no pure-Rust HEVC or VP9 one to fall
    /// back to, so without the plugin there is nothing to decode with. Shared
    /// so playback and export refuse in the same words.
    pub fn needs_plugin(self) -> String {
        format!(
            "{name} needs the VA-API plugin (libengine_hw.so) — there is no software {name} decoder",
            name = self.name()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoMeta {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub frame_count: u32,
    pub codec: Codec,
}

pub struct Demuxer {
    reader: Mp4Reader<BufReader<File>>,
    track_id: u32,
    sample_count: u32,
    /// 1-based sample id of *frame 0*: the edit list can start the presentation
    /// a couple of frames into the media, exactly as it does for audio priming
    /// (`audio::priming_samples`), and frame indices count from there.
    first_sample: u32,
    codec: Codec,
    /// Annex-B parameter sets (SPS+PPS for H.264, VPS+SPS+PPS for HEVC),
    /// re-injected ahead of every sync sample. Empty for VP9, which carries no
    /// out-of-band parameter sets.
    parameter_sets: Vec<u8>,
    /// Bytes of the NAL length prefix each sample is written with, off `avcC`
    /// or `hvcC`. Unused by VP9.
    nal_length: usize,
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
            .find_map(|t| match t.media_type() {
                Ok(MediaType::H264) => Some((t, Codec::H264)),
                Ok(MediaType::H265) => Some((t, Codec::Hevc)),
                Ok(MediaType::VP9) => Some((t, Codec::Vp9)),
                _ => None,
            })
            .ok_or("no H.264, HEVC or VP9 video track in file")?;
        let (track, codec) = track;

        let track_id = track.track_id();
        let meta = VideoMeta {
            width: track.width() as u32,
            height: track.height() as u32,
            frame_rate: frame_rate(track),
            frame_count: 0,
            codec,
        };
        let first_sample = first_frame_sample(stts_pairs(track), trim_ticks(track));
        let mut parameter_sets = Vec::new();
        let mut nal_length = 4;
        match codec {
            Codec::H264 => {
                let avcc = &track
                    .trak
                    .mdia
                    .minf
                    .stbl
                    .stsd
                    .avc1
                    .as_ref()
                    .ok_or("an H.264 track without an avc1 sample entry")?
                    .avcc;
                nal_length = usize::from(avcc.length_size_minus_one) + 1;
                parameter_sets.extend_from_slice(&START_CODE);
                parameter_sets.extend_from_slice(track.sequence_parameter_set()?);
                parameter_sets.extend_from_slice(&START_CODE);
                parameter_sets.extend_from_slice(track.picture_parameter_set()?);
            }
            // `mp4 0.14` reads one byte of `hvcC` and skips the rest
            // (`hev1.rs:193`), so the parameter sets an HEVC decoder cannot
            // start without are read out of the file by hand.
            Codec::Hevc => {
                let hvcc = hvcc_record(path, track_id)?;
                let (len, sets) = parse_hvcc(&hvcc)?;
                nal_length = len;
                parameter_sets = sets;
            }
            Codec::Vp9 => {}
        }
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
                codec,
                parameter_sets,
                nal_length,
                sync_samples,
                next_sample,
            },
        ))
    }

    /// Next access unit in decode order: Annex-B framed for H.264 and HEVC, the
    /// mp4 sample verbatim for VP9. `None` at end of track.
    pub fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
        if self.next_sample > self.sample_count {
            return Ok(None);
        }
        let sample = self.reader.read_sample(self.track_id, self.next_sample)?;
        self.next_sample += 1;
        let Some(sample) = sample else {
            return Ok(None);
        };
        // A VP9 mp4 sample is one (super)frame the decoder parses on its own:
        // no length prefixes to strip, no parameter sets to re-inject.
        if self.codec == Codec::Vp9 {
            return Ok(Some(sample.bytes.to_vec()));
        }

        let mut au = Vec::with_capacity(self.parameter_sets.len() + sample.bytes.len() + 16);
        if sample.is_sync {
            au.extend_from_slice(&self.parameter_sets);
        }
        append_annex_b(&sample.bytes, self.nal_length, &mut au)?;
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

/// Length-prefixed NALs (`avcC` or `hvcC` framing) -> Annex-B. `n` is the
/// prefix width the sample entry declares, 1..=4; a wrong one misparses
/// immediately, hence the check.
fn append_annex_b(mut src: &[u8], n: usize, out: &mut Vec<u8>) -> crate::Result<()> {
    while !src.is_empty() {
        if src.len() < n {
            return Err(format!("truncated NAL length prefix: {} bytes left", src.len()).into());
        }
        let len = src[..n]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | b as usize);
        if len == 0 || len > src.len() - n {
            return Err(format!(
                "bad NAL length {len} with {} bytes remaining (not a {n}-byte-prefixed sample?)",
                src.len() - n
            )
            .into());
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&src[n..n + len]);
        src = &src[n + len..];
    }
    Ok(())
}

/// The `hvcC` payload of `track_id`'s sample entry, read straight out of the
/// file: `mp4 0.14`'s `HvcCBox` keeps the configuration version and skips
/// everything after it (`hev1.rs:193`), so the VPS/SPS/PPS an HEVC decoder
/// cannot start without are unreachable through the crate.
fn hvcc_record(path: &Path, track_id: u32) -> crate::Result<Vec<u8>> {
    let moov = read_top_level(path, b"moov")?.ok_or("no moov box in file")?;
    let trak = boxes(&moov)
        .filter(|(kind, _)| *kind == b"trak")
        .find(|(_, payload)| child(payload, b"tkhd").and_then(tkhd_track_id) == Some(track_id))
        .ok_or("the HEVC track has no trak box")?
        .1;
    let stsd = child(trak, b"mdia")
        .and_then(|b| child(b, b"minf"))
        .and_then(|b| child(b, b"stbl"))
        .and_then(|b| child(b, b"stsd"))
        .ok_or("the HEVC track has no stsd box")?;
    // stsd is a FullBox (4) plus entry_count (4); then sample entries, whose
    // VisualSampleEntry header is a fixed 78 bytes before the child boxes.
    let entry = boxes(stsd.get(8..).unwrap_or_default())
        .next()
        .ok_or("empty stsd box")?
        .1;
    let hvcc = child(entry.get(78..).unwrap_or_default(), b"hvcC")
        .ok_or("no hvcC box in the HEVC sample entry")?;
    Ok(hvcc.to_vec())
}

/// HEVCDecoderConfigurationRecord (ISO 14496-15 §8.3.3.1) -> the NAL length
/// prefix width and the VPS/SPS/PPS arrays as one Annex-B blob.
fn parse_hvcc(rec: &[u8]) -> crate::Result<(usize, Vec<u8>)> {
    // 0 configurationVersion .. 21 lengthSizeMinusOne, 22 numOfArrays.
    let (&flags, &arrays) = rec
        .get(21)
        .zip(rec.get(22))
        .ok_or("hvcC box shorter than its fixed header")?;
    // 17 is bit_depth_luma_minus8 in its low 3 bits: the plugin's NV12 pool and
    // its vaGetImage read-back are 8-bit, so a Main 10 stream would decode into
    // a surface nothing here can carry -- refused by name instead of shown as
    // garbage. Upgrade path: a P010 pool chosen from the stream info.
    if rec[17] & 0x7 != 0 {
        return Err(format!(
            "10-bit HEVC (Main 10) is not supported yet — this file is {}-bit",
            8 + (rec[17] & 0x7)
        )
        .into());
    }
    let mut sets = Vec::new();
    let mut src = &rec[23..];
    for _ in 0..arrays {
        let (&head, rest) = src.split_first().ok_or("hvcC array header past the box")?;
        let count = usize::from(u16::from_be_bytes(
            rest.get(..2)
                .ok_or("hvcC array count past the box")?
                .try_into()
                .unwrap(),
        ));
        src = &rest[2..];
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes(
                src.get(..2)
                    .ok_or("hvcC NAL length past the box")?
                    .try_into()
                    .unwrap(),
            ));
            let nal = src.get(2..2 + len).ok_or("hvcC NAL past the box")?;
            // 32 VPS, 33 SPS, 34 PPS -- the rest (SEI) a decoder does not need
            // to start, and cros-codecs would only skip.
            if matches!(head & 0x3f, 32 | 33 | 34) {
                sets.extend_from_slice(&START_CODE);
                sets.extend_from_slice(nal);
            }
            src = &src[2 + len..];
        }
    }
    if sets.is_empty() {
        return Err("hvcC box carries no VPS/SPS/PPS".into());
    }
    Ok((usize::from(flags & 0x3) + 1, sets))
}

/// Payload of the first top-level box of type `want`, or `None` if the file has
/// none. Only the box wanted is read; the rest is seeked over, so a `moov` at
/// the end of a two-hour film costs one seek and not a copy of the file.
fn read_top_level(path: &Path, want: &[u8; 4]) -> crate::Result<Option<Vec<u8>>> {
    let mut file = File::open(path)?;
    let end = file.metadata()?.len();
    let mut at = 0u64;
    while at + 8 <= end {
        let mut header = [0u8; 8];
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut header)?;
        let (size, header_len) = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => (end - at, 8),
            1 => {
                let mut large = [0u8; 8];
                file.read_exact(&mut large)?;
                (u64::from_be_bytes(large), 16)
            }
            size => (u64::from(size), 8),
        };
        if size < header_len || at + size > end {
            return Err("truncated box in the file's top level".into());
        }
        if &header[4..8] == want {
            let mut payload = vec![0u8; (size - header_len) as usize];
            file.seek(SeekFrom::Start(at + header_len))?;
            file.read_exact(&mut payload)?;
            return Ok(Some(payload));
        }
        at += size;
    }
    Ok(None)
}

/// The ISO-BMFF boxes in `buf`, as (type, payload). Stops at the first
/// malformed header rather than erroring: every caller is looking for one box
/// and reports its own absence.
fn boxes(mut buf: &[u8]) -> impl Iterator<Item = (&[u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        let header: &[u8; 8] = buf.get(..8)?.try_into().ok()?;
        let (size, header_len) = match u32::from_be_bytes(header[..4].try_into().unwrap()) as usize
        {
            0 => (buf.len(), 8),
            1 => (
                u64::from_be_bytes(buf.get(8..16)?.try_into().ok()?) as usize,
                16,
            ),
            size => (size, 8),
        };
        if size < header_len || size > buf.len() {
            return None;
        }
        let (this, rest) = buf.split_at(size);
        buf = rest;
        Some((header[4..8].try_into().unwrap(), &this[header_len..]))
    })
}

/// Payload of the first child box of type `want`.
fn child<'a>(buf: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(buf).find(|(kind, _)| *kind == want).map(|(_, p)| p)
}

/// `track_id` out of a `tkhd` payload: a FullBox whose creation and
/// modification times are 32-bit in version 0 and 64-bit in version 1.
fn tkhd_track_id(payload: &[u8]) -> Option<u32> {
    let at = if payload.first() == Some(&1) { 20 } else { 12 };
    Some(u32::from_be_bytes(
        payload.get(at..at + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_two_nals() {
        let mut out = Vec::new();
        append_annex_b(&[0, 0, 0, 2, 0x65, 0xAA, 0, 0, 0, 1, 0x41], 4, &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0xAA, 0, 0, 0, 1, 0x41]);
        // The same two NALs written with a 2-byte prefix, which is what a
        // `hvcC`/`avcC` with lengthSizeMinusOne == 1 declares.
        let mut narrow = Vec::new();
        append_annex_b(&[0, 2, 0x65, 0xAA, 0, 1, 0x41], 2, &mut narrow).unwrap();
        assert_eq!(narrow, out, "the prefix width is the only difference");
    }

    /// The `hvcC` parse: prefix width out of byte 21 and VPS/SPS/PPS out of the
    /// arrays, with an SEI array in between that must not join them.
    #[test]
    fn reads_parameter_sets_out_of_an_hvcc_record() {
        let mut rec = vec![0u8; 23];
        rec[0] = 1; // configurationVersion
        rec[21] = 0xf0 | 0x1; // lengthSizeMinusOne == 1, high bits reserved
        rec[22] = 4; // numOfArrays
        for (kind, nal) in [(32u8, 0xAAu8), (33, 0xBB), (39, 0xCC), (34, 0xDD)] {
            rec.push(0x80 | kind); // array_completeness set
            rec.extend_from_slice(&1u16.to_be_bytes()); // numNalus
            rec.extend_from_slice(&2u16.to_be_bytes()); // nalUnitLength
            rec.extend_from_slice(&[kind << 1, nal]);
        }
        let (len, sets) = parse_hvcc(&rec).unwrap();
        assert_eq!(len, 2);
        assert_eq!(
            sets,
            [
                0, 0, 0, 1, 64, 0xAA, // VPS
                0, 0, 0, 1, 66, 0xBB, // SPS
                0, 0, 0, 1, 68, 0xDD, // PPS -- the SEI array is skipped
            ]
        );

        // 10-bit is refused by name rather than decoded into an 8-bit surface.
        let mut main10 = rec.clone();
        main10[17] = 0xf8 | 2;
        let refused = parse_hvcc(&main10).unwrap_err().to_string();
        assert!(refused.contains("10-bit"), "{refused}");
        // Nothing is read past the end of a truncated box.
        assert!(parse_hvcc(&rec[..20]).is_err());
        assert!(parse_hvcc(&rec[..rec.len() - 1]).is_err());
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
        assert!(append_annex_b(&[0, 0, 0, 9, 0x65], 4, &mut Vec::new()).is_err());
        assert!(append_annex_b(&[0, 0, 0], 4, &mut Vec::new()).is_err());
        assert!(append_annex_b(&[0, 0, 0, 0], 4, &mut Vec::new()).is_err());
    }
}
