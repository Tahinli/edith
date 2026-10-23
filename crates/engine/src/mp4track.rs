//! Edith's access to an mp4 track's audio, over `ec-mp4`'s demuxer.
//!
//! The audio readers ask an mp4 the same few questions -- which tracks are
//! audio, what each one's config says, how long its sample table is, where its
//! edit list starts the presentation -- and this is where those answers come
//! from. Stream numbers are `ec-mp4`'s: [`ec_mp4::TrackInfo::stream`] for a
//! track, [`ec_mp4::Mp4Demuxer::samples`] for its table.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use ec_core::{CodecId, Demuxer as _, MediaParameters, MediaType, Packet, StreamInfo};
use ec_mp4::Mp4Demuxer;

/// An mp4 file open for reading: its sample tables are its index, and sample
/// bytes are read on demand.
pub(crate) type Reader = Mp4Demuxer<BufReader<File>>;

/// Opens `path` as an mp4 (or QuickTime) file and reads its movie header and
/// sample tables.
pub(crate) fn open(path: &Path) -> crate::Result<Reader> {
    crate::demux::refuse_asf(path)?;
    let file = File::open(path)?;
    Mp4Demuxer::new(BufReader::new(file)).map_err(crate::demux::mp4_error)
}

/// The audio tracks of `reader`, in file order (the order a stream index
/// counts in), as `(track id, stream)`. `stream` is [`None`] for a track this
/// build has no codec for. Read from the `moov`, not a `HashMap`, so "stream 0"
/// is the same track from one run to the next.
pub(crate) fn audio_tracks(reader: &Reader) -> Vec<(u32, Option<u32>)> {
    reader
        .tracks()
        .iter()
        .filter(|t| t.media == Some(MediaType::Audio))
        .map(|t| (t.track_id, t.stream))
        .collect()
}

fn stream_info(reader: &Reader, stream: u32) -> Option<&StreamInfo> {
    reader.streams().iter().find(|s| s.index == stream)
}

/// The codec of `stream`, [`None`] for a stream that does not exist.
pub(crate) fn codec(reader: &Reader, stream: u32) -> Option<CodecId> {
    stream_info(reader, stream).map(|s| s.params.codec)
}

/// The fourcc of `track_id`'s first sample entry: `mp4a`, `ac-3`, `alac`, ...
pub(crate) fn sample_entry(reader: &Reader, track_id: u32) -> [u8; 4] {
    reader
        .tracks()
        .iter()
        .find(|t| t.track_id == track_id)
        .map_or([0; 4], |t| t.sample_entry)
}

/// The sample rate of an audio `stream`, in Hz (for AAC, the rate the decoder
/// produces: the SBR extension rate where the config states one).
pub(crate) fn sample_rate(reader: &Reader, stream: u32) -> u32 {
    match stream_info(reader, stream).map(|s| &s.params.media) {
        Some(MediaParameters::Audio(a)) => a.sample_rate,
        _ => 0,
    }
}

/// The channel count of an audio `stream`, from its config.
pub(crate) fn channels(reader: &Reader, stream: u32) -> usize {
    match stream_info(reader, stream).map(|s| &s.params.media) {
        Some(MediaParameters::Audio(a)) => a.layout.channel_count(),
        _ => 0,
    }
}

/// The codec configuration bytes of `stream` as the file states them: an AAC
/// track's `AudioSpecificConfig` verbatim, extension frequency and all.
pub(crate) fn extradata(reader: &Reader, stream: u32) -> Option<Box<[u8]>> {
    stream_info(reader, stream)
        .and_then(|s| s.params.extradata.as_deref())
        .map(Box::from)
}

/// Ticks per second of `stream`'s time base: its media timescale.
pub(crate) fn timescale(reader: &Reader, stream: u32) -> u32 {
    stream_info(reader, stream).map_or(1, |s| {
        let (num, den) = (s.time_base.num().max(1), s.time_base.den());
        u32::try_from(den / num).unwrap_or(u32::MAX).max(1)
    })
}

/// The `mdhd` duration of `stream`, in its timescale, [`None`] where the file
/// states none.
pub(crate) fn duration(reader: &Reader, stream: u32) -> Option<u64> {
    stream_info(reader, stream)?
        .duration
        .and_then(|d| u64::try_from(d).ok())
}

/// The ISO 639-2 language of `track_id`, [`None`] for the `und` a muxer writes
/// when it was never told.
pub(crate) fn language(reader: &Reader, track_id: u32) -> Option<String> {
    reader
        .tracks()
        .iter()
        .find(|t| t.track_id == track_id)
        .and_then(|t| t.language.clone())
}

/// `media_time` of `track_id`'s first real edit-list entry, in its timescale:
/// where the presentation starts inside the media. [`None`] with no edit list,
/// or only empty entries, which shift the timeline rather than trim the media.
pub(crate) fn media_time(reader: &Reader, track_id: u32) -> Option<u64> {
    reader
        .tracks()
        .iter()
        .find(|t| t.track_id == track_id)
        .and_then(|t| t.media_time)
        .and_then(|t| u64::try_from(t).ok())
}

/// How many samples `stream` has.
pub(crate) fn sample_count(reader: &Reader, stream: u32) -> u32 {
    u32::try_from(reader.samples(stream).len()).unwrap_or(u32::MAX)
}

/// The bytes of `stream`'s samples in all: what its byte rate is measured from.
pub(crate) fn sample_bytes(reader: &Reader, stream: u32) -> u64 {
    reader
        .samples(stream)
        .iter()
        .map(|s| u64::from(s.size))
        .sum()
}

/// The decode durations of `stream` as `(sample count, duration)` runs, the
/// shape of an `stts` table, for [`crate::audio::packet_at`].
pub(crate) fn stts(reader: &Reader, stream: u32) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for sample in reader.samples(stream) {
        match runs.last_mut() {
            Some((count, delta)) if *delta == sample.duration => *count += 1,
            _ => runs.push((1, sample.duration)),
        }
    }
    runs
}

/// Sample `id` of `stream`, counted from 1 as an mp4 counts them, read without
/// moving any other cursor. [`None`] past the end of the track.
pub(crate) fn sample(reader: &mut Reader, stream: u32, id: u32) -> crate::Result<Option<Packet>> {
    let Some(index) = id.checked_sub(1) else {
        return Ok(None);
    };
    match reader.read_sample(stream, index as usize) {
        Ok(packet) => Ok(Some(packet)),
        Err(ec_core::Error::Eof) => Ok(None),
        Err(e) => Err(crate::demux::mp4_error(e)),
    }
}
