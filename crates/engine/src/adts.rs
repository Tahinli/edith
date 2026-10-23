//! A standalone ADTS file (`.aac`: AAC frames, each behind its own header) as a
//! symphonia format reader.
//!
//! symphonia ships this reader inside its `aac` feature, and that feature is
//! the codec crate this project no longer links. So the reader is here, and it
//! is small: a frame is a 7- or 9-byte header and a payload, and the header
//! says how long the frame is. What it hands on is the whole frame, header
//! included, because `ec-aac` decodes ADTS from its own headers and needs no
//! config of its own to do so.
//!
//! Frames are counted when the file is opened, so the track has its length
//! the way an mp4 does and a seek can land on an exact frame.

use std::io::{Read, Seek, SeekFrom};
use std::num::NonZeroU32;

use symphonia_core::audio::Channels;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::errors::{Error, Result, SeekErrorKind, decode_error};
use symphonia_core::formats::well_known::FORMAT_ID_ADTS;
use symphonia_core::formats::{
    FormatInfo, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::packet::Packet;
use symphonia_core::units::{Duration, TimeBase, Timestamp};

/// Samples per channel in one AAC-LC raw data block, which is what an ADTS
/// frame carries per block. Fixed by the codec.
const SAMPLES_PER_BLOCK: i64 = 1024;

/// Longest ADTS header there is: seven bytes, or nine with the CRC.
const MAX_HEADER: usize = 9;

const ADTS_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_ADTS,
    short_name: "aac",
    long_name: "Audio Data Transport Stream (native AAC)",
};

/// An ADTS file, read from its first frame to its last.
pub(crate) struct AdtsReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    /// Byte offset of the first frame, the place a seek back to the start goes.
    first_frame_pos: u64,
    /// Timestamp, in samples, of the frame the reader is positioned at.
    next_ts: i64,
}

impl<'s> AdtsReader<'s> {
    /// The reader over `mss`, which must be seekable: a file, not a pipe. The
    /// first frame's header says what the track is, and the whole file is
    /// walked once to count its frames.
    pub(crate) fn try_new(mut mss: MediaSourceStream<'s>) -> Result<Self> {
        let first_frame_pos = mss.stream_position()?;
        let Some(first) = next_header(&mut mss)? else {
            return decode_error("adts: no frame header");
        };
        let rate = NonZeroU32::new(first.sample_rate)
            .ok_or(Error::DecodeError("adts: zero sample rate"))?;

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_sample_rate(first.sample_rate);
        if first.channels > 0 {
            params.with_channels(Channels::Discrete(first.channels));
        }

        let mut track = Track::new(0);
        track
            .with_codec_params(CodecParameters::Audio(params))
            .with_time_base(TimeBase::new(NonZeroU32::MIN, rate));

        let frames = count_frames(&mut mss)?;
        track.with_num_frames(frames);
        track.with_duration(Duration::new(frames));

        Ok(AdtsReader {
            media_info: MediaInfo::from_track(&track),
            tracks: vec![track],
            reader: mss,
            metadata: MetadataLog::default(),
            first_frame_pos,
            next_ts: 0,
        })
    }
}

/// The header of the frame at the reader's position, which is left where it
/// was. `None` at the end of the stream, or on a tail too short to be a header.
fn next_header(reader: &mut MediaSourceStream<'_>) -> Result<Option<ec_aac::AdtsHeader>> {
    let at = reader.stream_position()?;
    let mut buf = [0u8; MAX_HEADER];
    let mut got = 0;
    while got < MAX_HEADER {
        match reader.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    if got < 7 {
        return Ok(None);
    }
    let header = ec_aac::parse_adts(&buf[..got])
        .map_err(|_| Error::DecodeError("adts: invalid frame header"))?;
    if header.frame_length < header.header_len {
        return decode_error("adts: frame shorter than its header");
    }
    reader.seek(SeekFrom::Start(at))?;
    Ok(Some(header))
}

/// Samples per channel an ADTS frame carries: one block of 1024 per raw data
/// block it holds.
fn frame_samples(header: &ec_aac::AdtsHeader) -> i64 {
    SAMPLES_PER_BLOCK * i64::from(header.raw_blocks.max(1))
}

/// Every frame in the file, counted by walking the headers alone, as its length
/// in samples per channel. The reader is left where it was.
fn count_frames(reader: &mut MediaSourceStream<'_>) -> Result<u64> {
    let start = reader.stream_position()?;
    let mut total = 0u64;
    while let Some(header) = next_header(reader)? {
        total += frame_samples(&header) as u64;
        reader.seek(SeekFrom::Current(header.frame_length as i64))?;
    }
    reader.seek(SeekFrom::Start(start))?;
    Ok(total)
}

impl FormatReader for AdtsReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &ADTS_FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        let required = match to {
            SeekTo::Timestamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => self.tracks[0]
                .time_base
                .and_then(|tb| tb.calc_timestamp(time))
                .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?,
        };
        let required_ts = required.get();

        // Behind the reader: go back to the first frame and walk from there.
        if required_ts < self.next_ts {
            self.reader.seek(SeekFrom::Start(self.first_frame_pos))?;
            self.next_ts = 0;
        }

        // Skip whole frames while the next one still starts at or before the
        // target, then stand on the frame that contains it.
        while let Some(header) = next_header(&mut self.reader)? {
            let samples = frame_samples(&header);
            if self.next_ts + samples > required_ts {
                break;
            }
            self.reader.seek(SeekFrom::Current(header.frame_length as i64))?;
            self.next_ts += samples;
        }

        Ok(SeekedTo {
            track_id: 0,
            required_ts: required,
            actual_ts: Timestamp::new(self.next_ts),
        })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let Some(header) = next_header(&mut self.reader)? else {
            return Ok(None);
        };
        let mut frame = vec![0u8; header.frame_length];
        self.reader.read_exact(&mut frame)?;
        let samples = frame_samples(&header);
        let ts = self.next_ts;
        self.next_ts += samples;
        Ok(Some(Packet::new(
            0,
            Timestamp::new(ts),
            Duration::new(samples as u64),
            frame,
        )))
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
