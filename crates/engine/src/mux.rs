//! Annex-B -> MP4, the mirror of `demux`: encoders emit start-code framed NALs,
//! an mp4 sample wants 4-byte length prefixes with SPS/PPS living in `avcC`.
//!
//! ...and AV1 -> Matroska, the mirror of the *other* half of `demux`, for the
//! same reason that half exists: `mp4 0.14` has no `av01` sample entry at all,
//! so an AV1 export is written as EBML by hand exactly as an AV1 import is read
//! as EBML by hand ([`MkvMuxer`]).
//!
//! AV1 goes into an **mp4** too, and the missing sample entry is written by hand
//! there as well ([`Mp4Muxer::create_av1`]): the crate writes the file with an
//! `avc1` entry and this patches that entry into the `av01` + `av1C` pair the
//! spec asks for, which is the only box in a whole mp4 the crate cannot spell.
//! Both containers carry the timeline's AAC, so no format here writes a picture
//! whose sound was silently left behind.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use mp4::{
    AacConfig, AudioObjectType, AvcConfig, Bytes, ChannelConfig, FourCC, MediaConfig, Mp4Config,
    Mp4Sample, Mp4Writer, SampleFreqIndex, TrackConfig, TrackType,
};

/// 90 kHz is the H.264 convention, and it divides exactly at every *integer*
/// fps (3000 ticks at 30, 3600 at 25). The NTSC rates do not divide into it at
/// all -- 24000/1001 wants 3753.75 -- so those get their own clock; see
/// [`frame_timing`].
const VIDEO_TIMESCALE: u32 = 90_000;
/// Ticks per frame for a rate the 90 kHz clock cannot express: every NTSC rate
/// is `n/1001`, so 1001 ticks a frame makes the timescale a whole `n`.
const NTSC_TICKS: u32 = 1001;
/// An AAC-LC packet is always 1024 samples, so with timescale == sample rate the
/// per-packet duration is this constant.
const AAC_PACKET_SAMPLES: u32 = 1024;

const NAL_IDR: u8 = 5;
const NAL_SEI: u8 = 6;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

pub struct VideoParams<'a> {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub sps: &'a [u8],
    pub pps: &'a [u8],
}

/// What the audio track declares: read straight off the source file's `esds`
/// where the export copies packets, and off this project's own encoder where it
/// re-encodes ([`crate::export`] decides which). `sample_rate` says the same
/// thing `freq_index` does and is carried beside it because Matroska states the
/// rate as a number rather than as an index into the AAC table.
pub struct AudioParams {
    pub freq_index: u8,
    pub chan_conf: u8,
    pub sample_rate: u32,
}

/// Writes one H.264 *or* AV1 track plus an optional AAC track. mp4 0.14 ignores
/// `Mp4Sample::start_time` and builds all timing by accumulating `duration`, so
/// every sample handed over must carry an exact duration or the tracks drift.
pub struct Mp4Muxer {
    writer: Mp4Writer<BufWriter<File>>,
    frame_duration: u32,
    has_audio: bool,
    /// The sample entry an AV1 or HEVC file's is patched into at
    /// [`finish`](Mp4Muxer::finish) -- entry type, configuration box type and
    /// its payload (`av01`/`av1C`, `hvc1`/`hvcC`). `None` for H.264, which the
    /// crate spells itself. See [`create_av1`](Mp4Muxer::create_av1).
    entry: Option<([u8; 4], [u8; 4], Vec<u8>)>,
    /// Where the file is, for that patch: the writer owns the handle it wrote
    /// through and the patch reopens the finished file.
    path: PathBuf,
}

const VIDEO_TRACK: u32 = 1;
const AUDIO_TRACK: u32 = 2;

impl Mp4Muxer {
    pub fn create(
        path: &Path,
        video: &VideoParams,
        audio: Option<&AudioParams>,
    ) -> crate::Result<Self> {
        Self::open(path, video, audio, None)
    }

    /// The same file with an AV1 track in it. `mp4 0.14` writes no `av01` sample
    /// entry -- it has no AV1 anything -- so the file is written with an `avc1`
    /// entry of placeholder parameter sets and that one entry is rewritten at
    /// [`finish`](Mp4Muxer::finish) into the `av01` + `av1C` pair AV1-ISOBMFF
    /// §2.2 asks for. Everything else in an mp4 is codec-blind (the sample
    /// tables, the audio track, the movie header), so the entry is the only box
    /// this has to spell by hand -- and the `moov` it lives in is written *after*
    /// `mdat`, so growing it moves no sample and invalidates no chunk offset.
    ///
    /// The record's layout is the one OxideAV's `oxideav-mp4` writes
    /// (`src/sample_entries.rs`, MIT): a `VisualSampleEntry` header verbatim,
    /// then `av1C` -- the very four bytes and sequence header OBU [`MkvMuxer`]
    /// puts in `CodecPrivate`, which is what makes the two containers' AV1 one
    /// stream written twice rather than two.
    ///
    /// ponytail: the ceiling is that one entry. A second video track, or an AV1
    /// track beside an H.264 one, would need the walk in [`patch_av01`] to be
    /// told *which* track it is patching; the upgrade path is passing the track
    /// id it already knows down that walk.
    pub fn create_av1(
        path: &Path,
        video: &Av1Params,
        audio: Option<&AudioParams>,
    ) -> crate::Result<Self> {
        if video.config.is_empty() {
            return Err("no AV1 sequence header for the track's av1C record".into());
        }
        let mut av1c = AV1C_HEAD.to_vec();
        av1c.extend_from_slice(video.config);
        Self::open(
            path,
            &VideoParams {
                width: video.width,
                height: video.height,
                frame_rate: video.frame_rate,
                // Never read by anything: the entry that holds them is replaced
                // whole at `finish`. Four bytes because `AvcCBox::new` indexes
                // `sps[1..=3]` for the profile it will never be asked for.
                sps: &[0x67, 0x42, 0x00, 0x1E],
                pps: &[0x68, 0xCE, 0x3C, 0x80],
            },
            audio,
            Some((*b"av01", *b"av1C", av1c)),
        )
    }

    /// ...and the same trick for HEVC, which `mp4 0.14` cannot spell either:
    /// the file is written with an `avc1` entry and that entry is rewritten at
    /// [`finish`](Mp4Muxer::finish) into the `hvc1` + `hvcC` pair ISO/IEC
    /// 14496-15 §8.4.1 asks for -- the record being the very bytes the Matroska
    /// file puts in `CodecPrivate`, so the two containers carry one stream.
    /// `hvc1` rather than `hev1`: the samples this writes carry no parameter
    /// sets (they are dropped into `hvcC`), which is exactly what the `hvc1`
    /// name promises a reader.
    pub fn create_hevc(
        path: &Path,
        video: &HevcParams,
        audio: Option<&AudioParams>,
    ) -> crate::Result<Self> {
        if video.hvcc.is_empty() {
            return Err("no hvcC record for the track's sample entry".into());
        }
        Self::open(
            path,
            &VideoParams {
                width: video.width,
                height: video.height,
                frame_rate: video.frame_rate,
                sps: &[0x67, 0x42, 0x00, 0x1E],
                pps: &[0x68, 0xCE, 0x3C, 0x80],
            },
            audio,
            Some((*b"hvc1", *b"hvcC", video.hvcc.to_vec())),
        )
    }

    fn open(
        path: &Path,
        video: &VideoParams,
        audio: Option<&AudioParams>,
        entry: Option<([u8; 4], [u8; 4], Vec<u8>)>,
    ) -> crate::Result<Self> {
        if !video.frame_rate.is_finite() || video.frame_rate <= 0.0 {
            return Err(format!("bad frame rate {}", video.frame_rate).into());
        }
        if video.width == 0
            || video.height == 0
            || video.width > u16::MAX as u32
            || video.height > u16::MAX as u32
        {
            return Err(format!("bad dimensions {}x{}", video.width, video.height).into());
        }
        // AvcCBox::new indexes sps[1..=3] for profile/level, so a short SPS panics
        // inside the mp4 crate; refuse it here where the message still means something.
        if video.sps.len() < 4 || video.pps.is_empty() {
            return Err("no usable SPS/PPS for the video track".into());
        }

        let (timescale, frame_duration) = frame_timing(video.frame_rate)?;
        let mut writer = Mp4Writer::write_start(
            BufWriter::new(File::create(path)?),
            &Mp4Config {
                major_brand: FourCC::from(*b"isom"),
                minor_version: 512,
                // The codec's own brand among them, so a reader that picks a
                // parser off `ftyp` is told which of the two this is.
                compatible_brands: vec![
                    FourCC::from(*b"isom"),
                    FourCC::from(*b"iso2"),
                    FourCC::from(match &entry {
                        Some((kind, ..)) => *kind,
                        None => *b"avc1",
                    }),
                    FourCC::from(*b"mp41"),
                ],
                // The video track's own, deliberately: `mp4` converts every
                // sample duration into the movie timescale with an integer
                // division of its own (track.rs:752), so a coarser movie clock
                // loses a fraction of a millisecond *per frame* -- 1 kHz makes a
                // 4.000 s export announce itself as 3.960 s. Equal timescales
                // make the video conversion exact.
                timescale,
            },
        )?;
        writer.add_track(&TrackConfig {
            track_type: TrackType::Video,
            timescale,
            language: "und".to_string(),
            media_conf: MediaConfig::AvcConfig(AvcConfig {
                width: video.width as u16,
                height: video.height as u16,
                seq_param_set: video.sps.to_vec(),
                pic_param_set: video.pps.to_vec(),
            }),
        })?;
        if let Some(audio) = audio {
            let freq_index = SampleFreqIndex::try_from(audio.freq_index)?;
            // The index's own rate, not `audio.sample_rate`: this is the clock
            // an `stts` counts in and the `esds` states the index, so the two
            // must not be able to disagree.
            let sample_rate = freq_index.freq();
            writer.add_track(&TrackConfig {
                track_type: TrackType::Audio,
                timescale: sample_rate,
                language: "und".to_string(),
                media_conf: MediaConfig::AacConfig(AacConfig {
                    bitrate: 0,
                    profile: AudioObjectType::AacLowComplexity,
                    freq_index,
                    chan_conf: ChannelConfig::try_from(audio.chan_conf)?,
                }),
            })?;
        }

        Ok(Self {
            writer,
            frame_duration,
            has_audio: audio.is_some(),
            entry,
            path: path.to_path_buf(),
        })
    }

    /// One coded picture, Annex-B framed. Parameter sets inside it are dropped
    /// (they are already in `avcC`); an IDR slice marks the sample as a sync point.
    pub fn write_video_au(&mut self, annex_b: &[u8]) -> crate::Result<()> {
        let (bytes, is_sync) = annex_b_to_avcc(annex_b)?;
        self.writer.write_sample(
            VIDEO_TRACK,
            &Mp4Sample {
                start_time: 0, // ignored by the writer; timing is duration-accumulated
                duration: self.frame_duration,
                rendering_offset: 0, // no B-frames on either encode path
                is_sync,
                bytes: Bytes::from(bytes),
            },
        )?;
        Ok(())
    }

    /// One coded picture already framed the way its sample carries it: an AV1
    /// temporal unit is the low-overhead OBU stream the encoder handed over
    /// (nothing to reframe, exactly as a Matroska block holds it), and an HEVC
    /// sample is the length-prefixed NALs [`annex_b_to_hvcc`] made. `key` is the
    /// encoder's own word for a unit a decoder may start on -- an mp4 says it in
    /// `stss` where Matroska says it in the block flags.
    pub fn write_coded_sample(&mut self, obus: &[u8], key: bool) -> crate::Result<()> {
        if obus.is_empty() {
            return Err("an empty coded picture".into());
        }
        self.writer.write_sample(
            VIDEO_TRACK,
            &Mp4Sample {
                start_time: 0,
                duration: self.frame_duration,
                rendering_offset: 0,
                is_sync: key,
                bytes: Bytes::copy_from_slice(obus),
            },
        )?;
        Ok(())
    }

    /// One raw AAC packet (no ADTS header), copied verbatim from the source --
    /// or one of the hand-written silent ones a gap is filled with. Every AAC-LC
    /// access unit is [`AAC_PACKET_SAMPLES`] frames, gap or not.
    pub fn write_audio_packet(&mut self, bytes: &[u8]) -> crate::Result<()> {
        if !self.has_audio {
            return Err("audio packet written to a video-only file".into());
        }
        self.writer.write_sample(
            AUDIO_TRACK,
            &Mp4Sample {
                start_time: 0,
                duration: AAC_PACKET_SAMPLES,
                rendering_offset: 0,
                // Every AAC packet is a sync point; saying so per-sample would emit
                // an `stss` listing all of them, while no `stss` means the same thing.
                is_sync: false,
                bytes: Bytes::copy_from_slice(bytes),
            },
        )?;
        Ok(())
    }

    pub fn finish(mut self) -> crate::Result<()> {
        self.writer.write_end()?;
        let Self {
            writer,
            entry,
            path,
            ..
        } = self;
        // Not a `drop`: the buffered tail of `moov` has to be on disk before the
        // patch below reads it back, and a `BufWriter` swallows the error of
        // flushing it on drop.
        writer.into_writer().flush()?;
        match entry {
            Some((kind, config_kind, config)) => patch_entry(&path, &kind, &config_kind, &config),
            None => Ok(()),
        }
    }
}

/// Rewrites the finished file's one video sample entry from the `avc1` the crate
/// wrote into the `av01` + `av1C` an AV1 track declares -- or the `hvc1` +
/// `hvcC` an HEVC one does. Called once, on a complete file, and only for a file
/// [`Mp4Muxer::create_av1`] or [`Mp4Muxer::create_hevc`] opened.
///
/// `moov` sits after `mdat` (the crate writes it at `write_end`), so this only
/// ever rewrites the tail of the file: no sample moves and no chunk offset in
/// `co64` changes. The box tree is rebuilt rather than patched in place, which
/// is what keeps every ancestor's size right by construction.
fn patch_entry(
    path: &Path,
    kind: &[u8; 4],
    config_kind: &[u8; 4],
    config: &[u8],
) -> crate::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let end = file.metadata()?.len();
    let (at, payload) = top_level(&mut file, end, b"moov")?.ok_or("no moov box to patch")?;
    let patched = swap_entry(&payload, 0, kind, config_kind, config)
        .ok_or("no avc1 sample entry to rewrite")?;
    let mut out = Vec::with_capacity(patched.len() + 8);
    push_box(&mut out, b"moov", &patched);
    file.seek(SeekFrom::Start(at))?;
    file.write_all(&out)?;
    file.set_len(at + out.len() as u64)?;
    file.flush()?;
    Ok(())
}

/// Offset and payload of the first top-level box of type `want`.
fn top_level(file: &mut File, end: u64, want: &[u8; 4]) -> crate::Result<Option<(u64, Vec<u8>)>> {
    let mut at = 0u64;
    while at + 8 <= end {
        let mut header = [0u8; 8];
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut header)?;
        let (size, head_len) = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => (end - at, 8),
            1 => {
                let mut large = [0u8; 8];
                file.read_exact(&mut large)?;
                (u64::from_be_bytes(large), 16)
            }
            size => (u64::from(size), 8),
        };
        if size < head_len || at + size > end {
            return Err("truncated box in the file's top level".into());
        }
        if &header[4..8] == want {
            let mut payload = vec![0u8; (size - head_len) as usize];
            file.read_exact(&mut payload)?;
            return Ok(Some((at, payload)));
        }
        at += size;
    }
    Ok(None)
}

/// `moov` down to the sample descriptions, which is where the entry lives.
const STSD_PATH: [&[u8; 4]; 5] = [b"trak", b"mdia", b"minf", b"stbl", b"stsd"];

/// The box at `depth` of [`STSD_PATH`], rebuilt with its `avc1` entry replaced.
/// `None` where this branch holds no such entry -- an audio `trak` is walked
/// into and comes back untouched, which is how the *video* track is found
/// without being told which id it has.
fn swap_entry(
    payload: &[u8],
    depth: usize,
    want: &[u8; 4],
    config_kind: &[u8; 4],
    config: &[u8],
) -> Option<Vec<u8>> {
    if depth == STSD_PATH.len() {
        // stsd is a FullBox (4) plus entry_count (4), then the sample entries.
        let mut out = payload.get(..8)?.to_vec();
        let (kind, entry) = crate::demux::boxes(payload.get(8..)?).next()?;
        if kind != b"avc1" {
            return None;
        }
        // A `VisualSampleEntry` is 78 bytes of fixed fields (dimensions and all)
        // before its codec box, and `av01` and `hvc1` carry exactly the same
        // ones -- so the header is kept and only the configuration box is
        // swapped.
        let mut swapped = entry.get(..78)?.to_vec();
        push_box(&mut swapped, config_kind, config);
        push_box(&mut out, want, &swapped);
        return Some(out);
    }
    let mut out = Vec::with_capacity(payload.len());
    let mut done = false;
    for (kind, child) in crate::demux::boxes(payload) {
        let patched = match !done && kind == STSD_PATH[depth] {
            true => swap_entry(child, depth + 1, want, config_kind, config),
            false => None,
        };
        match patched {
            Some(child) => {
                push_box(&mut out, kind, &child);
                done = true;
            }
            None => push_box(&mut out, kind, child),
        }
    }
    done.then_some(out)
}

/// One mp4 box: 32-bit size, four-character type, payload. Every box inside a
/// `moov` fits that form -- the 64-bit one exists for `mdat`, which this never
/// rewrites.
fn push_box(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
}

// Matroska element ids, written with the leading length marker they carry in the
// file -- the same spelling `demux` reads them back with.
const EBML: u32 = 0x1A45_DFA3;
const EBML_VERSION: u32 = 0x4286;
const EBML_READ_VERSION: u32 = 0x42F7;
const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
const DOC_TYPE: u32 = 0x4282;
const DOC_TYPE_VERSION: u32 = 0x4287;
const DOC_TYPE_READ_VERSION: u32 = 0x4285;
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2AD7B1;
const DURATION: u32 = 0x4489;
const MUXING_APP: u32 = 0x4D80;
const WRITING_APP: u32 = 0x5741;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_UID: u32 = 0x73C5;
const TRACK_TYPE: u32 = 0x83;
const FLAG_LACING: u32 = 0x9C;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;
const DEFAULT_DURATION: u32 = 0x23E383;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const AUDIO: u32 = 0xE1;
const SAMPLING_FREQUENCY: u32 = 0xB5;
const CHANNELS: u32 = 0x9F;
const CODEC_DELAY: u32 = 0x56AA;
const CLUSTER: u32 = 0x1F43_B675;
const CLUSTER_TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const TRACK_NAME: u32 = 0x536E;
const TRACK_LANGUAGE: u32 = 0x22B59C;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const BLOCK_DURATION: u32 = 0x9B;

/// One millisecond, the tick every Matroska muxer writes and this one's block
/// timestamps are in. The *rate* is not derived from these -- `DefaultDuration`
/// is, in exact nanoseconds -- so a millisecond is precise enough for the
/// presentation times and coarse enough to keep a cluster's 16-bit relative
/// timestamp covering half a minute.
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;
/// A cluster is buffered whole (its size is a header field), so it is flushed
/// at the first keyframe past this -- a ceiling on what the muxer holds, not a
/// target.
const CLUSTER_BYTES: usize = 4 << 20;
/// ...and on how far a block's timestamp may sit from its cluster's: the field
/// is a signed 16-bit millisecond count.
const CLUSTER_MS: i64 = 30_000;

/// The fixed four bytes of an `AV1CodecConfigurationRecord` for what both encode
/// paths here are configured to produce: profile 0, 8-bit, 4:2:0, no operating
/// point delay. `seq_level_idx` is written as 31 -- "maximum parameters", i.e.
/// unstated -- which is what rav1e's own `container_sequence_header` writes and
/// what keeps this record from claiming a level neither encoder promised. The
/// sequence header OBU follows it; that is where a decoder reads the real
/// parameters from, and `demux::parse_av1c` hands exactly those bytes back.
const AV1C_HEAD: [u8; 4] = [
    0x81, // marker 1, version 1
    0x1F, // seq_profile 0, seq_level_idx_0 31
    0x0C, // tier 0, 8-bit, colour, chroma subsampled both ways
    0x00, // no initial presentation delay
];

/// What an AV1 Matroska track declares. `config` is the sequence header OBU,
/// which is [`av1_sequence_header`]'s answer for the first coded keyframe.
pub struct Av1Params<'a> {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub config: &'a [u8],
}

/// What an HEVC track declares in either container: the `hvcC` record
/// [`hvcc_record`] built out of the encoder's own VPS/SPS/PPS. The dimensions
/// are the *displayed* ones -- the coded picture is padded up to the 16-sample
/// CTB grid and the SPS crops it back with a conformance window, which is the
/// size a decoder outputs and therefore the size the container states.
pub struct HevcParams<'a> {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub hvcc: &'a [u8],
}

/// The soft subtitle track a Matroska file carries beside the picture: text,
/// timed, still text in the file -- a player draws it and a user can turn it
/// off, which is what "soft" means and what burning it into the pixels is not.
///
/// The cues are the *exported timeline's* (`export::timeline_cues` puts them
/// there), because the file's clock is the timeline's; nothing here shifts
/// anything.
pub struct SubParams {
    /// What the track is called in the file. A three-letter code goes in as
    /// `Language`, which is what a player's subtitle menu reads; anything else
    /// is a `Name`, which is what it shows when there is no language to show.
    ///
    /// ponytail: one string for both because a [`crate::subtitle::SubtitleTrack`]
    /// carries one label and not a (language, name) pair. The upgrade path is to
    /// keep the two apart from the demuxer down -- `MkvSubtitle` already has
    /// them -- and it belongs to whoever needs a per-language export list.
    pub label: String,
    /// In start order, which is the order they are written in.
    pub cues: Vec<crate::subtitle::Cue>,
}

/// The cues waiting to be interleaved into the clusters, [`MkvAudio`]'s twin.
struct MkvSubs {
    cues: Vec<crate::subtitle::Cue>,
    /// Cues already written.
    next: usize,
}

/// Writes one AV1 video track as Matroska (`.mkv`), and the timeline's AAC
/// beside it where there is any: an `A_AAC` track whose `CodecPrivate` is the
/// two-byte `AudioSpecificConfig` an mp4's `esds` carries, which is what
/// symphonia's `mkv` reader -- this project's own way back in -- configures its
/// decoder from. An AV1 export used to be picture alone; a video file whose
/// sound was silently left behind is exactly the failure nobody notices until
/// the file has gone somewhere.
///
/// The sound is *interleaved*: each packet is written into the cluster of the
/// picture it plays under, so a player has both by the time it needs them rather
/// than a file it must read to the end of to hear.
///
/// The file is patched twice at [`finish`](MkvMuxer::finish) -- the segment size
/// and the duration -- so a killed process leaves an unfinished file rather than
/// a wrong one; the export's `.part` rename covers that already.
pub struct MkvMuxer {
    file: File,
    /// Nanoseconds a frame lasts: `DefaultDuration`, which is the *only* exact
    /// statement of the frame rate in the file (see `demux::MkvDemuxer::open`).
    frame_ns: u64,
    segment_size_at: u64,
    duration_at: u64,
    /// The cluster being built: everything after its id and size, so it can be
    /// written once its length is known.
    cluster: Vec<u8>,
    cluster_ts: i64,
    frames: u64,
    /// The AAC track, if the timeline has sound, and how far it has been
    /// written.
    audio: Option<MkvAudio>,
    /// The subtitle track, if one travels with the file, and how far it has
    /// been written.
    subs: Option<MkvSubs>,
}

/// The sound waiting to be interleaved into the clusters.
struct MkvAudio {
    packets: Vec<crate::AacPacket>,
    /// Packets already written.
    next: usize,
    /// Frames already written, which is what the next packet's timestamp is
    /// counted from: every AAC-LC packet says its own length and they are not
    /// assumed equal here.
    samples: u64,
    sample_rate: u32,
}

impl MkvMuxer {
    /// `audio` is the whole track at once -- the export has it before it has a
    /// single coded picture, and interleaving needs to look ahead of the frame
    /// being written.
    ///
    /// ponytail: that is the copy path's own ceiling reappearing (the track sits
    /// in memory, ~500 MB an hour); the upgrade path is the same streaming
    /// `copy_segments` `export::copy_audio` names.
    pub fn create(
        path: &Path,
        video: &Av1Params,
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
        subs: Option<SubParams>,
    ) -> crate::Result<Self> {
        if video.config.is_empty() {
            return Err("no AV1 sequence header for the track's CodecPrivate".into());
        }
        let mut av1c = AV1C_HEAD.to_vec();
        av1c.extend_from_slice(video.config);
        Self::open(
            path,
            video.width,
            video.height,
            video.frame_rate,
            b"V_AV1",
            &av1c,
            audio,
            subs,
        )
    }

    /// The same file with an HEVC track in it: `V_MPEGH/ISO/HEVC`, whose
    /// `CodecPrivate` is the very `hvcC` record an mp4 puts in its sample entry
    /// ([`hvcc_record`]) -- and whose blocks are therefore length-prefixed NALs
    /// like an mp4 sample's, not Annex B. That is what this engine's own reader
    /// expects back (`demux`'s Matroska branch parses `CodecPrivate` as `hvcC`
    /// and reframes the blocks by the length it states), and what every other
    /// Matroska reader expects too.
    pub fn create_hevc(
        path: &Path,
        video: &HevcParams,
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
        subs: Option<SubParams>,
    ) -> crate::Result<Self> {
        if video.hvcc.is_empty() {
            return Err("no hvcC record for the track's CodecPrivate".into());
        }
        Self::open(
            path,
            video.width,
            video.height,
            video.frame_rate,
            b"V_MPEGH/ISO/HEVC",
            video.hvcc,
            audio,
            subs,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        path: &Path,
        width: u32,
        height: u32,
        frame_rate: f64,
        codec_id: &[u8],
        codec_private: &[u8],
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
        subs: Option<SubParams>,
    ) -> crate::Result<Self> {
        if !frame_rate.is_finite() || frame_rate <= 0.0 {
            return Err(format!("bad frame rate {frame_rate}").into());
        }
        if width == 0 || height == 0 {
            return Err(format!("bad dimensions {width}x{height}").into());
        }
        let frame_ns = (1e9 / frame_rate).round() as u64;
        if frame_ns == 0 {
            return Err(format!("frame rate {frame_rate} is too fast to time").into());
        }

        let mut head = Vec::new();
        let mut ebml = Vec::new();
        uint(&mut ebml, EBML_VERSION, 1);
        uint(&mut ebml, EBML_READ_VERSION, 1);
        uint(&mut ebml, EBML_MAX_ID_LENGTH, 4);
        uint(&mut ebml, EBML_MAX_SIZE_LENGTH, 8);
        elem(&mut ebml, DOC_TYPE, b"matroska");
        uint(&mut ebml, DOC_TYPE_VERSION, 4);
        uint(&mut ebml, DOC_TYPE_READ_VERSION, 2);
        elem(&mut head, EBML, &ebml);

        // The segment's size is only known at `finish`; it is reserved at the
        // widest encoding (8 bytes) so patching it never moves a byte after it.
        put_id(&mut head, SEGMENT);
        let segment_size_at = head.len() as u64;
        head.extend_from_slice(&[0x01, 0, 0, 0, 0, 0, 0, 0]);

        let mut info = Vec::new();
        uint(&mut info, TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS);
        elem(&mut info, MUXING_APP, b"edith");
        elem(&mut info, WRITING_APP, b"edith");
        put_id(&mut info, DURATION);
        put_size(&mut info, 8);
        // Where those eight bytes will land once `Info` is written into `head`,
        // which is what `finish` seeks back to.
        let duration_at = (head.len() + elem_head_len(INFO, info.len() + 8) + info.len()) as u64;
        info.extend_from_slice(&0f64.to_be_bytes());
        elem(&mut head, INFO, &info);

        let mut entry = Vec::new();
        uint(&mut entry, TRACK_NUMBER, 1);
        uint(&mut entry, TRACK_UID, 1);
        uint(&mut entry, TRACK_TYPE, 1); // video
        uint(&mut entry, FLAG_LACING, 0);
        elem(&mut entry, CODEC_ID, codec_id);
        elem(&mut entry, CODEC_PRIVATE, codec_private);
        uint(&mut entry, DEFAULT_DURATION, frame_ns);
        let mut dims = Vec::new();
        uint(&mut dims, PIXEL_WIDTH, u64::from(width));
        uint(&mut dims, PIXEL_HEIGHT, u64::from(height));
        elem(&mut entry, VIDEO, &dims);
        let mut tracks = Vec::new();
        elem(&mut tracks, TRACK_ENTRY, &entry);
        if let Some((audio, _)) = &audio {
            elem(&mut tracks, TRACK_ENTRY, &aac_track_entry(audio)?);
        }
        // Declared even where the audio track is not: the numbers are fixed
        // (1 picture, 2 sound, 3 text) rather than counted, so a file with
        // subtitles and no sound still says track 3 in its blocks.
        if let Some(subs) = &subs {
            elem(&mut tracks, TRACK_ENTRY, &subtitle_track_entry(&subs.label));
        }
        elem(&mut head, TRACKS, &tracks);

        let mut file = File::create(path)?;
        file.write_all(&head)?;
        Ok(Self {
            file,
            frame_ns,
            segment_size_at,
            duration_at,
            cluster: Vec::new(),
            cluster_ts: 0,
            frames: 0,
            audio: audio.map(|(params, packets)| MkvAudio {
                packets,
                next: 0,
                samples: 0,
                sample_rate: params.sample_rate.max(1),
            }),
            subs: subs.map(|subs| MkvSubs {
                cues: subs.cues,
                next: 0,
            }),
        })
    }

    /// One coded picture: a whole AV1 temporal unit, in the low-overhead OBU
    /// format the demuxer hands back. `key` marks a block a decoder may be
    /// started from -- said by the encoder rather than guessed at here, and a
    /// keyframe is where a new cluster opens, so every seek target is one.
    pub fn write_frame(&mut self, obus: &[u8], key: bool) -> crate::Result<()> {
        if obus.is_empty() {
            return Err("an empty AV1 temporal unit".into());
        }
        let ts = (self.frames * self.frame_ns + TIMESTAMP_SCALE_NS / 2) as i64
            / TIMESTAMP_SCALE_NS as i64;
        // A new cluster at every keyframe -- a seek lands on one, so a cluster
        // is a whole GOP -- and at the two limits a cluster has whatever the
        // encoder keys: what it may weigh, and how far a 16-bit relative
        // timestamp reaches.
        if self.cluster.is_empty()
            || key
            || self.cluster.len() >= CLUSTER_BYTES
            || ts - self.cluster_ts >= CLUSTER_MS
        {
            self.flush()?;
            self.cluster_ts = ts;
            uint(&mut self.cluster, CLUSTER_TIMESTAMP, ts as u64);
        }
        // The sound this picture plays under goes in first, into the cluster the
        // picture opened: a player reads the two together. The cue over it with
        // them, for the same reason.
        self.drain_audio(ts)?;
        self.drain_subs(ts)?;
        self.block(1, ts, key, obus);
        self.frames += 1;
        Ok(())
    }

    /// Every audio packet whose timestamp has been reached, into the current
    /// cluster. `i64::MAX` at [`finish`](MkvMuxer::finish) drains what is left --
    /// the sound of a timeline can outlast its last picture by a packet.
    fn drain_audio(&mut self, until: i64) -> crate::Result<()> {
        loop {
            let Some(audio) = &mut self.audio else {
                return Ok(());
            };
            let Some(packet) = audio.packets.get_mut(audio.next) else {
                return Ok(());
            };
            // The packet's own start, in whole milliseconds: every AAC-LC packet
            // states its length, so a track of them is counted rather than
            // assumed to be 1024 frames each.
            let rate = i64::from(audio.sample_rate);
            let ts = (audio.samples as i64 * 1000 + rate / 2) / rate;
            if ts > until {
                return Ok(());
            }
            // Taken, not copied: the packet is written once and the track can be
            // hundreds of megabytes.
            let bytes = std::mem::take(&mut packet.bytes);
            audio.samples += u64::from(packet.samples);
            audio.next += 1;
            // A cluster of its own where the sound has run past what a 16-bit
            // relative timestamp reaches, which only the tail drain can do.
            if self.cluster.is_empty() || ts - self.cluster_ts >= CLUSTER_MS {
                self.flush()?;
                self.cluster_ts = ts;
                uint(&mut self.cluster, CLUSTER_TIMESTAMP, ts as u64);
            }
            self.block(2, ts, true, &bytes);
        }
    }

    /// Every cue that has come up by `until`, into the current cluster --
    /// [`drain_audio`](MkvMuxer::drain_audio)'s twin, and `i64::MAX` at
    /// [`finish`](MkvMuxer::finish) writes what is left: a cue may still be on
    /// screen over the last picture, and one written nowhere is one a player
    /// never shows.
    fn drain_subs(&mut self, until: i64) -> crate::Result<()> {
        loop {
            let Some(subs) = &mut self.subs else {
                return Ok(());
            };
            let Some(cue) = subs.cues.get_mut(subs.next) else {
                return Ok(());
            };
            // Milliseconds, the tick this file is written in: a cue is read for
            // a second or more and no eye finds the half a tick it rounds by.
            let ts = (cue.start_us + 500) / 1_000;
            if ts > until {
                return Ok(());
            }
            // A cue that says it ends before it starts stays up for one tick
            // rather than for a duration a reader would take as unsigned.
            let ms = ((cue.end_us - cue.start_us + 500) / 1_000).max(1) as u64;
            let text = std::mem::take(&mut cue.text);
            subs.next += 1;
            // A cluster of its own where the text has run past what a 16-bit
            // relative timestamp reaches, exactly as the sound's drain does.
            if self.cluster.is_empty() || ts - self.cluster_ts >= CLUSTER_MS {
                self.flush()?;
                self.cluster_ts = ts;
                uint(&mut self.cluster, CLUSTER_TIMESTAMP, ts as u64);
            }
            self.block_group(SUB_TRACK, ts, ms, text.as_bytes());
        }
    }

    /// One `SimpleBlock` of `track`, timed against the open cluster. Every AAC
    /// packet is a keyframe; a picture is one when the encoder said so.
    fn block(&mut self, track: u8, ts: i64, key: bool, payload: &[u8]) {
        let mut block = Vec::with_capacity(payload.len() + 4);
        block.push(0x80 | track); // the track number as a one-byte EBML integer
        block.extend_from_slice(&((ts - self.cluster_ts) as i16).to_be_bytes());
        block.push(if key { 0x80 } else { 0 });
        block.extend_from_slice(payload);
        elem(&mut self.cluster, SIMPLE_BLOCK, &block);
    }

    /// One `BlockGroup`: a `Block` and the `BlockDuration` beside it. That pair
    /// is how a cue says when it goes away -- a `SimpleBlock` has nowhere to put
    /// a duration, and a subtitle without one stays up until whatever a player
    /// decides, which is what a subtitle must never do.
    fn block_group(&mut self, track: u8, ts: i64, duration_ms: u64, payload: &[u8]) {
        let mut block = Vec::with_capacity(payload.len() + 4);
        block.push(0x80 | track); // the track number as a one-byte EBML integer
        block.extend_from_slice(&((ts - self.cluster_ts) as i16).to_be_bytes());
        // No flags a text block can carry: not lacing, and "keyframe" is a
        // `SimpleBlock` field that a plain `Block` does not have at all.
        block.push(0);
        block.extend_from_slice(payload);
        let mut group = Vec::new();
        elem(&mut group, BLOCK, &block);
        uint(&mut group, BLOCK_DURATION, duration_ms);
        elem(&mut self.cluster, BLOCK_GROUP, &group);
    }

    fn flush(&mut self) -> crate::Result<()> {
        if self.cluster.is_empty() {
            return Ok(());
        }
        let mut head = Vec::new();
        put_id(&mut head, CLUSTER);
        put_size(&mut head, self.cluster.len() as u64);
        self.file.write_all(&head)?;
        self.file.write_all(&self.cluster)?;
        self.cluster.clear();
        Ok(())
    }

    /// Closes the last cluster and patches the two fields that could not be
    /// known while writing: the segment's size and the presentation duration.
    pub fn finish(mut self) -> crate::Result<()> {
        // Whatever sound outlasts the last picture, before the last cluster is
        // closed: a track cut short here is a file that goes silent at the end.
        self.drain_audio(i64::MAX)?;
        self.drain_subs(i64::MAX)?;
        self.flush()?;
        if self.frames == 0 {
            return Err("no frames were written to the Matroska file".into());
        }
        let end = self.file.stream_position()?;
        let body = end - (self.segment_size_at + 8);
        // The reserved 8-byte size: marker bit in the top byte, value under it.
        let size = (1u64 << 56) | body;
        self.file.seek(SeekFrom::Start(self.segment_size_at))?;
        self.file.write_all(&size.to_be_bytes())?;
        let ms = self.frames as f64 * self.frame_ns as f64 / TIMESTAMP_SCALE_NS as f64;
        self.file.seek(SeekFrom::Start(self.duration_at))?;
        self.file.write_all(&ms.to_be_bytes())?;
        self.file.flush()?;
        Ok(())
    }
}

/// The `TrackEntry` of the AAC track: `A_AAC`, whose `CodecPrivate` is the
/// two-byte `AudioSpecificConfig` an mp4 states inside its `esds` -- object type
/// 2 (AAC-LC), the sample rate as its index into the AAC table, and the channel
/// configuration. symphonia's `mkv` reader hands exactly these bytes to its AAC
/// decoder, which is how a file written here reads back here.
///
/// `CodecDelay` is one packet: the first AAC access unit of both paths that
/// reach this -- a copy, whose head packet is the source's priming, and this
/// project's own encoder, whose delay is one 1024-frame block -- is that delay,
/// and an mp4 reader drops it by convention where Matroska has to be told in
/// nanoseconds.
fn aac_track_entry(audio: &AudioParams) -> crate::Result<Vec<u8>> {
    let rate = audio.sample_rate;
    if rate == 0 || audio.chan_conf == 0 {
        return Err(format!(
            "an AAC track at {rate} Hz with {} channels",
            audio.chan_conf
        )
        .into());
    }
    let mut entry = Vec::new();
    uint(&mut entry, TRACK_NUMBER, 2);
    uint(&mut entry, TRACK_UID, 2);
    uint(&mut entry, TRACK_TYPE, 2); // audio
    uint(&mut entry, FLAG_LACING, 0);
    elem(&mut entry, CODEC_ID, b"A_AAC");
    // 5 bits object type, 4 bits sample rate index, 4 bits channel config, and
    // three zero bits to the byte -- the whole of an AAC-LC AudioSpecificConfig.
    let asc = (2u16 << 11)
        | (u16::from(audio.freq_index & 0xF) << 7)
        | (u16::from(audio.chan_conf & 0xF) << 3);
    elem(&mut entry, CODEC_PRIVATE, &asc.to_be_bytes());
    uint(
        &mut entry,
        CODEC_DELAY,
        u64::from(AAC_PACKET_SAMPLES) * 1_000_000_000 / u64::from(rate),
    );
    let mut audio_elem = Vec::new();
    put_id(&mut audio_elem, SAMPLING_FREQUENCY);
    put_size(&mut audio_elem, 8);
    audio_elem.extend_from_slice(&f64::from(rate).to_be_bytes());
    uint(&mut audio_elem, CHANNELS, u64::from(audio.chan_conf));
    elem(&mut entry, AUDIO, &audio_elem);
    Ok(entry)
}

/// Which track a subtitle block names. Fixed, like the picture's 1 and the
/// sound's 2: a file with no audio track still writes its text on 3, so nothing
/// has to count tracks to read a block.
const SUB_TRACK: u8 = 3;

/// The `TrackEntry` of the subtitle track: type 0x11 (subtitles) and
/// `S_TEXT/UTF8`, whose blocks are the cue's own UTF-8 text and whose timing is
/// the block's -- the codec every player draws and the one this project's own
/// reader parses back (`subtitle::cues_of`).
///
/// The label is a `Language` where it is one (`eng`, `tur`: three letters is
/// what ISO-639-2 is) and a `Name` otherwise, which is exactly what
/// [`crate::subtitle::of_matroska`] reads back out, so a track exported and
/// re-imported keeps the name it had.
fn subtitle_track_entry(label: &str) -> Vec<u8> {
    let mut entry = Vec::new();
    uint(&mut entry, TRACK_NUMBER, u64::from(SUB_TRACK));
    uint(&mut entry, TRACK_UID, u64::from(SUB_TRACK));
    uint(&mut entry, TRACK_TYPE, 0x11);
    uint(&mut entry, FLAG_LACING, 0);
    elem(&mut entry, CODEC_ID, b"S_TEXT/UTF8");
    let code = label.len() == 3 && label.bytes().all(|b| b.is_ascii_lowercase());
    match (code, label.is_empty()) {
        (true, _) => elem(&mut entry, TRACK_LANGUAGE, label.as_bytes()),
        (false, false) => elem(&mut entry, TRACK_NAME, label.as_bytes()),
        (false, true) => {}
    }
    entry
}

/// The sequence header OBU of a coded temporal unit, for the track's
/// `CodecPrivate`. `None` when the unit carries none, which the first keyframe
/// of a stream never may -- no decoder can start without it.
pub fn av1_sequence_header(obus: &[u8]) -> Option<&[u8]> {
    let mut at = 0;
    while at < obus.len() {
        let header = obus[at];
        // OBU header: type in bits 6..3, then the extension and size flags.
        let kind = (header >> 3) & 0xF;
        let mut len = 1 + usize::from(header & 0x4 != 0);
        // Only the low-overhead format is written or read here: an OBU with no
        // size field runs to the end of the unit, which makes the rest of it
        // unwalkable, so it is not something to guess at.
        if header & 0x2 == 0 {
            return None;
        }
        let (size, size_len) = leb128(obus.get(at + len..)?)?;
        len += size_len;
        let body = obus.get(at + len..at + len + size)?;
        if kind == 1 {
            return Some(&obus[at..at + len + body.len()]);
        }
        at += len + size;
    }
    None
}

/// An unsigned LEB128 as AV1 writes its OBU sizes: `(value, bytes read)`.
fn leb128(buf: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for (i, &byte) in buf.iter().take(8).enumerate() {
        value |= usize::from(byte & 0x7F) << (i * 7);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// An element id, written as the bytes it is spelled with -- the leading zeros
/// of the constant are not part of it.
fn put_id(out: &mut Vec<u8>, id: u32) {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|&&b| b == 0).count();
    out.extend_from_slice(&bytes[skip..]);
}

/// An element size as an EBML variable-length integer, in the fewest bytes that
/// hold it. A value of all ones means *unknown* length, so a size that would
/// land on one takes a byte more.
fn put_size(out: &mut Vec<u8>, size: u64) {
    let mut len = 1;
    while len < 8 && size >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let value = (1u64 << (7 * len)) | size;
    out.extend_from_slice(&value.to_be_bytes()[8 - len..]);
}

fn elem(out: &mut Vec<u8>, id: u32, payload: &[u8]) {
    put_id(out, id);
    put_size(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

/// An unsigned integer element, big-endian in as few bytes as it takes (one at
/// the least: a zero is a byte, not an absence).
fn uint(out: &mut Vec<u8>, id: u32, value: u64) {
    let bytes = value.to_be_bytes();
    let skip = bytes.iter().take_while(|&&b| b == 0).count().min(7);
    elem(out, id, &bytes[skip..]);
}

/// How many bytes an element's id and size take, which is what an offset inside
/// a payload has to be moved by to become an offset in the file.
fn elem_head_len(id: u32, payload: usize) -> usize {
    let (mut head, mut size) = (Vec::new(), Vec::new());
    put_id(&mut head, id);
    put_size(&mut size, payload as u64);
    head.len() + size.len()
}

/// `(timescale, ticks per frame)` for `fps`, chosen so one frame is a *whole*
/// number of ticks: 90 kHz wherever it divides exactly (every integer rate, so
/// those exports stay byte for byte what they were), and `round(fps * 1001)`
/// over [`NTSC_TICKS`] otherwise, which is exact for the whole `n/1001` family.
/// A fixed 90 kHz clock leaves 24000/1001 a quarter tick short *per frame* --
/// half a second of drift against the audio track by the end of a two-hour
/// export, which is this bug's own truncation reborn at the writing end.
pub(crate) fn frame_timing(fps: f64) -> crate::Result<(u32, u32)> {
    let ticks = f64::from(VIDEO_TIMESCALE) / fps;
    if (ticks - ticks.round()).abs() < 1e-9 && (1.0..=f64::from(u32::MAX)).contains(&ticks.round())
    {
        return Ok((VIDEO_TIMESCALE, ticks.round() as u32));
    }
    let timescale = (fps * f64::from(NTSC_TICKS)).round();
    if !(1.0..=f64::from(u32::MAX)).contains(&timescale) {
        return Err(format!("frame rate {fps} has no usable timescale").into());
    }
    Ok((timescale as u32, NTSC_TICKS))
}

/// SPS and PPS of an Annex-B access unit, for the `avcC` box. `None` when the
/// unit does not carry both, which the first exported unit always must.
pub fn parameter_sets(annex_b: &[u8]) -> Option<(&[u8], &[u8])> {
    let nals = split_annex_b(annex_b);
    let sps = nals.iter().find(|n| nal_type(n) == NAL_SPS)?;
    let pps = nals.iter().find(|n| nal_type(n) == NAL_PPS)?;
    Some((sps, pps))
}

/// The `HEVCDecoderConfigurationRecord` an HEVC track declares, built out of the
/// parameter sets the encoder put in its first access unit -- the exact record
/// `demux::parse_hvcc` reads back, which is what makes an export of this
/// project's own something it can re-open.
///
/// The 22 fixed bytes before the arrays: the 12-byte `profile_tier_level` is
/// copied straight out of the SPS (its first 12 bytes after the two-byte NAL
/// header and the four `sps_video_parameter_set_id`/`sps_max_sub_layers_minus1`
/// /`sps_temporal_id_nesting_flag` bits -- §7.3.2.2 puts the PTL there and the
/// hvcC header states the same fields in the same order), and the rest is what
/// this encoder is: 4:2:0, 8-bit, one temporal layer, 4-byte NAL lengths. Only
/// the *arrays* are read by anything here, but a record whose header lied about
/// the profile would be a file `ffprobe` disagrees with.
///
/// `None` where the unit carries no VPS, SPS or PPS -- the first coded intra AU
/// always carries all three.
pub fn hvcc_record(annex_b: &[u8]) -> Option<Vec<u8>> {
    let nals = split_annex_b(annex_b);
    let of = |kind: u8| nals.iter().copied().find(|n| hevc_nal_type(n) == kind);
    let (vps, sps, pps) = (of(HEVC_VPS)?, of(HEVC_SPS)?, of(HEVC_PPS)?);
    // The SPS as a decoder reads it: the emulation-prevention bytes the encoder
    // escaped it with are not part of the syntax, and a run of zeros in the
    // constraint flags is exactly where one lands.
    let rbsp = unescape(sps.get(2..)?);
    let mut rec = vec![1u8]; // configurationVersion
    rec.extend_from_slice(rbsp.get(1..13)?); // profile_tier_level, verbatim
    rec.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc = 0
    rec.push(0xFC); // parallelismType = 0 (unknown)
    rec.push(0xFC | 1); // chroma_format_idc = 1 (4:2:0)
    rec.push(0xF8); // bit_depth_luma_minus8 = 0
    rec.push(0xF8); // bit_depth_chroma_minus8 = 0
    rec.extend_from_slice(&0u16.to_be_bytes()); // avgFrameRate: unstated
    // constantFrameRate 0, numTemporalLayers 1, temporalIdNested 1,
    // lengthSizeMinusOne 3 -- the same 4-byte prefix `annex_b_to_hvcc` writes.
    rec.push(0b00_001_1_11);
    rec.push(3); // numOfArrays
    for (kind, nal) in [(HEVC_VPS, vps), (HEVC_SPS, sps), (HEVC_PPS, pps)] {
        rec.push(0x80 | kind); // array_completeness = 1, NAL_unit_type
        rec.extend_from_slice(&1u16.to_be_bytes()); // numNalus
        rec.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        rec.extend_from_slice(nal);
    }
    Some(rec)
}

/// One HEVC access unit as a sample of both containers: 4-byte length prefixes,
/// parameter sets dropped (they are in the `hvcC`, which is what `hvc1` and a
/// Matroska `CodecPrivate` both promise). `None` where the unit holds no coded
/// slice at all.
pub(crate) fn annex_b_to_hvcc(annex_b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(annex_b.len());
    for nal in split_annex_b(annex_b) {
        // 32 VPS, 33 SPS, 34 PPS, 35 AUD -- and everything from 40 up is
        // non-VCL padding no decoder needs to start.
        if matches!(hevc_nal_type(nal), HEVC_VPS | HEVC_SPS | HEVC_PPS | 35) {
            continue;
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    (!out.is_empty()).then_some(out)
}

/// §7.3.1.2 `nal_unit_type`, the six bits after the forbidden_zero_bit.
fn hevc_nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0xFF, |b| (b >> 1) & 0x3F)
}

const HEVC_VPS: u8 = 32;
const HEVC_SPS: u8 = 33;
const HEVC_PPS: u8 = 34;

/// A coded NAL payload back to its RBSP: every `00 00 03` loses the `03`
/// (§7.3.1.1). Only ever asked of an SPS, which is short.
fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    for (i, &byte) in nal.iter().enumerate() {
        if byte == 3 && i >= 2 && nal[i - 1] == 0 && nal[i - 2] == 0 {
            continue;
        }
        out.push(byte);
    }
    out
}

/// Whether the unit carries a coded picture (NAL types 1..=5). An encoder that
/// buffered instead of coding hands back an empty buffer, and one that emitted
/// only parameter sets has no sample to write -- both are for the caller to skip
/// rather than for [`Mp4Muxer::write_video_au`] to reject.
pub(crate) fn has_coded_slice(annex_b: &[u8]) -> bool {
    split_annex_b(annex_b)
        .iter()
        .any(|nal| (1..=NAL_IDR).contains(&nal_type(nal)))
}

/// Exact inverse of `demux::append_annex_b`: 4-byte length prefixes (the only
/// prefix size `mp4` writes, avc1.rs hardcodes `length_size_minus_one = 3`).
/// Parameter sets, access unit delimiters and SEI are stripped — the first two
/// are redundant with `avcC`, SEI is not worth carrying. Returns the sample
/// bytes and whether the unit is an IDR.
fn annex_b_to_avcc(annex_b: &[u8]) -> crate::Result<(Vec<u8>, bool)> {
    let mut out = Vec::with_capacity(annex_b.len());
    let mut is_idr = false;
    for nal in split_annex_b(annex_b) {
        match nal_type(nal) {
            NAL_SPS | NAL_PPS | NAL_AUD | NAL_SEI => continue,
            NAL_IDR => is_idr = true,
            _ => {}
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    if out.is_empty() {
        return Err("access unit has no coded slice".into());
    }
    Ok((out, is_idr))
}

/// NAL payloads of an Annex-B buffer, both 3- and 4-byte start codes, trailing
/// zero bytes trimmed (they belong to the next start code, or are padding).
fn split_annex_b(src: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start = None;
    let mut i = 0;
    while i + 2 < src.len() {
        if src[i] == 0 && src[i + 1] == 0 && src[i + 2] == 1 {
            if let Some(s) = start {
                push_nal(&mut nals, &src[s..i]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        push_nal(&mut nals, &src[s..]);
    }
    nals
}

fn push_nal<'a>(nals: &mut Vec<&'a [u8]>, nal: &'a [u8]) {
    let end = nal.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    if end > 0 {
        nals.push(&nal[..end]);
    }
}

fn nal_type(nal: &[u8]) -> u8 {
    nal[0] & 0x1F
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;

    use super::*;

    const SPS: &[u8] = &[0x67, 0x42, 0x00, 0x1E, 0xAB];
    const PPS: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];

    fn au(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for nal in nals {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(nal);
        }
        v
    }

    #[test]
    fn splits_both_start_code_lengths() {
        let src = [
            0, 0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0xBB, 0, 0, 0, 1, 0x65, 0xCC,
        ];
        let nals = split_annex_b(&src);
        assert_eq!(nals, [&[0x67, 0xAA][..], &[0x68, 0xBB], &[0x65, 0xCC]]);
    }

    #[test]
    fn trims_trailing_zero_padding() {
        let src = [0, 0, 0, 1, 0x65, 0xCC, 0, 0, 0];
        assert_eq!(split_annex_b(&src), [&[0x65, 0xCC][..]]);
    }

    #[test]
    fn repacks_multi_nal_unit_and_drops_parameter_sets() {
        let slice = [0x65, 0x88, 0x99];
        let (bytes, is_idr) = annex_b_to_avcc(&au(&[SPS, PPS, &[0x09, 0x10], &slice])).unwrap();
        assert!(is_idr);
        assert_eq!(
            bytes,
            [0, 0, 0, 3, 0x65, 0x88, 0x99],
            "4-byte prefix, slice only"
        );

        let (bytes, is_idr) = annex_b_to_avcc(&au(&[&[0x41, 0x9A], &[0x41, 0x9B]])).unwrap();
        assert!(!is_idr, "no IDR slice in the unit");
        assert_eq!(bytes, [0, 0, 0, 2, 0x41, 0x9A, 0, 0, 0, 2, 0x41, 0x9B]);
    }

    #[test]
    fn rejects_units_with_no_slice() {
        assert!(annex_b_to_avcc(&[]).is_err(), "empty");
        assert!(annex_b_to_avcc(&[0x65, 0x88]).is_err(), "no start code");
        assert!(
            annex_b_to_avcc(&au(&[SPS, PPS])).is_err(),
            "parameter sets only"
        );
    }

    #[test]
    fn finds_parameter_sets() {
        assert_eq!(
            parameter_sets(&au(&[SPS, PPS, &[0x65, 0x11]])),
            Some((SPS, PPS))
        );
        assert!(
            parameter_sets(&au(&[SPS, &[0x65, 0x11]])).is_none(),
            "no PPS"
        );
    }

    /// Every frame must be a whole number of ticks, or the video track drifts
    /// against the audio one over a long export -- the truncation this bug is.
    #[test]
    fn frame_timing_is_exact_at_ntsc_and_unchanged_at_integer_rates() {
        assert_eq!(frame_timing(30.0).unwrap(), (90_000, 3_000), "was 90k/3000");
        assert_eq!(frame_timing(25.0).unwrap(), (90_000, 3_600));
        assert_eq!(frame_timing(24_000.0 / 1001.0).unwrap(), (24_000, 1_001));
        // 29.97 is the one NTSC rate 90 kHz already counts exactly (3003 ticks),
        // so it keeps the conventional clock rather than growing its own.
        assert_eq!(frame_timing(30_000.0 / 1001.0).unwrap(), (90_000, 3_003));
        // And the pair plays back as the rate it came from, to the last bit.
        for fps in [30.0, 25.0, 24_000.0 / 1001.0, 30_000.0 / 1001.0, 60.0] {
            let (ts, ticks) = frame_timing(fps).unwrap();
            let played = f64::from(ts) / f64::from(ticks);
            assert!((played - fps).abs() < 1e-9, "{fps} writes back as {played}");
        }
        // Rate so low the 90 kHz clock cannot count it: refused, not truncated.
        assert!(frame_timing(1e-9).is_err());
    }

    #[test]
    fn create_rejects_unusable_config() {
        let out = std::env::temp_dir().join("ve_mux_never_written.mp4");
        let ok = VideoParams {
            width: 64,
            height: 64,
            frame_rate: 30.0,
            sps: SPS,
            pps: PPS,
        };
        assert!(
            Mp4Muxer::create(
                &out,
                &VideoParams {
                    sps: &SPS[..3],
                    ..ok
                },
                None
            )
            .is_err(),
            "SPS too short for avcC"
        );
        assert!(
            Mp4Muxer::create(
                &out,
                &VideoParams {
                    frame_rate: 0.0,
                    ..ok
                },
                None
            )
            .is_err()
        );
        assert!(
            Mp4Muxer::create(&out, &VideoParams { width: 0, ..ok }, None).is_err(),
            "zero width"
        );
        let _ = std::fs::remove_file(&out);
    }

    /// Three encoder-shaped access units out, then back in through both `mp4`'s
    /// own reader and our `Demuxer` — the round trip is what proves the repack is
    /// the exact inverse of `demux::append_annex_b`.
    #[test]
    fn round_trips_through_both_readers() {
        let units = [
            au(&[SPS, PPS, &[0x65, 0x01, 0x02, 0x03]]),
            au(&[&[0x41, 0x04, 0x05]]),
            au(&[&[0x41, 0x06]]),
        ];
        let out = std::env::temp_dir().join(format!("ve_mux_{}.mp4", std::process::id()));

        let mut muxer = Mp4Muxer::create(
            &out,
            &VideoParams {
                width: 1280,
                height: 720,
                frame_rate: 30.0,
                sps: SPS,
                pps: PPS,
            },
            Some(&AudioParams {
                freq_index: 4, // 44100
                chan_conf: 2,
                sample_rate: 44_100,
            }),
        )
        .unwrap();
        for unit in &units {
            muxer.write_video_au(unit).unwrap();
        }
        muxer.write_audio_packet(&[0x21, 0x22, 0x23]).unwrap();
        muxer.write_audio_packet(&[0x24, 0x25]).unwrap();
        muxer.finish().unwrap();

        let file = File::open(&out).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).unwrap();
        let (video_id, audio_id) = (VIDEO_TRACK, AUDIO_TRACK);
        assert_eq!(reader.sample_count(video_id).unwrap(), 3);
        assert_eq!(reader.sample_count(audio_id).unwrap(), 2);
        {
            let track = &reader.tracks()[&video_id];
            assert_eq!((track.width(), track.height()), (1280, 720));
            assert_eq!(track.frame_rate().round(), 30.0);
            assert_eq!(track.sequence_parameter_set().unwrap(), SPS);
            assert_eq!(track.picture_parameter_set().unwrap(), PPS);
        }
        let first = reader.read_sample(video_id, 1).unwrap().unwrap();
        assert!(first.is_sync, "IDR unit is a sync sample");
        assert_eq!(first.duration, 3000, "90000 / 30 fps");
        assert_eq!(&first.bytes[..], [0, 0, 0, 4, 0x65, 0x01, 0x02, 0x03]);
        assert!(!reader.read_sample(video_id, 2).unwrap().unwrap().is_sync);
        assert_eq!(
            &reader.read_sample(audio_id, 1).unwrap().unwrap().bytes[..],
            [0x21, 0x22, 0x23],
            "audio packets copied verbatim"
        );
        let second = reader.read_sample(audio_id, 2).unwrap().unwrap();
        assert_eq!(&second.bytes[..], [0x24, 0x25], "verbatim");
        assert_eq!(
            second.duration, AAC_PACKET_SAMPLES,
            "every AAC-LC packet is one 1024-frame stts entry"
        );

        let (meta, mut demuxer) = crate::demux::Demuxer::open(&out).unwrap();
        assert_eq!(meta.frame_count, 3);
        assert_eq!((meta.width, meta.height), (1280, 720));
        assert_eq!(meta.frame_rate.round(), 30.0);
        for expected in &units {
            assert_eq!(
                &demuxer.next_access_unit().unwrap().unwrap(),
                expected,
                "demux returns the exact bytes that went in"
            );
        }
        assert!(demuxer.next_access_unit().unwrap().is_none());

        std::fs::remove_file(&out).unwrap();
    }

    /// One OBU in the low-overhead format: header byte, then a one-byte LEB128
    /// size, which is all a payload this short needs.
    fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![(kind << 3) | 0x02, payload.len() as u8];
        out.extend_from_slice(payload);
        out
    }

    /// An EBML size is written in the fewest bytes that hold it -- and never in
    /// a length whose value is all ones, which is the *unknown* size the reader
    /// would take for "runs to the end of its parent".
    #[test]
    fn element_sizes_never_spell_an_unknown_length() {
        let size = |n| {
            let mut out = Vec::new();
            put_size(&mut out, n);
            out
        };
        assert_eq!(size(0), [0x80]);
        assert_eq!(size(126), [0xFE]);
        assert_eq!(size(127), [0x40, 0x7F], "127 is all ones in one byte");
        assert_eq!(size(128), [0x40, 0x80]);
        assert_eq!(size(0x3FFE), [0x7F, 0xFE]);
        assert_eq!(size(0x3FFF), [0x20, 0x3F, 0xFF], "and again at two bytes");
        // Ids are their own bytes, marker and all: that is how `demux` compares
        // them, so a one-byte id must not grow leading zeros here.
        let mut id = Vec::new();
        put_id(&mut id, SIMPLE_BLOCK);
        assert_eq!(id, [0xA3]);
        let mut id = Vec::new();
        put_id(&mut id, CLUSTER);
        assert_eq!(id, [0x1F, 0x43, 0xB6, 0x75]);
    }

    /// One second of a 440 Hz tone as AAC-LC packets, through the very encoder
    /// an export re-encodes with: real bitstream, so a file written with it is
    /// one a decoder either plays or does not.
    fn tone_packets(rate: u32, channels: u16) -> Vec<crate::AacPacket> {
        let frames = rate as usize;
        let mut pcm = Vec::with_capacity(frames * usize::from(channels));
        for i in 0..frames {
            let s = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
            for _ in 0..channels {
                pcm.push(s);
            }
        }
        let mut encoder = rusty_aac::AacEncoder::new(rusty_aac::AacEncoderConfig::default());
        encoder.push_pcm(&pcm, channels, rate).unwrap();
        encoder.finish();
        let mut packets = Vec::new();
        while let Ok(packet) = encoder.next_packet() {
            packets.push(crate::AacPacket {
                bytes: packet.data,
                samples: packet.duration,
            });
        }
        assert!(packets.len() > 10, "a second of audio is many packets");
        packets
    }

    /// The Matroska file carries **sound**, and this project's own reader is
    /// what hears it: picture and an AAC track out through the muxer, then the
    /// picture back through our EBML reader and the sound back through the
    /// symphonia `mkv` reader `audio::Track::open` uses. An AV1 export used to be
    /// picture alone, which is a file whose sound went missing with nobody told.
    #[test]
    fn a_matroska_file_carries_its_picture_and_its_sound() {
        let sequence = obu(1, &[0x11, 0x22, 0x33]);
        let mut key = sequence.clone();
        key.extend_from_slice(&obu(6, &[0xAA; 8]));
        let inter = obu(6, &[0xBB; 5]);
        let packets = tone_packets(48_000, 2);
        let heard: usize = packets.iter().map(|p| p.samples as usize).sum();

        let out = std::env::temp_dir().join(format!("ve_mkv_sound_{}.mkv", std::process::id()));
        let mut muxer = MkvMuxer::create(
            &out,
            &Av1Params {
                width: 640,
                height: 360,
                frame_rate: 30.0,
                config: av1_sequence_header(&key).unwrap(),
            },
            Some((
                &AudioParams {
                    freq_index: 3, // 48000
                    chan_conf: 2,
                    sample_rate: 48_000,
                },
                packets,
            )),
            None,
        )
        .unwrap();
        // Half a second of picture under a second of sound: the tail of the
        // audio outlives the last frame and must still be in the file.
        muxer.write_frame(&key, true).unwrap();
        for _ in 0..14 {
            muxer.write_frame(&inter, false).unwrap();
        }
        muxer.finish().unwrap();

        // The picture is untouched by the sound beside it: same units, same
        // count, same sequence header in front of the keyframe.
        let (meta, mut demuxer) = crate::demux::Demuxer::open(&out).expect("reopen");
        assert_eq!(meta.frame_count, 15);
        assert_eq!(meta.codec, crate::demux::Codec::Av1);
        let first = demuxer.next_access_unit().unwrap().unwrap();
        assert_eq!(&first[sequence.len()..], &key[..]);
        assert_eq!(demuxer.next_access_unit().unwrap().unwrap(), inter);

        // ...and the sound is there, by name and then by ear.
        assert_eq!(
            crate::demux::matroska_audio_codec(&out).unwrap().as_deref(),
            Some("A_AAC")
        );
        let probe = crate::audio::AudioSession::probe(&out, 0)
            .unwrap()
            .expect("the file has an audio track");
        assert_eq!((probe.sample_rate, probe.channels), (48_000, 2));
        let (audio, chunks) = crate::audio::AudioSession::open(&out)
            .unwrap()
            .expect("decodes through symphonia's mkv reader");
        assert_eq!((audio.sample_rate, audio.channels), (48_000, 2));
        let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
        let frames = samples.len() / 2;
        assert!(
            frames + 1024 >= heard && frames <= heard + 2048,
            "{frames} frames read back of {heard} written"
        );
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        assert!(rms > 0.1, "the tone came back silent (rms {rms})");
        std::fs::remove_file(&out).unwrap();
    }

    /// The same stream in the other container: AV1 into an mp4, whose `av01`
    /// sample entry is written by hand, read back through our own demuxer --
    /// which is the only test that the patched box tree is still an mp4.
    #[test]
    fn an_av1_track_round_trips_through_an_mp4() {
        let sequence = obu(1, &[0x11, 0x22, 0x33]);
        let mut key = sequence.clone();
        key.extend_from_slice(&obu(6, &[0xAA; 8]));
        let inter = obu(6, &[0xBB; 5]);
        let packets = tone_packets(48_000, 2);

        let out = std::env::temp_dir().join(format!("ve_mp4_av1_{}.mp4", std::process::id()));
        let mut muxer = Mp4Muxer::create_av1(
            &out,
            &Av1Params {
                width: 640,
                height: 360,
                frame_rate: 30.0,
                config: av1_sequence_header(&key).unwrap(),
            },
            Some(&AudioParams {
                freq_index: 3,
                chan_conf: 2,
                sample_rate: 48_000,
            }),
        )
        .unwrap();
        muxer.write_coded_sample(&key, true).unwrap();
        muxer.write_coded_sample(&inter, false).unwrap();
        muxer.write_coded_sample(&inter, false).unwrap();
        for packet in &packets {
            muxer.write_audio_packet(&packet.bytes).unwrap();
        }
        muxer.finish().unwrap();

        let (meta, mut demuxer) = crate::demux::Demuxer::open(&out).expect("reopen");
        assert_eq!(meta.codec, crate::demux::Codec::Av1, "read back as AV1");
        assert_eq!((meta.width, meta.height), (640, 360));
        assert!((meta.frame_rate - 30.0).abs() < 1e-6, "{}", meta.frame_rate);
        assert_eq!(meta.frame_count, 3);
        // The sequence header off `av1C` in front of the keyframe, exactly as
        // the Matroska reader puts the one off `CodecPrivate` there.
        let first = demuxer.next_access_unit().unwrap().unwrap();
        assert_eq!(&first[..sequence.len()], &sequence[..]);
        assert_eq!(&first[sequence.len()..], &key[..]);
        assert_eq!(demuxer.next_access_unit().unwrap().unwrap(), inter);
        assert_eq!(demuxer.next_access_unit().unwrap().unwrap(), inter);
        assert!(demuxer.next_access_unit().unwrap().is_none());
        assert_eq!(demuxer.seek_to_sync_at_or_before(2), 0, "one keyframe");

        // ...and the audio track beside it is the mp4 path's own, unchanged by
        // the patch: same packets, playable through the same reader.
        let (audio, chunks) = crate::audio::AudioSession::open(&out)
            .unwrap()
            .expect("the mp4's AAC track");
        assert_eq!((audio.sample_rate, audio.channels), (48_000, 2));
        let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        assert!(rms > 0.1, "the tone came back silent (rms {rms})");
        std::fs::remove_file(&out).unwrap();
    }

    /// The Matroska half of the round trip, with no encoder in it: three coded
    /// units out through the muxer and back in through our own demuxer, which is
    /// where the hand-written EBML meets the hand-written EBML reader.
    #[test]
    fn an_av1_track_round_trips_through_our_own_matroska_reader() {
        let sequence = obu(1, &[0x11, 0x22, 0x33]);
        let mut key = sequence.clone();
        key.extend_from_slice(&obu(6, &[0xAA; 8]));
        let inter = obu(6, &[0xBB; 5]);

        let out = std::env::temp_dir().join(format!("ve_mkv_{}.mkv", std::process::id()));
        let config = av1_sequence_header(&key).expect("the keyframe carries one");
        assert_eq!(config, &sequence[..], "the sequence header OBU, whole");
        let mut muxer = MkvMuxer::create(
            &out,
            &Av1Params {
                width: 640,
                height: 360,
                frame_rate: 30.0,
                config,
            },
            None,
            None,
        )
        .unwrap();
        muxer.write_frame(&key, true).unwrap();
        muxer.write_frame(&inter, false).unwrap();
        muxer.write_frame(&inter, false).unwrap();
        muxer.finish().unwrap();

        let (meta, mut demuxer) = crate::demux::Demuxer::open(&out).expect("reopen");
        assert_eq!(meta.codec, crate::demux::Codec::Av1);
        assert_eq!((meta.width, meta.height), (640, 360));
        assert!(
            (meta.frame_rate - 30.0).abs() < 1e-6,
            "DefaultDuration must state the rate exactly: {}",
            meta.frame_rate
        );
        assert_eq!(meta.frame_count, 3);

        // The keyframe comes back with the `CodecPrivate` sequence header in
        // front of it, which is the demuxer's own doing -- so what this checks
        // is that the record was written and parsed, not that the block grew.
        let first = demuxer.next_access_unit().unwrap().unwrap();
        assert_eq!(&first[..sequence.len()], &sequence[..]);
        assert_eq!(&first[sequence.len()..], &key[..]);
        assert_eq!(demuxer.next_access_unit().unwrap().unwrap(), inter);
        assert_eq!(demuxer.next_access_unit().unwrap().unwrap(), inter);
        assert!(demuxer.next_access_unit().unwrap().is_none());
        // One keyframe, so every seek rewinds to it -- and the flag really was
        // written, or block 0 would be the answer by default rather than by
        // being a sync point.
        assert_eq!(demuxer.seek_to_sync_at_or_before(2), 0);

        // A track with no sequence header to declare is refused where the
        // message still means something, rather than written unplayable.
        assert!(
            MkvMuxer::create(
                &out,
                &Av1Params {
                    width: 640,
                    height: 360,
                    frame_rate: 30.0,
                    config: &[],
                },
                None,
                None,
            )
            .is_err()
        );
        assert!(av1_sequence_header(&inter).is_none(), "no sequence header");
        std::fs::remove_file(&out).unwrap();
    }

    /// The whole pipe at an NTSC rate: what we write is what we read back, to
    /// the ninth decimal. Before this bug was fixed the same round trip lost
    /// 23.976 on the way out (a rounded frame duration) *and* on the way back
    /// in (`mp4`'s integer `frame_rate`), and an hour of export drifted a second.
    #[test]
    fn an_ntsc_rate_survives_the_round_trip() {
        const NTSC: f64 = 24_000.0 / 1001.0;
        let out = std::env::temp_dir().join(format!("ve_mux_ntsc_{}.mp4", std::process::id()));
        let mut muxer = Mp4Muxer::create(
            &out,
            &VideoParams {
                width: 640,
                height: 360,
                frame_rate: NTSC,
                sps: SPS,
                pps: PPS,
            },
            None,
        )
        .unwrap();
        muxer
            .write_video_au(&au(&[SPS, PPS, &[0x65, 0x01]]))
            .unwrap();
        muxer.write_video_au(&au(&[&[0x41, 0x02]])).unwrap();
        muxer.finish().unwrap();

        let file = File::open(&out).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).unwrap();
        assert_eq!(reader.tracks()[&VIDEO_TRACK].timescale(), 24_000);
        let first = reader.read_sample(VIDEO_TRACK, 1).unwrap().unwrap();
        assert_eq!(first.duration, 1_001, "one frame is a whole 1001 ticks");

        let (meta, _) = crate::demux::Demuxer::open(&out).unwrap();
        assert!(
            (meta.frame_rate - NTSC).abs() < 1e-9,
            "read back as {} fps, not {NTSC}",
            meta.frame_rate
        );
        std::fs::remove_file(&out).unwrap();
    }
}
