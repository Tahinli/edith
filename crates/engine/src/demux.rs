//! Container demux: pulls the video track out as access units the decoders take
//! -- Annex-B for H.264 and HEVC, the sample bytes untouched for VP9 (an mp4 VP9
//! sample is a self-contained superframe already) and for AV1, whose Matroska
//! block is one temporal unit in the low-overhead OBU format the decoder reads.
//!
//! Two containers, one interface: mp4 through the `mp4` crate, Matroska
//! (`.mkv`/`.webm`) walked here as EBML. AV1 is why the second one exists --
//! `mp4 0.14` has no `av01` sample entry at all, and AV1 ships in Matroska in
//! practice -- and HEVC rides the same walk, because that is the other codec a
//! `.mkv` off a disc arrives in. An AV1 *mp4* is read here too, its sample entry
//! picked out of the `stsd` by hand for the same reason `mux` writes it by hand:
//! this project exports one, and a file this cannot reopen is a file it had no
//! business writing.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use mp4::{MediaType, Mp4Reader, Mp4Track, TrackType};

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
    Av1,
}

impl Codec {
    /// How a refusal names it, and how a mismatch reads to a user.
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Vp9 => "VP9",
            Self::Av1 => "AV1",
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

/// The file, whichever container it came in. Which one is decided by the
/// extension at [`Demuxer::open`] and never again: everything downstream --
/// playback, export, the plugin -- speaks access units and display frames.
pub enum Demuxer {
    Mp4(Mp4Demuxer),
    Mkv(MkvDemuxer),
}

impl Demuxer {
    pub fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
        if is_matroska(path) {
            let (meta, mkv) = MkvDemuxer::open(path)?;
            return Ok((meta, Self::Mkv(mkv)));
        }
        let (meta, mp4) = Mp4Demuxer::open(path)?;
        Ok((meta, Self::Mp4(mp4)))
    }

    /// Next access unit in decode order, `None` at end of track.
    pub fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
        match self {
            Self::Mp4(d) => d.next_access_unit(),
            Self::Mkv(d) => d.next_access_unit(),
        }
    }

    /// Rewinds/forwards the read cursor to the latest sync sample at or before
    /// display frame `frame` (0-based), which is the earliest point a decoder
    /// can start from and still reach it. Returns the display index of the
    /// first picture the caller will now be handed.
    pub fn seek_to_sync_at_or_before(&mut self, frame: u32) -> i64 {
        match self {
            Self::Mp4(d) => d.seek_to_sync_at_or_before(frame),
            Self::Mkv(d) => d.seek_to_sync_at_or_before(frame),
        }
    }

    /// Bits per luma sample the stream is coded at: 8 for everything but an
    /// HEVC Main 10 or a 10-bit AV1 track, which decode into a P010 surface
    /// rather than an NV12 one. Not part of [`VideoMeta`] because nothing above
    /// the plugin has a use for it -- what comes out of the read-back is 8-bit
    /// either way.
    pub fn bit_depth(&self) -> u8 {
        match self {
            Self::Mp4(d) => d.bit_depth,
            Self::Mkv(d) => d.bit_depth,
        }
    }
}

/// Whether a path names a Matroska file, which is where AV1 arrives and where a
/// film off a disc arrives as HEVC. Extension only, like [`crate::is_audio`]:
/// the demuxer is what really decides, but the audio path has to know before it
/// opens anything that this file's sound is read by symphonia's `mkv` reader
/// and not by either mp4 one (see [`crate::audio::Track::open`]).
pub fn is_matroska(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mkv" | "webm"))
}

pub struct Mp4Demuxer {
    reader: Mp4Reader<BufReader<File>>,
    track_id: u32,
    sample_count: u32,
    /// 1-based sample id of *frame 0*: the edit list can start the presentation
    /// a couple of frames into the media, exactly as it does for audio priming
    /// (`audio::priming_samples`), and frame indices count from there.
    first_sample: u32,
    codec: Codec,
    /// Annex-B parameter sets (SPS+PPS for H.264, VPS+SPS+PPS for HEVC), or
    /// the sequence header OBU for AV1, re-injected ahead of every sync sample.
    /// Empty for VP9, which carries no out-of-band parameter sets.
    parameter_sets: Vec<u8>,
    /// Bytes of the NAL length prefix each sample is written with, off `avcC`
    /// or `hvcC`. Unused by VP9.
    nal_length: usize,
    /// `stss` entries, ascending 1-based sample ids. Empty means no `stss` box
    /// at all, i.e. every sample is a sync sample.
    sync_samples: Vec<u32>,
    next_sample: u32,
    /// Bits per luma sample; see [`Demuxer::bit_depth`].
    bit_depth: u8,
}

impl Mp4Demuxer {
    fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
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
                // `mp4 0.14`'s `stsd` parser knows only a `hev1` sample entry
                // (`stsd.rs:107`) and no `av01` at all, so an `hvc1`-tagged HEVC
                // track -- what Apple and ffmpeg's mov muxer write in practice
                // -- and every AV1 track report no media type, and such a file
                // used to read back as having no video. Its sample tables
                // (`stts`/`stsz`/`stsc`/`stss`) are parsed regardless of the
                // entry the crate dropped, so only the fourcc has to be read
                // here by hand. hvc1 differs from hev1 in that the parameter
                // sets may not repeat in-band, which costs nothing: they are
                // re-injected out of `hvcC` ahead of every sync sample either
                // way, exactly as an AV1 sequence header is out of `av1C`.
                _ => matches!(t.track_type(), Ok(TrackType::Video))
                    .then(|| sample_entry(path, t.track_id()).ok())
                    .flatten()
                    .and_then(|(kind, _)| match &kind {
                        b"hvc1" => Some((t, Codec::Hevc)),
                        b"av01" => Some((t, Codec::Av1)),
                        _ => None,
                    }),
            })
            .ok_or("no H.264, HEVC, VP9 or AV1 video track in file")?;
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
        let mut bit_depth = 8;
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
                let (len, sets, depth) = parse_hvcc(&hvcc)?;
                nal_length = len;
                parameter_sets = sets;
                bit_depth = depth;
            }
            // The sequence header OBU out of `av1C`, re-injected ahead of every
            // sync sample exactly as [`MkvDemuxer`] does it off `CodecPrivate` --
            // the same record in the same bytes, which is what makes an AV1 mp4
            // written here and an AV1 Matroska written here one stream in two
            // containers.
            Codec::Av1 => {
                let (_, entry) = sample_entry(path, track_id)?;
                let av1c = child(entry.get(78..).unwrap_or_default(), b"av1C")
                    .ok_or("no av1C box in the AV1 sample entry")?;
                let (sets, depth) = parse_av1c(av1c)?;
                parameter_sets = sets;
                bit_depth = depth;
            }
            // No parameter sets: a VP9 sample is self-contained.
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
                bit_depth,
            },
        ))
    }

    /// Next access unit in decode order: Annex-B framed for H.264 and HEVC, the
    /// mp4 sample verbatim for VP9. `None` at end of track.
    fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
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
        // An AV1 one is a whole temporal unit, likewise unframed -- with the
        // sequence header in front of it where a decoder may start.
        if self.codec == Codec::Av1 {
            let mut au = Vec::with_capacity(self.parameter_sets.len() + sample.bytes.len());
            if sample.is_sync {
                au.extend_from_slice(&self.parameter_sets);
            }
            au.extend_from_slice(&sample.bytes);
            return Ok(Some(au));
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
    fn seek_to_sync_at_or_before(&mut self, frame: u32) -> i64 {
        let target = frame
            .saturating_add(self.first_sample)
            .clamp(1, self.sample_count.max(1));
        let chosen = sync_at_or_before(&self.sync_samples, target);
        self.next_sample = chosen;
        i64::from(chosen) - i64::from(self.first_sample)
    }
}

/// One Matroska block of the video track: where its bytes are and whether a
/// decoder may start on it. 16 bytes an entry, so an hour of 30 fps costs ~1.7
/// MB of index -- the price of knowing the frame count and the sync points of a
/// container that indexes neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Block {
    at: u64,
    len: usize,
    key: bool,
}

/// AV1, HEVC or H.264 out of a Matroska file (`.mkv`/`.webm`).
///
/// The whole segment is walked once at open -- element *headers* only, the block
/// payloads are seeked over -- because Matroska carries no sample table: the
/// frame count and the sync points exist nowhere else, and Cues index only some
/// keyframes and only when a muxer bothered to write them.
pub struct MkvDemuxer {
    file: File,
    blocks: Vec<Block>,
    codec: Codec,
    /// The `av1C` configuration OBUs (the sequence header), re-injected ahead of
    /// every keyframe exactly as the H.264 parameter sets are. An AV1 stream
    /// repeats its sequence header at every keyframe anyway, and cros-codecs
    /// ignores one identical to the sequence in force (`av1.rs:381`), so this is
    /// free when the encoder already wrote one and load-bearing when it did not.
    ///
    /// For HEVC this is the Annex-B VPS/SPS/PPS off the `CodecPrivate`, which is
    /// an `hvcC` record verbatim -- the same blob, re-injected for the same
    /// reason, as the one [`Mp4Demuxer`] carries; for H.264 the SPS/PPS off the
    /// `avcC` beside it, which is that record's H.264 twin.
    config: Vec<u8>,
    /// Bytes of the NAL length prefix an HEVC or H.264 block is written with;
    /// unused by AV1, whose block is one temporal unit and carries no prefixes.
    nal_length: usize,
    /// Bits per luma sample; see [`Demuxer::bit_depth`].
    bit_depth: u8,
    /// One block's bytes, reused across access units: a 4K HEVC keyframe is a
    /// megabyte and it is reframed, not handed over, so the read needs a
    /// landing place of its own.
    scratch: Vec<u8>,
    next: usize,
}

impl MkvDemuxer {
    fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
        let mut file = File::open(path)?;
        let end = file.metadata()?.len();
        let segment = mkv_segment(&mut file, end)?;
        let (video, _, other) = mkv_tracks(&mut file, segment)?;
        let video = match video {
            Some(video) => video,
            // Named, because "no video track" is a lie about a file that has one
            // in a codec this does not read.
            None => {
                return Err(match other {
                    Some(codec) => format!(
                        "{codec} video in a Matroska file is not supported — AV1, HEVC and H.264 are"
                    ),
                    None => "this Matroska file has no video track".to_string(),
                }
                .into());
            }
        };
        let (blocks, span) = mkv_blocks(&mut file, segment, video.number)?;
        if blocks.is_empty() {
            return Err(format!(
                "the {} track in this Matroska file has no frames",
                video.codec.name()
            )
            .into());
        }
        // Timing, in this order: what the track declares, then what its own
        // timestamps say. `DefaultDuration` is nanoseconds and exact -- 33333333
        // for 30 fps, 41708333 for 23.976 -- which is the whole reason it is
        // preferred; the fallback measures the presentation span instead, and
        // that is in `TimestampScale` ticks (a millisecond, as good as every
        // muxer writes), so it lands within ~0.05 % rather than exactly.
        //
        // ponytail: a genuinely variable-rate file is averaged to one rate here,
        // because a timeline frame is a fixed slice of a second everywhere else
        // in this engine. Per-frame durations are the upgrade path, and they are
        // a project-wide change, not a demuxer one.
        let frame_rate = match video.default_duration {
            Some(ns) => 1e9 / ns as f64,
            None => match span {
                Some((first, last)) if last > first && blocks.len() > 1 => {
                    (blocks.len() - 1) as f64 * 1e9
                        / ((last - first) as f64 * video.timestamp_scale as f64)
                }
                // Nothing said and nothing measurable: `fps_from_stts` answers
                // an mp4 with no timing the same way.
                _ => 0.0,
            },
        };
        let meta = VideoMeta {
            width: video.width,
            height: video.height,
            frame_rate,
            frame_count: blocks.len() as u32,
            codec: video.codec,
        };
        Ok((
            meta,
            Self {
                file,
                blocks,
                codec: video.codec,
                config: video.config,
                nal_length: video.nal_length,
                bit_depth: video.bit_depth,
                scratch: Vec::new(),
                next: 0,
            },
        ))
    }

    /// Next access unit in decode order: the block verbatim for AV1, which is
    /// one temporal unit already, and Annex-B for HEVC, whose block holds the
    /// same length-prefixed NALs an mp4 sample does.
    fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
        let Some(&block) = self.blocks.get(self.next) else {
            return Ok(None);
        };
        self.next += 1;
        let head = if block.key { self.config.len() } else { 0 };
        if self.codec == Codec::Av1 {
            let mut au = vec![0u8; head + block.len];
            au[..head].copy_from_slice(&self.config[..head]);
            read_exact_at(&mut self.file, block.at, &mut au[head..])?;
            return Ok(Some(au));
        }
        let Self { file, scratch, .. } = self;
        scratch.resize(block.len, 0);
        read_exact_at(file, block.at, scratch)?;
        let mut au = Vec::with_capacity(head + block.len + 16);
        au.extend_from_slice(&self.config[..head]);
        append_annex_b(&self.scratch, self.nal_length, &mut au)?;
        Ok(Some(au))
    }

    /// As [`Mp4Demuxer::seek_to_sync_at_or_before`]. Never negative: Matroska
    /// has no edit list, so block 0 is frame 0.
    fn seek_to_sync_at_or_before(&mut self, frame: u32) -> i64 {
        let target = (frame as usize).min(self.blocks.len().saturating_sub(1));
        // The keyframe at or before the target, or -- for a stream whose first
        // block is not one -- the earliest block there is, which is the only
        // place a decoder can be started from anyway.
        self.next = self.blocks[..=target]
            .iter()
            .rposition(|b| b.key)
            .unwrap_or(0);
        self.next as i64
    }
}

/// The codec id of `path`'s first audio track (`A_AAC`, `A_OPUS`, ...), or
/// `None` for a Matroska file with no sound at all. Header only, and only worth
/// calling once a session has come up silent: no audio track of a Matroska file
/// is decoded yet, so this exists to say which one is being left out.
pub fn matroska_audio_codec(path: &Path) -> crate::Result<Option<String>> {
    let mut file = File::open(path)?;
    let end = file.metadata()?.len();
    let segment = mkv_segment(&mut file, end)?;
    Ok(mkv_tracks(&mut file, segment)?.1)
}

/// What the `Tracks` element says about the video track.
struct MkvVideo {
    number: u64,
    width: u32,
    height: u32,
    default_duration: Option<u64>,
    timestamp_scale: u64,
    codec: Codec,
    config: Vec<u8>,
    nal_length: usize,
    bit_depth: u8,
}

// Matroska element IDs, written with the leading length marker they carry in
// the file, which is what `ebml_element` reads them back as.
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2AD7B1;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;
const DEFAULT_DURATION: u32 = 0x23E383;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const CLUSTER: u32 = 0x1F43_B675;
const CLUSTER_TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const REFERENCE_BLOCK: u32 = 0xFB;

/// Body range of the file's `Segment` element, which everything else lives in.
fn mkv_segment(file: &mut File, end: u64) -> crate::Result<(u64, u64)> {
    let mut at = 0;
    while let Some(e) = ebml_element(file, at, end)? {
        if e.0 == SEGMENT {
            return Ok((e.1, e.2));
        }
        at = e.2;
    }
    Err("no Segment element: not a Matroska file".into())
}

/// The video track of `segment`, the codec id of its first audio track, and the
/// codec id of a video track this cannot read -- the last two are what tell a
/// user why a file plays silent ([`crate::audio::AudioSession::unsupported`])
/// or refuses to open at all.
///
/// Header only: the walk stops at the first `Cluster`, so this costs a handful
/// of seeks whatever the file weighs.
fn mkv_tracks(
    file: &mut File,
    segment: (u64, u64),
) -> crate::Result<(Option<MkvVideo>, Option<String>, Option<String>)> {
    let (mut video, mut audio, mut other) = (None, None, None);
    let mut timestamp_scale = 1_000_000;
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(file, at, segment.1)? {
        match id {
            CLUSTER => break,
            INFO => {
                let mut at = body;
                while let Some(e) = ebml_element(file, at, stop)? {
                    if e.0 == TIMESTAMP_SCALE {
                        timestamp_scale = ebml_uint(file, e.1, e.2)?.max(1);
                    }
                    at = e.2;
                }
            }
            TRACKS => {
                let mut at = body;
                while let Some(e) = ebml_element(file, at, stop)? {
                    if e.0 == TRACK_ENTRY {
                        match mkv_track_entry(file, e.1, e.2, timestamp_scale)? {
                            MkvEntry::Video(track) if video.is_none() => video = Some(track),
                            MkvEntry::OtherVideo(codec) if other.is_none() => other = Some(codec),
                            MkvEntry::Audio(codec) if audio.is_none() => audio = Some(codec),
                            _ => {}
                        }
                    }
                    at = e.2;
                }
            }
            _ => {}
        }
        at = stop;
    }
    Ok((video, audio, other))
}

/// What one `TrackEntry` turned out to be.
enum MkvEntry {
    Video(MkvVideo),
    /// A video track in a codec this does not read, by the name it gives itself.
    OtherVideo(String),
    /// An audio track, likewise by its codec id (`A_AAC`, `A_OPUS`, ...).
    Audio(String),
    /// Subtitles, buttons: nobody's here.
    Other,
}

/// One `TrackEntry`, read for what it is.
fn mkv_track_entry(
    file: &mut File,
    body: u64,
    end: u64,
    timestamp_scale: u64,
) -> crate::Result<MkvEntry> {
    let (mut number, mut kind, mut codec, mut default_duration) = (0, 0, String::new(), None);
    let (mut width, mut height, mut config) = (0, 0, Vec::new());
    let mut at = body;
    while let Some((id, body, stop)) = ebml_element(file, at, end)? {
        match id {
            TRACK_NUMBER => number = ebml_uint(file, body, stop)?,
            TRACK_TYPE => kind = ebml_uint(file, body, stop)?,
            DEFAULT_DURATION => {
                default_duration = Some(ebml_uint(file, body, stop)?).filter(|d| *d > 0)
            }
            CODEC_ID => {
                codec = String::from_utf8_lossy(&ebml_bytes(file, body, stop)?).into_owned()
            }
            CODEC_PRIVATE => config = ebml_bytes(file, body, stop)?,
            VIDEO => {
                let mut at = body;
                while let Some(e) = ebml_element(file, at, stop)? {
                    match e.0 {
                        PIXEL_WIDTH => width = ebml_uint(file, e.1, e.2)? as u32,
                        PIXEL_HEIGHT => height = ebml_uint(file, e.1, e.2)? as u32,
                        _ => {}
                    }
                    at = e.2;
                }
            }
            _ => {}
        }
        at = stop;
    }
    // 1 is video, 2 is audio; the rest (subtitles, buttons) are nobody's here.
    // A Matroska `CodecPrivate` is the codec's own configuration record --
    // `av1C` for AV1 and an `hvcC` for HEVC, byte for byte the one an mp4
    // sample entry carries, so it parses with the very same reader.
    let (codec, nal_length, config, bit_depth) = match (kind, codec.as_str()) {
        (1, "V_AV1") => {
            let (sets, bit_depth) = parse_av1c(&config)?;
            (Codec::Av1, 4, sets, bit_depth)
        }
        (1, "V_MPEGH/ISO/HEVC") => {
            let (nal_length, sets, bit_depth) = parse_hvcc(&config)?;
            (Codec::Hevc, nal_length, sets, bit_depth)
        }
        // The `avcC` beside them, and the same story: length-prefixed blocks and
        // the SPS/PPS out of the record. Taken as 8-bit, which is what the whole
        // H.264 path here assumes of an mp4's `avc1` too.
        (1, "V_MPEG4/ISO/AVC") => {
            let (nal_length, sets) = parse_avcc(&config)?;
            (Codec::H264, nal_length, sets, 8)
        }
        (1, _) => return Ok(MkvEntry::OtherVideo(codec)),
        (2, _) => return Ok(MkvEntry::Audio(codec)),
        _ => return Ok(MkvEntry::Other),
    };
    Ok(MkvEntry::Video(MkvVideo {
        number,
        width,
        height,
        default_duration,
        timestamp_scale,
        codec,
        config,
        nal_length,
        bit_depth,
    }))
}

/// AVCDecoderConfigurationRecord (ISO 14496-15 §5.3.3.1) -> the NAL length
/// prefix width and the SPS/PPS as one Annex-B blob, which is [`parse_hvcc`]'s
/// job for HEVC. The mp4 path gets both out of `mp4 0.14`'s `AvcCBox`; a
/// Matroska file carries the identical record in `CodecPrivate`, unparsed.
fn parse_avcc(rec: &[u8]) -> crate::Result<(usize, Vec<u8>)> {
    // 0 configurationVersion .. 4 lengthSizeMinusOne, 5 numOfSequenceParameterSets.
    let (&flags, &sps_count) = rec
        .get(4)
        .zip(rec.get(5))
        .ok_or("avcC record shorter than its fixed header")?;
    let mut sets = Vec::new();
    let mut src = &rec[6..];
    // The SPS array, whose count is the low 5 bits of byte 5, and then the PPS
    // array, whose count is a byte of its own.
    let mut count = usize::from(sps_count & 0x1f);
    for array in 0..2 {
        if array == 1 {
            let (&pps_count, rest) = src
                .split_first()
                .ok_or("avcC record ends before its PPS count")?;
            count = usize::from(pps_count);
            src = rest;
        }
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes(
                src.get(..2)
                    .ok_or("avcC NAL length past the record")?
                    .try_into()
                    .unwrap(),
            ));
            let nal = src.get(2..2 + len).ok_or("avcC NAL past the record")?;
            sets.extend_from_slice(&START_CODE);
            sets.extend_from_slice(nal);
            src = &src[2 + len..];
        }
    }
    if sets.is_empty() {
        return Err("avcC record carries no SPS/PPS".into());
    }
    Ok((usize::from(flags & 0x3) + 1, sets))
}

/// The configuration OBUs of an `AV1CodecConfigurationRecord` (AV1-ISOBMFF
/// §2.3.3): four fixed bytes, then the sequence header OBU in the same
/// low-overhead format the blocks are in, so it needs no reframing.
///
/// Also the bits per luma sample, out of `high_bitdepth`/`twelve_bit` in byte 2:
/// 10-bit decodes through the P010 pool HEVC Main 10 already goes through
/// ([`crate::hw`]), 12-bit is refused by name because that pool is the only
/// deeper one there is.
///
/// An empty record is not an error -- an encoder that repeats its sequence
/// header in-band needs none -- and is taken as 8-bit, which is what the depth
/// defaults to everywhere else here.
fn parse_av1c(rec: &[u8]) -> crate::Result<(Vec<u8>, u8)> {
    if rec.is_empty() {
        return Ok((Vec::new(), 8));
    }
    let &flags = rec
        .get(2)
        .ok_or("av1C record shorter than its fixed header")?;
    // `twelve_bit` only says anything when `high_bitdepth` is set (AV1 §5.5.1),
    // so it is read where the record means it and nowhere else.
    let bit_depth = match (flags & 0x40 != 0, flags & 0x20 != 0) {
        (true, true) => {
            return Err(
                "12-bit AV1 is not supported — 8- and 10-bit are what the decoder carries".into(),
            );
        }
        (true, false) => 10,
        _ => 8,
    };
    Ok((rec.get(4..).unwrap_or_default().to_vec(), bit_depth))
}

/// Every block of track `number`, in storage order, with the presentation span
/// (first and last timestamp in `TimestampScale` ticks) the frame-rate fallback
/// needs.
///
/// Only element headers are read: a block's payload is seeked over and fetched
/// later by [`MkvDemuxer::next_access_unit`], which is what keeps this an index
/// pass rather than a read of the whole file.
fn mkv_blocks(
    file: &mut File,
    segment: (u64, u64),
    number: u64,
) -> crate::Result<(Vec<Block>, Option<(i64, i64)>)> {
    let mut blocks = Vec::new();
    let mut span: Option<(i64, i64)> = None;
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(file, at, segment.1)? {
        at = stop;
        if id != CLUSTER {
            continue;
        }
        let mut cluster_ts = 0i64;
        let mut child = body;
        while let Some((id, body, stop)) = ebml_element(file, child, stop)? {
            child = stop;
            let (block, key) = match id {
                CLUSTER_TIMESTAMP => {
                    cluster_ts = ebml_uint(file, body, stop)? as i64;
                    continue;
                }
                SIMPLE_BLOCK => {
                    let block = mkv_block(file, body, stop)?;
                    // Bit 7 of the flags is the keyframe bit; a `Block` inside a
                    // `BlockGroup` has no such bit, which is the case below.
                    (block, block.flags & 0x80 != 0)
                }
                BLOCK_GROUP => {
                    let (mut found, mut key) = (None, true);
                    let mut child = body;
                    while let Some(e) = ebml_element(file, child, stop)? {
                        match e.0 {
                            BLOCK => found = Some(mkv_block(file, e.1, e.2)?),
                            // A block that references another is not one a
                            // decoder can be started from.
                            REFERENCE_BLOCK => key = false,
                            _ => {}
                        }
                        child = e.2;
                    }
                    match found {
                        Some(block) => (block, key),
                        None => continue,
                    }
                }
                _ => continue,
            };
            if block.number != number {
                continue;
            }
            // Lacing packs several frames into one block. Audio muxers use it,
            // video ones do not, and guessing at frame boundaries inside a laced
            // block is not something to do silently.
            if block.flags & 0x06 != 0 {
                return Err("laced video blocks are not supported".into());
            }
            let ts = cluster_ts + i64::from(block.rel);
            span = Some(match span {
                Some((first, last)) => (first.min(ts), last.max(ts)),
                None => (ts, ts),
            });
            blocks.push(Block {
                at: block.at,
                len: block.len,
                key,
            });
        }
    }
    Ok((blocks, span))
}

/// A `SimpleBlock`/`Block` header: track number, timestamp relative to the
/// cluster's, flags, and where the frame's own bytes start.
#[derive(Debug, Clone, Copy)]
struct BlockHeader {
    number: u64,
    rel: i16,
    flags: u8,
    at: u64,
    len: usize,
}

fn mkv_block(file: &mut File, body: u64, stop: u64) -> crate::Result<BlockHeader> {
    let mut head = [0u8; 11];
    let n = read_at(file, body, &mut head)?;
    let (number, len) = ebml_vint(&head[..n], true)?;
    let rest = head[..n]
        .get(len..len + 3)
        .ok_or("truncated Matroska block header")?;
    let at = body + (len + 3) as u64;
    Ok(BlockHeader {
        number,
        rel: i16::from_be_bytes([rest[0], rest[1]]),
        flags: rest[2],
        at,
        len: stop.saturating_sub(at) as usize,
    })
}

/// The EBML element at `at`: its id (marker bit and all), where its payload
/// starts, and where it ends. `None` once `end` is reached. An element of
/// unknown length -- what a muxer writes while it is still recording -- runs to
/// the end of its parent, which is exactly how a reader is told to take it.
fn ebml_element(file: &mut File, at: u64, end: u64) -> crate::Result<Option<(u32, u64, u64)>> {
    if at + 2 > end {
        return Ok(None);
    }
    let mut head = [0u8; 16];
    let n = read_at(file, at, &mut head[..(end - at).min(16) as usize])?;
    let (id, id_len) = ebml_vint(&head[..n], false)?;
    let (size, size_len) = ebml_vint(&head[id_len..n], true)?;
    let body = at + (id_len + size_len) as u64;
    let stop = match size {
        u64::MAX => end,
        size => body.saturating_add(size).min(end),
    };
    if body > end {
        return Err("truncated Matroska element header".into());
    }
    Ok(Some((id as u32, body, stop)))
}

/// EBML variable-length integer: the leading zeros of the first byte say how
/// many bytes it takes. `strip` clears the marker bit, which is what a *size*
/// wants and an *id* does not -- an id is written and compared with it. An
/// all-ones size means unknown length, and comes back as [`u64::MAX`].
fn ebml_vint(buf: &[u8], strip: bool) -> crate::Result<(u64, usize)> {
    let &first = buf.first().ok_or("truncated EBML integer")?;
    if first == 0 {
        // A 9-byte-or-longer integer; Matroska defines none.
        return Err("bad EBML variable-length integer".into());
    }
    let len = first.leading_zeros() as usize + 1;
    let bytes = buf.get(..len).ok_or("truncated EBML integer")?;
    // `0xFF >> len` in `u16`: an 8-byte integer shifts a `u8` mask right off
    // its own end, and the marker bit is then all the first byte was.
    let mut value = u64::from(if strip {
        first & (0xFFu16 >> len) as u8
    } else {
        first
    });
    for &b in &bytes[1..] {
        value = (value << 8) | u64::from(b);
    }
    if strip && value == (1u64 << (7 * len)) - 1 {
        return Ok((u64::MAX, len));
    }
    Ok((value, len))
}

/// An EBML unsigned integer element: big-endian, as many bytes as it was
/// written with, and an absent one is a zero by the spec's own default.
fn ebml_uint(file: &mut File, body: u64, stop: u64) -> crate::Result<u64> {
    Ok(ebml_bytes(file, body, stop)?
        .iter()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b)))
}

/// The payload of an element, which is only ever a header field here -- 8 bytes
/// for an integer, a few hundred for a `CodecPrivate`.
fn ebml_bytes(file: &mut File, body: u64, stop: u64) -> crate::Result<Vec<u8>> {
    let len = stop.saturating_sub(body);
    if len > 1 << 20 {
        return Err("a Matroska header field larger than a megabyte".into());
    }
    let mut buf = vec![0u8; len as usize];
    read_exact_at(file, body, &mut buf)?;
    Ok(buf)
}

/// Positioned reads, so the walk never has to keep a cursor in step with itself.
fn read_at(file: &mut File, at: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, at)
}

fn read_exact_at(file: &mut File, at: u64, buf: &mut [u8]) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, at)
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
    let (_, entry) = sample_entry(path, track_id)?;
    // A VisualSampleEntry header is a fixed 78 bytes before the child boxes,
    // and `hvcC` sits there for `hev1` and `hvc1` alike.
    let hvcc = child(entry.get(78..).unwrap_or_default(), b"hvcC")
        .ok_or("no hvcC box in the HEVC sample entry")?;
    Ok(hvcc.to_vec())
}

/// The four-character code of `track_id`'s first `stsd` sample entry and that
/// entry's payload. Read by hand for the same reason the record above is: the
/// crate keeps no fourcc for a sample entry it does not recognise, so this is
/// the only thing that can tell an `hvc1` track from a track in a codec nothing
/// here reads. Shared with `audio`, which asks it the same question of a `soun`
/// track: `mp4a` or the `ac-3` the crate drops on the floor.
pub(crate) fn sample_entry(path: &Path, track_id: u32) -> crate::Result<([u8; 4], Vec<u8>)> {
    let moov = read_top_level(path, b"moov")?.ok_or("no moov box in file")?;
    let trak = boxes(&moov)
        .filter(|(kind, _)| *kind == b"trak")
        .find(|(_, payload)| child(payload, b"tkhd").and_then(tkhd_track_id) == Some(track_id))
        .ok_or("that track has no trak box")?
        .1;
    let stsd = child(trak, b"mdia")
        .and_then(|b| child(b, b"minf"))
        .and_then(|b| child(b, b"stbl"))
        .and_then(|b| child(b, b"stsd"))
        .ok_or("that track has no stsd box")?;
    // stsd is a FullBox (4) plus entry_count (4), then the sample entries.
    let (kind, payload) = boxes(stsd.get(8..).unwrap_or_default())
        .next()
        .ok_or("empty stsd box")?;
    Ok((*kind, payload.to_vec()))
}

/// HEVCDecoderConfigurationRecord (ISO 14496-15 §8.3.3.1) -> the NAL length
/// prefix width, the VPS/SPS/PPS arrays as one Annex-B blob, and the luma bit
/// depth the plugin picks its surface pool from.
fn parse_hvcc(rec: &[u8]) -> crate::Result<(usize, Vec<u8>, u8)> {
    // 0 configurationVersion .. 21 lengthSizeMinusOne, 22 numOfArrays.
    let (&flags, &arrays) = rec
        .get(21)
        .zip(rec.get(22))
        .ok_or("hvcC box shorter than its fixed header")?;
    // 17 is bit_depth_luma_minus8 in its low 3 bits. 8-bit decodes into the
    // plugin's NV12 pool and 10-bit (Main 10) into its P010 one; 12-bit has
    // neither a pool here nor, on the hardware this was written against, a
    // VA-API profile -- refused by name rather than shown as garbage.
    let bit_depth = 8 + (rec[17] & 0x7);
    if bit_depth > 10 {
        return Err(format!(
            "{bit_depth}-bit HEVC is not supported — 8- and 10-bit are what the decoder carries"
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
    Ok((usize::from(flags & 0x3) + 1, sets, bit_depth))
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
pub(crate) fn boxes(mut buf: &[u8]) -> impl Iterator<Item = (&[u8; 4], &[u8])> {
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

/// One subtitle track of a Matroska file, exactly as its `TrackEntry` declares
/// it. What the cues *mean* is [`crate::subtitle`]'s business; this is the
/// walk, and it reads every track type 0x11 the file has -- including the ones
/// nothing here can render, which is how a caller names what it is leaving out
/// instead of quietly opening a film with no subtitles in the list.
#[derive(Debug)]
pub struct MkvSubtitle {
    pub number: u64,
    /// The Matroska codec id: `S_TEXT/UTF8`, `S_TEXT/ASS`, `S_HDMV/PGS`, ...
    pub codec: String,
    /// `Language`, ISO-639-2, `und` when the file says nothing.
    pub language: String,
    /// `Name`, the human label a muxer wrote, often empty.
    pub name: String,
    /// `CodecPrivate`: the ASS script header (`[Script Info]` through the
    /// `[Events]` `Format:` line) for `S_TEXT/ASS`, empty for `S_TEXT/UTF8`.
    pub private: Vec<u8>,
    /// Every cue block, in storage order -- but only for the `S_TEXT/*` codecs.
    /// A bitmap track (PGS, VobSub) comes back declared and empty: its blocks
    /// are megabytes of pictures, and nothing here can draw one.
    pub cues: Vec<MkvCue>,
}

/// One subtitle block: when it shows and the bytes it shows.
#[derive(Debug)]
pub struct MkvCue {
    /// Microseconds from the start of the file -- the block timestamp scaled by
    /// the segment's `TimestampScale`, which is the unit
    /// [`crate::subtitle::Cue`] keeps.
    pub start_us: i64,
    /// `BlockDuration`, microseconds. `None` for a block that declares none,
    /// which leaves how long the cue stays up to the caller.
    pub duration_us: Option<i64>,
    /// The block's own bytes: the text for `S_TEXT/UTF8`, the comma-separated
    /// `Dialogue` fields (`ReadOrder` first, no timing) for `S_TEXT/ASS`.
    pub payload: Vec<u8>,
}

/// The subtitle tracks of a Matroska file, in file order. An mp4's are not read
/// (its `tx3g` is a different beast, and no file this project opens carries
/// one); anything that is not a Matroska file at all is an error, as it is for
/// [`matroska_audio_codec`].
///
/// Two passes at most: the header walk stops at the first `Cluster`, and the
/// cue pass runs only when there is a text track to fill.
pub fn matroska_subtitles(path: &Path) -> crate::Result<Vec<MkvSubtitle>> {
    let mut file = File::open(path)?;
    let end = file.metadata()?.len();
    let segment = mkv_segment(&mut file, end)?;
    let mut tracks: Vec<MkvSubtitle> = Vec::new();
    let mut timestamp_scale = 1_000_000;
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(&mut file, at, segment.1)? {
        match id {
            CLUSTER => break,
            INFO => {
                let mut at = body;
                while let Some(e) = ebml_element(&mut file, at, stop)? {
                    if e.0 == TIMESTAMP_SCALE {
                        timestamp_scale = ebml_uint(&mut file, e.1, e.2)?.max(1);
                    }
                    at = e.2;
                }
            }
            TRACKS => {
                let mut at = body;
                while let Some(e) = ebml_element(&mut file, at, stop)? {
                    if e.0 == TRACK_ENTRY {
                        tracks.extend(mkv_subtitle_entry(&mut file, e.1, e.2)?);
                    }
                    at = e.2;
                }
            }
            _ => {}
        }
        at = stop;
    }
    // The text tracks only: see `MkvSubtitle::cues`.
    let wanted: Vec<u64> = tracks
        .iter()
        .filter(|t| t.codec.starts_with("S_TEXT"))
        .map(|t| t.number)
        .collect();
    if !wanted.is_empty() {
        mkv_subtitle_blocks(&mut file, segment, timestamp_scale, &wanted, &mut tracks)?;
    }
    Ok(tracks)
}

/// One `TrackEntry`, `Some` only for track type 0x11 -- the subtitles.
fn mkv_subtitle_entry(file: &mut File, body: u64, end: u64) -> crate::Result<Option<MkvSubtitle>> {
    const TRACK_LANGUAGE: u32 = 0x22B59C;
    const TRACK_NAME: u32 = 0x536E;
    const SUBTITLE: u64 = 0x11;
    let (mut number, mut kind, mut codec) = (0, 0, String::new());
    let (mut language, mut name, mut private) = (String::new(), String::new(), Vec::new());
    let mut at = body;
    while let Some((id, body, stop)) = ebml_element(file, at, end)? {
        match id {
            TRACK_NUMBER => number = ebml_uint(file, body, stop)?,
            TRACK_TYPE => kind = ebml_uint(file, body, stop)?,
            CODEC_ID => codec = string_of(file, body, stop)?,
            CODEC_PRIVATE => private = ebml_bytes(file, body, stop)?,
            TRACK_LANGUAGE => language = string_of(file, body, stop)?,
            TRACK_NAME => name = string_of(file, body, stop)?,
            _ => {}
        }
        at = stop;
    }
    Ok((kind == SUBTITLE).then(|| MkvSubtitle {
        number,
        codec,
        // What a `TrackEntry` without a `Language` element means, by spec.
        language: if language.is_empty() {
            "und".into()
        } else {
            language
        },
        name,
        private,
        cues: Vec::new(),
    }))
}

/// Every block of the `wanted` tracks, appended to their entries in `tracks`.
///
/// A second cluster walk rather than a widening of [`mkv_blocks`]: that one
/// indexes *one* video track and reads no payloads, and a subtitle needs every
/// track at once, the bytes, and the `BlockDuration` beside them.
fn mkv_subtitle_blocks(
    file: &mut File,
    segment: (u64, u64),
    timestamp_scale: u64,
    wanted: &[u64],
    tracks: &mut [MkvSubtitle],
) -> crate::Result<()> {
    const BLOCK_DURATION: u32 = 0x9B;
    // Ticks to microseconds, the unit `MkvCue` keeps. `TimestampScale` is
    // nanoseconds per tick and is a millisecond in every file anything writes.
    let us = |ticks: i64| ticks * timestamp_scale as i64 / 1_000;
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(file, at, segment.1)? {
        at = stop;
        if id != CLUSTER {
            continue;
        }
        let mut cluster_ts = 0i64;
        let mut child = body;
        while let Some((id, body, stop)) = ebml_element(file, child, stop)? {
            child = stop;
            let (block, duration) = match id {
                CLUSTER_TIMESTAMP => {
                    cluster_ts = ebml_uint(file, body, stop)? as i64;
                    continue;
                }
                SIMPLE_BLOCK => (mkv_block(file, body, stop)?, None),
                BLOCK_GROUP => {
                    let (mut found, mut duration) = (None, None);
                    let mut child = body;
                    while let Some(e) = ebml_element(file, child, stop)? {
                        match e.0 {
                            BLOCK => found = Some(mkv_block(file, e.1, e.2)?),
                            BLOCK_DURATION => duration = Some(ebml_uint(file, e.1, e.2)? as i64),
                            _ => {}
                        }
                        child = e.2;
                    }
                    match found {
                        Some(block) => (block, duration),
                        None => continue,
                    }
                }
                _ => continue,
            };
            if !wanted.contains(&block.number) {
                continue;
            }
            // As for a video block: guessing at the boundaries inside a laced
            // one is not something to do silently. No muxer laces subtitles.
            if block.flags & 0x06 != 0 {
                return Err("laced subtitle blocks are not supported".into());
            }
            // A megabyte of *text* in one cue is a corrupt file, not a subtitle,
            // and a crafted length may not reach an allocation through here.
            if block.len > 1 << 20 {
                return Err("a Matroska subtitle block larger than a megabyte".into());
            }
            let mut payload = vec![0u8; block.len];
            read_exact_at(file, block.at, &mut payload)?;
            let Some(track) = tracks.iter_mut().find(|t| t.number == block.number) else {
                continue;
            };
            track.cues.push(MkvCue {
                start_us: us(cluster_ts + i64::from(block.rel)),
                duration_us: duration.map(us),
                payload,
            });
        }
    }
    Ok(())
}

/// A string element, without the trailing NULs a `Name` may be padded with.
fn string_of(file: &mut File, body: u64, stop: u64) -> crate::Result<String> {
    let bytes = ebml_bytes(file, body, stop)?;
    Ok(String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .to_owned())
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
        let (len, sets, depth) = parse_hvcc(&rec).unwrap();
        assert_eq!(len, 2);
        assert_eq!(depth, 8);
        assert_eq!(
            sets,
            [
                0, 0, 0, 1, 64, 0xAA, // VPS
                0, 0, 0, 1, 66, 0xBB, // SPS
                0, 0, 0, 1, 68, 0xDD, // PPS -- the SEI array is skipped
            ]
        );

        // Main 10 is read, and read as 10-bit: that is what picks the P010
        // surface pool over the NV12 one.
        let mut main10 = rec.clone();
        main10[17] = 0xf8 | 2;
        assert_eq!(parse_hvcc(&main10).unwrap().2, 10);
        // 12-bit has no pool and no profile here, and is refused by name.
        let mut main12 = rec.clone();
        main12[17] = 0xf8 | 4;
        let refused = parse_hvcc(&main12).unwrap_err().to_string();
        assert!(refused.contains("12-bit"), "{refused}");
        // Nothing is read past the end of a truncated box.
        assert!(parse_hvcc(&rec[..20]).is_err());
        assert!(parse_hvcc(&rec[..rec.len() - 1]).is_err());
    }

    /// The `avcC` parse: prefix width out of byte 4, and both arrays -- the SPS
    /// count is five bits of byte 5, the PPS count a byte of its own, and taking
    /// the second for the first is the mistake this checks against.
    #[test]
    fn reads_parameter_sets_out_of_an_avcc_record() {
        let mut rec = vec![1, 0x42, 0x00, 0x1f, 0xfc | 0x1, 0xe0 | 2];
        for (kind, nal) in [(7u8, 0xAAu8), (7, 0xBB)] {
            rec.extend_from_slice(&2u16.to_be_bytes());
            rec.extend_from_slice(&[kind, nal]);
        }
        rec.push(1); // numOfPictureParameterSets
        rec.extend_from_slice(&2u16.to_be_bytes());
        rec.extend_from_slice(&[8, 0xCC]);

        let (len, sets) = parse_avcc(&rec).unwrap();
        assert_eq!(len, 2, "lengthSizeMinusOne == 1");
        assert_eq!(
            sets,
            [
                0, 0, 0, 1, 7, 0xAA, // SPS
                0, 0, 0, 1, 7, 0xBB, // the second SPS, which a one-array read loses
                0, 0, 0, 1, 8, 0xCC, // PPS
            ]
        );
        // Nothing is read past the end of a truncated record, and a Matroska
        // file with no `CodecPrivate` at all is refused rather than started.
        assert!(parse_avcc(&rec[..5]).is_err());
        assert!(parse_avcc(&rec[..rec.len() - 1]).is_err());
        assert!(parse_avcc(&[]).is_err());
    }

    /// EBML's variable-length integers, which every Matroska read starts with:
    /// the leading zeros of the first byte are the length, an *id* keeps its
    /// marker bit and a *size* loses it, and an all-ones size is "unknown".
    #[test]
    fn ebml_integers_read_ids_and_sizes_differently() {
        // The Segment id, 4 bytes, marker kept.
        assert_eq!(
            ebml_vint(&[0x18, 0x53, 0x80, 0x67], false).unwrap(),
            (SEGMENT as u64, 4)
        );
        // A one-byte id, and the same byte read as a size is its low 7 bits.
        assert_eq!(ebml_vint(&[0xA3], false).unwrap(), (0xA3, 1));
        assert_eq!(ebml_vint(&[0xA3], true).unwrap(), (0x23, 1));
        // Sizes: 0x4143 is 323 bytes, not 0x4143.
        assert_eq!(ebml_vint(&[0x41, 0x43], true).unwrap(), (323, 2));
        // Unknown length -- what a muxer writes while still recording.
        assert_eq!(ebml_vint(&[0xFF], true).unwrap().0, u64::MAX);
        assert_eq!(
            ebml_vint(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], true)
                .unwrap()
                .0,
            u64::MAX
        );
        // Nothing is read past the end of the buffer, and a zero first byte is
        // a 9-byte integer Matroska never writes.
        assert!(ebml_vint(&[0x41], true).is_err());
        assert!(ebml_vint(&[], false).is_err());
        assert!(ebml_vint(&[0x00, 1, 2], true).is_err());
    }

    /// The `av1C` record: four fixed bytes and then the sequence header OBU,
    /// which is handed to the decoder as it stands, and the depth beside it --
    /// 10-bit reads as 10 (the P010 pool, exactly as HEVC Main 10), 12-bit is
    /// refused by name.
    #[test]
    fn reads_the_sequence_header_out_of_an_av1c_record() {
        // marker/version, profile 0 + level 5, 8-bit 4:2:0, no delay; then a
        // 13-byte sequence header OBU in low-overhead format.
        let mut rec = vec![0x81, 0x05, 0x0C, 0x00];
        rec.extend_from_slice(&[
            0x0A, 0x0B, 0x00, 0x00, 0x00, 0x2D, 0x4C, 0xFF, 0xB3, 0xC0, 0x2F, 0x80, 0x00,
        ]);
        let (config, depth) = parse_av1c(&rec).unwrap();
        assert_eq!(config, rec[4..], "the fixed header is not part of the OBUs");
        assert_eq!((config[0] >> 3) & 0xF, 1, "OBU type 1 is a sequence header");
        assert_eq!(depth, 8);

        let mut ten = rec.clone();
        ten[2] |= 0x40; // high_bitdepth
        let (ten_config, ten_depth) = parse_av1c(&ten).unwrap();
        assert_eq!(ten_depth, 10, "what picks the P010 surface pool");
        assert_eq!(ten_config, config, "the OBUs are read the same either way");
        let mut twelve = ten.clone();
        twelve[2] |= 0x20; // twelve_bit
        assert!(
            parse_av1c(&twelve)
                .unwrap_err()
                .to_string()
                .contains("12-bit")
        );
        // `twelve_bit` without `high_bitdepth` says nothing, and an 8-bit
        // record reads as one whatever that bit holds.
        let mut stray = rec.clone();
        stray[2] |= 0x20;
        assert_eq!(parse_av1c(&stray).unwrap().1, 8);

        // An encoder that repeats its sequence header in-band writes no record
        // at all, which is not an error; a truncated one is.
        assert!(parse_av1c(&[]).unwrap().0.is_empty());
        assert!(parse_av1c(&[0x81, 0x05]).is_err());
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
