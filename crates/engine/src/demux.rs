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

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use mp4::{Mp4Reader, Mp4Track, TrackType};

use crate::audio::{edit_media_time, packet_at, stts_pairs};
use crate::colorspace::{ColorDescription, ContentLight, Tags, bitstream_tags};

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

    /// The other refusal: the plugin *is* here and opened the file, and then
    /// could not decode a picture out of it. Not a missing decoder, and saying
    /// so would send the user installing something they already have.
    pub fn undecodable(self) -> String {
        format!(
            "this {name} stream cannot be decoded — the VA-API plugin opened it, gave no picture",
            name = self.name()
        )
    }
}

/// The capability matrix: every [`Codec`] the decoder layer can be handed, with
/// the id that names it in each container -- the Matroska `CodecID` and the mp4
/// `stsd` sample entry fourcc. Both dispatches read *this*, so a codec is
/// reachable from both containers or from neither, never from one because
/// somebody wrote an arm and forgot its twin (VP9 was exactly that: decoded out
/// of an mp4, refused out of a `.webm`, with the VA-API decoder wired all along).
///
/// A cell that is deliberately absent is a row in [`UNSUPPORTED`] with a reason,
/// never a silent fall-through; `every_codec_is_reachable_from_both_containers`
/// holds both halves.
const CODEC_IDS: &[(Codec, &str, &[u8; 4])] = &[
    (Codec::H264, "V_MPEG4/ISO/AVC", b"avc1"),
    (Codec::Hevc, "V_MPEGH/ISO/HEVC", b"hvc1"),
    (Codec::Vp9, "V_VP9", b"vp09"),
    (Codec::Av1, "V_AV1", b"av01"),
];

/// The written-down gaps: container ids -- a Matroska `CodecID` or a four-byte
/// mp4 fourcc -- this reads no picture out of, and why. Asserted to be genuinely
/// unreachable by the same test that asserts [`CODEC_IDS`] is reachable, so a gap
/// that gets closed has to be moved out of here rather than left lying as a false
/// claim.
const UNSUPPORTED: &[(&str, &str)] = &[
    // The mp4 path re-injects the parameter sets out of the `avcC` ahead of
    // every sync sample and has nothing to inject for an `avc3` track, which
    // carries them in-band only; `mp4 0.14` parses no `avc3` sample entry at all
    // either, so `sequence_parameter_set()` would come back empty-handed.
    ("avc3", "H.264 with parameter sets in-band only"),
    ("vp08", "no VP8 decoder: cros-codecs carries none"),
    ("V_VP8", "no VP8 decoder: cros-codecs carries none"),
    ("V_MPEG2", "no MPEG-2 decoder, hardware or software"),
    (
        "V_MS/VFW/FOURCC",
        "a codec inside a BITMAPINFOHEADER; nothing here unwraps one",
    ),
];

/// Which codec a Matroska `CodecID` names, or `None` for a track this reads no
/// picture out of. The mkv half of [`CODEC_IDS`].
fn mkv_codec(id: &str) -> Option<Codec> {
    CODEC_IDS
        .iter()
        .find(|(_, mkv, _)| *mkv == id)
        .map(|&(codec, ..)| codec)
}

/// Which codec an mp4 `stsd` sample entry fourcc names. The mp4 half of
/// [`CODEC_IDS`], plus `hev1`: HEVC arrives under either fourcc and the record
/// the parameter sets come out of is the same `hvcC` either way.
fn mp4_codec(fourcc: &[u8; 4]) -> Option<Codec> {
    if fourcc == b"hev1" {
        return Some(Codec::Hevc);
    }
    CODEC_IDS
        .iter()
        .find(|(_, _, mp4)| *mp4 == fourcc)
        .map(|&(codec, ..)| codec)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoMeta {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub frame_count: u32,
    pub codec: Codec,
    /// What this stream's samples mean -- which matrix, which transfer curve,
    /// which range -- resolved off the container's tags, the bitstream's, and
    /// the resolution, in that order. See [`crate::colorspace`].
    pub color: ColorDescription,
}

/// What a file spends its bytes at, in bits per second -- the whole file, and
/// what each of its streams costs inside it.
///
/// The three are one budget split three ways, so all three are measured over
/// **the same seconds**: the container's own duration, not each track's. That
/// is what makes the line read as a breakdown -- `total` can never come out
/// below `video + audio`, because those are disjoint byte counts over a shared
/// denominator, and the leftover is the container's overhead. Dividing each
/// track by its own length is how a file with 2 s of picture and 60 s of sound
/// reports a total thirty times its real byte rate, under components that add
/// up to a thirtieth of it.
///
/// Every field is `None` rather than `0` wherever the container does not state
/// the number and nothing here derives it. A properties row a header did not
/// give is left out rather than shown as a zero, and a fabricated `0 kbps` is
/// not a missing measurement, it is a wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaBitrate {
    /// Every byte of the file over its playing time -- both streams, the
    /// container's overhead and all -- which is what "the bitrate of a file"
    /// means to anyone reading it, and what `ffprobe` calls `format=bit_rate`.
    pub total: Option<u64>,
    /// The picture track's own bytes over that same time.
    pub video: Option<u64>,
    /// The bytes of the **one sound track this engine would play**, over that
    /// same time -- first in file order, which is the rule
    /// [`crate::audio::AudioSession::probe_streams`] hands out stream 0 by and
    /// the one playback and export open. A dual-audio file has more, and this
    /// number describes neither their sum nor the biggest of them; ask
    /// `probe_streams` how many there are before calling this "the sound".
    pub audio: Option<u64>,
}

/// What `path` is coded at, from its header and its sample table alone --
/// **no decoder is opened**, which is what lets a library probe run this over
/// every file it lists.
///
/// The cost is one [`Demuxer::open`]: an mp4's boxes, or a Matroska segment's
/// element headers, both of which the caller's other per-file probes already
/// pay. Nothing here reads a block payload.
///
/// Cheap is not instant, and the caller has to know which: measured over
/// `a local media folder`, every mp4 answered in single-digit milliseconds, and a
/// 12.9 GB 2160p AV1 Matroska took **seconds** cold and ~150 ms warm --
/// Matroska indexes no samples, so its open walks every cluster header in the
/// file, and what that costs is disk, not arithmetic. Timings on that file
/// swing between ~150 ms and ~1.2 s run to run on the same build, so treat any
/// figure here as an order of magnitude and nothing finer. This belongs on
/// `background_executor` like every other open, never on a repaint.
///
/// A file that will not open at all -- no video track, a codec nothing reads,
/// a still image, which has no playing time of its own -- comes back all
/// `None`. That is the answer, not an error: the cards this feeds show the
/// rows they have.
pub fn probe_bitrate(path: &Path) -> MediaBitrate {
    let unstated = MediaBitrate::default();
    let Ok(bytes) = std::fs::metadata(path).map(|m| m.len()) else {
        return unstated;
    };
    // A still is as long as the clip is stretched to, so there is no per-second
    // anything to state about the file.
    if crate::is_image(path) {
        return unstated;
    }
    // A standalone audio file's length is not a frame count; it is what the
    // audio header says, exactly as `PlaybackSession::import` reads it.
    //
    // corner-cut: `duration_secs` falls back to a decode for a stream whose
    // header states no length (a bare mp3 with no Xing header), which is the
    // one input here that is not header-only -- ~0.5 ms per source second, the
    // same cost import already pays for that file. Upgrade path is a
    // header-only variant that answers `None` instead of decoding.
    if crate::is_audio(path) {
        let Ok(Some(secs)) = crate::AudioSession::duration_secs(path) else {
            return unstated;
        };
        let total = bits_per_second(bytes, secs);
        // One stream and nothing else in the file: what a second of it costs is
        // what a second of its sound costs, give or take a header.
        return MediaBitrate {
            total,
            video: None,
            audio: total,
        };
    }
    let Ok((meta, mut demuxer)) = Demuxer::open(path) else {
        return unstated;
    };
    // The one caller that wants the whole index rather than a window: this adds
    // block *lengths* up, and a Matroska open only walks the clusters a read
    // asked for ([`MkvDemuxer`]). So the walk this probe always paid is paid
    // here, where the numbers need it, and not by the opens that seek and
    // decode. A file that will not walk keeps whatever window it has -- the
    // rates then describe part of the file, which is why the failure is not
    // ignored but taken as "no measurement".
    if let Demuxer::Mkv(d) = &mut demuxer
        && d.complete_index().is_err()
    {
        return unstated;
    }
    // How long the *file* plays, which on a well-made file is how long its
    // picture plays and on a badly-made one is not: a 2 s clip carrying a 60 s
    // commentary is 60 s of file, and dividing its bytes by 2 claims thirty
    // times the byte rate it really has. Both containers state this outright --
    // an mp4 in the `mvhd` (the movie header, which spans every track), a
    // Matroska in `Info.Duration` -- so it is read rather than derived.
    //
    // Where a file states none, the picture's own length is the fallback and
    // the old answer: frame count over frame rate, the same length the timeline
    // lays the clip out at.
    let stated = match &demuxer {
        Demuxer::Mkv(d) => d.container_secs,
        Demuxer::Mp4(d) => {
            let mvhd = &d.reader.moov.mvhd;
            (mvhd.timescale > 0 && mvhd.duration > 0)
                .then(|| mvhd.duration as f64 / f64::from(mvhd.timescale))
        }
    };
    let secs = stated.unwrap_or(match meta.frame_rate > 0.0 {
        true => f64::from(meta.frame_count) / meta.frame_rate,
        false => 0.0,
    });
    let (video, audio) = match &demuxer {
        // Matroska indexes no samples, so what the open's one walk counted *is*
        // the sample table: block bytes per track number ([`mkv_blocks`]), the
        // sound's beside the picture's, neither of them costing a second pass.
        Demuxer::Mkv(d) => {
            let bytes_of = |number| {
                d.track_bytes
                    .iter()
                    .find(|(n, _)| *n == number)
                    .map(|&(_, bytes)| bytes)
            };
            (
                // The picture's blocks are indexed, so its size is that index.
                // Measured, this is the same number `track_bytes` holds for the
                // same track on every fixture and every file in `a local media folder`;
                // the two can only part on a *laced* video track, where one
                // block becomes several `Block`s over the frame bytes alone and
                // the lace header is left out. Nothing here has ever produced
                // one -- lacing is a sound-track trick -- so this is a
                // difference of principle, not one anything has exercised.
                bits_per_second(d.blocks.iter().map(|b| b.len as u64).sum(), secs),
                // The sound's, from the raw block lengths, lace headers
                // included: the two components are measured on slightly
                // different bases for that reason, which is worth tens of bytes
                // a block and nothing on the rate.
                d.audio_number
                    .and_then(bytes_of)
                    .and_then(|bytes| bits_per_second(bytes, secs)),
            )
        }
        Demuxer::Mp4(d) => {
            let tracks = d.reader.tracks();
            // In file order, out of `moov.traks`, for [`Mp4Demuxer::open`]'s
            // reason and for `audio::audio_track_ids`': `tracks()` is a
            // `HashMap`, so "the audio track" of a dual-audio file would
            // otherwise be a different one per run. Deliberately the *same*
            // first-in-file rule the audio path picks stream 0 by, so the
            // bitrate row and the audio row of one card name one track -- see
            // [`MediaBitrate::audio`], which says what that number is not.
            let audio = d
                .reader
                .moov
                .traks
                .iter()
                .map(|trak| trak.tkhd.track_id)
                .find(|id| {
                    tracks
                        .get(id)
                        .is_some_and(|t| matches!(t.track_type(), Ok(TrackType::Audio)))
                });
            let rate = |id| {
                tracks
                    .get(id)
                    .map(mp4_track_bytes)
                    .and_then(|bytes| bits_per_second(bytes, secs))
            };
            (rate(&d.track_id), audio.as_ref().and_then(rate))
        }
    };
    MediaBitrate {
        total: bits_per_second(bytes, secs),
        video,
        audio,
    }
}

/// `bytes` over `secs`, in bits per second, or `None` where that is not a
/// number worth showing: nothing measured either side, or a rate that rounds to
/// zero. A `0` here would read as a real measurement, which is the one answer
/// this must never give.
fn bits_per_second(bytes: u64, secs: f64) -> Option<u64> {
    (secs > 0.0)
        .then(|| (bytes as f64 * 8.0 / secs) as u64)
        .filter(|&bps| bps > 0)
}

/// How many bytes of the file an mp4 track's samples are -- its share of the
/// `mdat`, which over the container's seconds is its share of the byte rate.
///
/// Not `Mp4Track::bitrate`, deliberately, though that looks like the ready-made
/// answer. It divides by the track's *own* duration, which is the arithmetic
/// [`MediaBitrate`] exists to not do; it truncates that duration to whole
/// seconds (`mp4-0.14.0/src/track.rs:213`), so a file under a second comes back
/// `0`; and for an `mp4a` track it answers `esds.avg_bitrate` instead, a number
/// the encoder declared rather than one the file spends, which is `0` on any
/// `esds` that declared none. Bytes are what all three of these fields agree to
/// be measured in.
///
/// `Mp4Track::total_sample_size` is private to that crate; this is it, off the
/// `stsz` fields that are not.
fn mp4_track_bytes(track: &Mp4Track) -> u64 {
    let stsz = &track.trak.mdia.minf.stbl.stsz;
    match stsz.sample_size {
        0 => stsz.sample_sizes.iter().map(|&n| u64::from(n)).sum(),
        uniform => u64::from(uniform) * u64::from(track.sample_count()),
    }
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
        let (meta, mut demuxer) = if is_matroska(path) {
            let (meta, mkv) = MkvDemuxer::open(path)?;
            (meta, Self::Mkv(mkv))
        } else {
            let (meta, mp4) = Mp4Demuxer::open(path)?;
            (meta, Self::Mp4(mp4))
        };
        demuxer.fill_sei_light();
        demuxer.fill_vp9_depth()?;
        Ok((meta, demuxer))
    }

    /// What the file says about how bright its pictures get, [`ContentLight`],
    /// read at open out of the container and -- for an HEVC stream whose
    /// container said nothing -- out of the first access unit's SEI.
    ///
    /// Read by the two sites that build a tone map -- the decode funnel
    /// (`decode::DecodeSession::open_worker`) and the export table -- as the
    /// assumed peak of [`crate::tonemap::Preset::Reference`]. Still not part of
    /// [`VideoMeta`]: both of them have the demuxer itself in hand, so the
    /// number travels no further than the open that read it.
    pub fn light(&self) -> ContentLight {
        match self {
            Self::Mp4(d) => d.light,
            Self::Mkv(d) => d.light,
        }
    }

    /// The bitstream tier of [`Self::light`]: an HEVC encoder writes the film's
    /// peak into an SEI ahead of every keyframe, and a container that carried
    /// none of it -- a web rip's Matroska, which tags the curve and stops there
    /// -- leaves that SEI as the only place the grade still exists.
    ///
    /// One access unit is read for it and the cursor put back where it opened,
    /// and only when the container said nothing at all: a file that spoke is
    /// believed whole rather than half-read, which is what keeps this off the
    /// cost of every open.
    fn fill_sei_light(&mut self) {
        let (codec, light) = match self {
            Self::Mp4(d) => (d.codec, d.light),
            Self::Mkv(d) => (d.codec, d.light),
        };
        if codec != Codec::Hevc || light != ContentLight::default() {
            return;
        }
        let first = self.next_access_unit();
        self.seek_to_sync_at_or_before(0);
        let Ok(Some(au)) = first else {
            return;
        };
        let found = light.over(crate::colorspace::hevc_sei_light(&au));
        match self {
            Self::Mp4(d) => d.light = found,
            Self::Mkv(d) => d.light = found,
        }
    }

    /// The bitstream tier of [`Self::bit_depth`] for VP9, which is *all* of it:
    /// an HEVC track states its depth in an `hvcC` and an AV1 one in an `av1C`,
    /// and a VP9 track states it nowhere either demuxer reads -- a Matroska
    /// `TrackEntry` carries no configuration record at all, and the `vpcC` an mp4
    /// writes is optional and describes the file rather than the frame. So it is
    /// read where it is always true: the uncompressed header of the first
    /// keyframe, which is what [`vp9_bit_depth`] parses.
    ///
    /// Assuming 8 instead is how a profile 2 stream would decode into an NV12
    /// pool and come back as garbage -- the plugin picks its surface pool off
    /// this number ([`crate::hw`]).
    fn fill_vp9_depth(&mut self) -> crate::Result<()> {
        let codec = match self {
            Self::Mp4(d) => d.codec,
            Self::Mkv(d) => d.codec,
        };
        if codec != Codec::Vp9 {
            return Ok(());
        }
        let first = self.next_access_unit();
        self.seek_to_sync_at_or_before(0);
        let Ok(Some(au)) = first else {
            return Ok(());
        };
        // A 12-bit stream is refused by name here, exactly as a 12-bit AV1 is in
        // `parse_av1c`: the plugin has an NV12 pool and a P010 one and no third.
        let Some(depth) = vp9_bit_depth(&au)? else {
            return Ok(());
        };
        match self {
            Self::Mp4(d) => d.bit_depth = depth,
            Self::Mkv(d) => d.bit_depth = depth,
        }
        Ok(())
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
///
/// *Every* extension Matroska states, not the two that carry a film: `.mka` is
/// the sound alone and `.mks` the subtitles alone -- the same bytes, the same
/// reader, so refusing them was this engine refusing a file it parses. The set
/// is the standard's own and closed by it.
pub fn is_matroska(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "mkv" | "mka" | "mks" | "mk3d" | "webm"
        )
    })
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
    /// Where every sync sample really sits on screen: `(display index, 0-based
    /// sample index)` per [`sync_display_order`]. Empty -- and then unused, the
    /// `stss` table above answering on its own -- for a track with no `ctts`
    /// box, whose samples are stored in the order they are shown in.
    sync_display: Vec<(u32, u32)>,
    /// Display index of [`Self::first_sample`], i.e. of frame 0. Zero without a
    /// `ctts` box, and not the sample's own position with one.
    first_display: u32,
    next_sample: u32,
    /// Bits per luma sample; see [`Demuxer::bit_depth`].
    bit_depth: u8,
    /// How bright the track says it gets; see [`Demuxer::light`].
    light: ContentLight,
}

impl Mp4Demuxer {
    fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;

        // In *file* order, out of `moov.traks` rather than `Mp4Reader::tracks`,
        // which is a `HashMap`: iterating that would make "the video track" a
        // different one from one run to the next on a file carrying two, and a
        // saved `.edith` would reopen on the other picture. The audio side reads
        // the same box for the same reason ([`crate::audio::audio_track_ids`]).
        //
        // The fourcc of the sample entry is what names the codec, read out of the
        // file by hand: `mp4 0.14`'s `stsd` parser knows only a `hev1` entry
        // (`stsd.rs:107`) and no `av01` at all, so an `hvc1`-tagged HEVC track --
        // what Apple and ffmpeg's mov muxer write in practice -- and every AV1
        // track report no media type through `media_type()`, and such a file used
        // to read back as having no video. Its sample tables
        // (`stts`/`stsz`/`stsc`/`stss`) are parsed regardless of the entry the
        // crate dropped, so the fourcc is all that is missing. hvc1 differs from
        // hev1 in that the parameter sets may not repeat in-band, which costs
        // nothing: they are re-injected out of `hvcC` ahead of every sync sample
        // either way, exactly as an AV1 sequence header is out of `av1C`.
        let (track, codec) = reader
            .moov
            .traks
            .iter()
            .filter_map(|trak| reader.tracks().get(&trak.tkhd.track_id))
            .find_map(|t| {
                matches!(t.track_type(), Ok(TrackType::Video))
                    .then(|| sample_entry(path, t.track_id()).ok())
                    .flatten()
                    .and_then(|(kind, _)| mp4_codec(&kind))
                    .map(|codec| (t, codec))
            })
            // Named, for the same reason the Matroska door names it: "no video
            // track" is a lie about a file that has one in a codec this does not
            // read, and the fourcc is what tells a user which.
            .ok_or_else(|| mp4_no_video(path, &reader))?;

        let track_id = track.track_id();
        let meta = VideoMeta {
            width: track.width() as u32,
            height: track.height() as u32,
            frame_rate: frame_rate(track),
            frame_count: 0,
            codec,
            // Filled below, once the parameter sets the bitstream tier reads are
            // in hand.
            color: ColorDescription::default(),
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
        // Only a track that carries composition offsets stores its samples out
        // of display order, and only an `stss` table makes "which keyframe" a
        // question at all -- without either, a sample id *is* a display index
        // and the table below would be an index of itself.
        let (sync_display, first_display) = match track.trak.mdia.minf.stbl.ctts.as_ref() {
            Some(ctts) if !sync_samples.is_empty() => {
                let times = composition_times(
                    stts_pairs(track),
                    ctts.entries
                        .iter()
                        .map(|e| (e.sample_count, e.sample_offset)),
                );
                let syncs = sync_display_order(&times, |i| {
                    sync_samples.binary_search(&(i as u32 + 1)).is_ok()
                });
                (syncs, display_index(&times, first_sample - 1))
            }
            _ => (Vec::new(), 0),
        };
        Ok((
            VideoMeta {
                // The samples the edit list trims off the front are not frames of
                // the presentation, so they are not counted as ones.
                frame_count: sample_count.saturating_sub(first_sample - 1),
                color: ColorDescription::resolve(
                    colr_tags(path, track_id).unwrap_or_default(),
                    bitstream_tags(codec, &parameter_sets),
                    meta.height,
                ),
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
                sync_display,
                first_display,
                next_sample,
                bit_depth,
                light: mp4_light(path, track_id),
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
        // With composition offsets in play, which sync sample is "at or before"
        // the target is a question about display order and sample ids cannot
        // answer it ([`sync_display_order`]).
        if !self.sync_display.is_empty() {
            let target = frame.saturating_add(self.first_display);
            let (display, sample) =
                sync_display_at_or_before(&self.sync_display, target).unwrap_or((0, 0));
            self.next_sample = sample + 1;
            return i64::from(display) - i64::from(self.first_display);
        }
        let target = frame
            .saturating_add(self.first_sample)
            .clamp(1, self.sample_count.max(1));
        let chosen = sync_at_or_before(&self.sync_samples, target);
        self.next_sample = chosen;
        i64::from(chosen) - i64::from(self.first_sample)
    }
}

/// One Matroska block of an indexed track: where its bytes are, whether a
/// decoder may start on it, and when it is presented (in `TimestampScale`
/// ticks, which is what a sound track seeks against -- Matroska indexes no
/// samples either). 24 bytes an entry, so an hour of 30 fps costs ~2.6 MB of
/// index -- the price of knowing the frame count and the sync points of a
/// container that indexes neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Block {
    at: u64,
    len: usize,
    key: bool,
    ts: i64,
}

/// AV1, HEVC or H.264 out of a Matroska file (`.mkv`/`.webm`).
///
/// Matroska carries no sample table, so where an mp4 has an `stss` this has to
/// read the file. What it reads is the file's own **`Cues`**: the seek index a
/// muxer writes at the far end of the segment, one entry per keyframe, found
/// through the `SeekHead` in kilobytes rather than gigabytes. The clusters
/// themselves are walked **only where a caller asks to read** -- one cue
/// interval at a time, ten seconds of film -- and the window of [`Block`]s that
/// walk builds is what [`Self::next_access_unit`] hands out.
///
/// That turned a 4.9 GB film's open from 3.2 s cold (9.9 MB of element headers,
/// every cluster in the file) into ~30 ms, and an open happens on every seek and
/// every clip span, not once per file.
///
/// A file with no usable `Cues` -- no index at all, or a track that declares no
/// `DefaultDuration`, so a cue time cannot be turned into a frame number -- gets
/// the whole-segment walk this always did ([`mkv_blocks`]), which is also where
/// a cue index caught disagreeing with the blocks it names degrades to
/// ([`Self::cues_agree`]). Both paths answer the same questions with the same
/// numbers; only what they read to do it differs.
pub struct MkvDemuxer {
    file: File,
    /// The file's own path, kept for one reason: the whole walk this falls back
    /// to is cached beside the user's other caches, and a sidecar is named after
    /// the file it indexes ([`mkv_blocks`]).
    path: PathBuf,
    /// The blocks walked so far, which for a lazily indexed file is the window
    /// around what has been read and not the file: `blocks[i]` is the file's
    /// block `base + i`.
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
    /// The `CodecPrivate` record verbatim; see [`MkvVideo::private`].
    private: Vec<u8>,
    /// Nanoseconds a timestamp tick is worth -- `TimestampScale`, a millisecond
    /// in every file anything writes. What turns a block's own timestamp into
    /// the nanoseconds a copy re-times it by.
    timestamp_scale: u64,
    /// Bytes of the NAL length prefix an HEVC or H.264 block is written with;
    /// unused by AV1, whose block is one temporal unit and carries no prefixes.
    nal_length: usize,
    /// Bits per luma sample; see [`Demuxer::bit_depth`].
    bit_depth: u8,
    /// How bright the track says it gets; see [`Demuxer::light`].
    light: ContentLight,
    /// One block's bytes, reused across access units: a 4K HEVC keyframe is a
    /// megabyte and it is reframed, not handed over, so the read needs a
    /// landing place of its own.
    scratch: Vec<u8>,
    /// The file's own number for the block handed out next -- an index into
    /// `blocks` only after `base` is taken off it.
    next: usize,
    /// The file's number for `blocks[0]`; `0` whenever the index is the whole
    /// walk, which is what makes the two paths one lookup.
    base: usize,
    /// The keyframe index off the file's `Cues`, empty for a file walked whole.
    cues: Vec<Cue>,
    /// [`Self::window_syncs`]'s answer, and the `(base, length)` of the window it
    /// answers for: recomputed when the window moves and not per seek.
    syncs: Vec<(i64, usize)>,
    syncs_for: Option<(usize, usize)>,
    /// How far [`Self::cues_agree`] has got through `cues`: everything before it
    /// has been checked against real blocks, or walked past by a jump, and is
    /// never looked at again.
    checked: usize,
    /// Where the next cluster to be walked into the window begins, and where the
    /// segment it belongs to starts and ends.
    reach: u64,
    segment: (u64, u64),
    /// `TrackNumber` of the picture, which the lazy walk needs to filter the
    /// clusters it opens later by.
    number: u64,
    /// How many blocks the track has in the whole file: `blocks.len()` for a
    /// walked file, and for a cued one the last cue's index plus the blocks
    /// after it, counted at open ([`Self::count_frames`]). Exact either way --
    /// it is the clip's length on the timeline.
    frames: usize,
    /// Whether `blocks` is the whole file rather than a window: what
    /// [`probe_bitrate`] needs before it adds block lengths up, and what says a
    /// read past the end of `blocks` is the end of the track and not a cluster
    /// nobody has walked yet.
    complete: bool,
    /// What the track's `ContentEncodings` asks of every block; [`Unpack::None`]
    /// for a file that declares none, which is most of them.
    unpack: Unpack,
    /// Block bytes per `TrackNumber` over the clusters walked so far -- the
    /// sound track's as well as the picture's, since a block's length is already
    /// parsed on the way to skipping it. [`probe_bitrate`] is the only reader,
    /// and it completes the index first: on a lazily opened file this holds the
    /// window's tracks and not the file's.
    track_bytes: Vec<(u64, u64)>,
    /// `TrackNumber` of the file's first audio track, the one
    /// [`matroska_audio_codec`] also answers for; `None` for a silent file.
    audio_number: Option<u64>,
    /// What `Info.Duration` says the whole *file* plays for, in seconds --
    /// which is not the picture's length on a file whose sound outlasts it.
    /// `None` for the rare file that states none. [`probe_bitrate`] only.
    container_secs: Option<f64>,
}

impl MkvDemuxer {
    fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
        let mut file = File::open(path)?;
        let end = file.metadata()?.len();
        let segment = mkv_segment(&mut file, end)?;
        let (video, audio, other, _, container_secs) = mkv_tracks(&mut file, segment)?;
        let video = match video {
            Some(video) => video,
            // Named, because "no video track" is a lie about a file that has one
            // in a codec this does not read.
            None => {
                return Err(match other {
                    // By the reason the table gives where there is one, so the
                    // refusal says what is missing rather than only what is not.
                    Some(codec) => match UNSUPPORTED.iter().find(|(id, _)| *id == codec) {
                        Some((_, why)) => {
                            format!("{codec} video in a Matroska file is not supported — {why}")
                        }
                        None => format!(
                            "{codec} video in a Matroska file is not supported — AV1, HEVC, H.264 and VP9 are"
                        ),
                    },
                    None => "this Matroska file has no video track".to_string(),
                }
                .into());
            }
        };
        // A picture nothing here can put back together is a refusal at the door,
        // by the name of what the muxer did to it -- not a decode of garbage.
        if let Unpack::Refused(why) = &video.unpack {
            return Err(why.clone().into());
        }
        // `DefaultDuration` is nanoseconds a frame and exact -- 33333333 for
        // 30 fps, 41708333 for 23.976 -- and it is what turns a `CueTime` into a
        // frame number, so a track that states one can be opened off the file's
        // seek index. `TimestampScale` is nanoseconds a tick, so their ratio is
        // ticks a frame, which is the unit `Cues` speak in.
        let ticks_per_frame = video
            .default_duration
            .map(|ns| ns as f64 / video.timestamp_scale as f64)
            .filter(|&t| t > 0.0);
        let cues = match ticks_per_frame {
            Some(ticks) => mkv_cues(&mut file, segment, video.number, ticks)?,
            None => Vec::new(),
        };
        let mut mkv = Self {
            file,
            path: path.to_path_buf(),
            blocks: Vec::new(),
            codec: video.codec,
            config: video.config,
            private: video.private,
            timestamp_scale: video.timestamp_scale,
            nal_length: video.nal_length,
            bit_depth: video.bit_depth,
            light: video.light,
            scratch: Vec::new(),
            next: 0,
            base: 0,
            cues,
            syncs: Vec::new(),
            syncs_for: None,
            checked: 0,
            reach: segment.0,
            segment,
            number: video.number,
            frames: 0,
            complete: false,
            unpack: video.unpack,
            track_bytes: Vec::new(),
            audio_number: audio.first().map(|t| t.number),
            container_secs,
        };
        // What the first cluster holds, and -- for a cued file -- how many
        // blocks there are in the whole of it. Either answer may find the cue
        // index unusable and fall back to the walk, which is why the frame count
        // is read off the demuxer afterwards rather than returned from here.
        mkv.count_frames()?;
        if mkv.frames == 0 {
            return Err(format!(
                "the {} track in this Matroska file has no frames",
                video.codec.name()
            )
            .into());
        }
        // Timing, in this order: what the track declares, then what its own
        // timestamps say. `DefaultDuration` is preferred for being exact; the
        // fallback measures the presentation span instead, and that is in
        // `TimestampScale` ticks (a millisecond, as good as every muxer writes),
        // so it lands within ~0.05 % rather than exactly. Only a file with no
        // `DefaultDuration` reaches it, and such a file was walked whole just
        // above -- the span is the window's, and the window is the file.
        //
        // corner-cut: a genuinely variable-rate file is averaged to one rate here,
        // because a timeline frame is a fixed slice of a second everywhere else
        // in this engine. Per-frame durations are the upgrade path, and they are
        // a project-wide change, not a demuxer one.
        let frame_rate = match video.default_duration {
            Some(ns) => 1e9 / ns as f64,
            None => {
                let span = mkv
                    .blocks
                    .iter()
                    .map(|b| b.ts)
                    .min()
                    .zip(mkv.blocks.iter().map(|b| b.ts).max());
                match span {
                    Some((first, last)) if last > first && mkv.blocks.len() > 1 => {
                        (mkv.blocks.len() - 1) as f64 * 1e9
                            / ((last - first) as f64 * video.timestamp_scale as f64)
                    }
                    // Nothing said and nothing measurable: `fps_from_stts`
                    // answers an mp4 with no timing the same way.
                    _ => 0.0,
                }
            }
        };
        let meta = VideoMeta {
            width: video.width,
            height: video.height,
            frame_rate,
            frame_count: mkv.frames as u32,
            codec: video.codec,
            // `config` is the parameter sets by now -- Annex-B for H.264 and
            // HEVC, the sequence header OBU for AV1 -- which is what the
            // bitstream tier reads.
            color: ColorDescription::resolve(
                video.tags,
                bitstream_tags(video.codec, &mkv.config),
                video.height,
            ),
        };
        Ok((meta, mkv))
    }

    /// How many blocks the track has, and a window ready at the front of the
    /// file for the read that follows.
    ///
    /// For a walked file that is the walk's own length. For a cued one it is the
    /// **last cue's index plus the blocks after it**, counted by walking from
    /// that cue to the end of the segment -- one cue interval, ten seconds of
    /// film, rather than all of it. Exact at open, not a duration times a rate
    /// corrected later: a clip's length on the timeline is the first thing the
    /// UI shows and the last thing that may move under it.
    ///
    /// The first cluster is walked before that, so the cue arithmetic is checked
    /// against real blocks at index 0 (where the file's own numbering starts)
    /// before anything is counted off it.
    fn count_frames(&mut self) -> crate::Result<()> {
        if self.cues.is_empty() {
            return self.complete_index();
        }
        self.walk_cluster()?;
        if !self.cues_agree() {
            return self.complete_index();
        }
        let Some(&last) = self.cues.last() else {
            return self.complete_index();
        };
        self.jump(last)?;
        while self.walk_cluster()? {}
        if !self.complete {
            self.frames = self.base + self.blocks.len();
            // Back to the front: what a decoder asks for first is frame 0, and
            // the tail window is the wrong end of the file for it.
            self.blocks.clear();
            self.track_bytes.clear();
            self.base = 0;
            self.reach = self.segment.0;
        }
        Ok(())
    }

    /// The whole segment walked, as every open used to do it: what a file with
    /// no usable `Cues` gets, and what a cue index caught disagreeing with the
    /// file's own blocks degrades to. Idempotent, and the only thing that sets
    /// [`Self::complete`].
    fn complete_index(&mut self) -> crate::Result<()> {
        if self.complete {
            return Ok(());
        }
        let (blocks, _, track_bytes) =
            mkv_blocks(&self.path, &mut self.file, self.segment, self.number)?;
        self.frames = blocks.len();
        self.blocks = blocks;
        self.track_bytes = track_bytes;
        self.base = 0;
        self.reach = self.segment.1;
        self.cues.clear();
        self.complete = true;
        Ok(())
    }

    /// Walks the next `Cluster` at or after [`Self::reach`] onto the end of the
    /// window. `false` once the segment is out of clusters.
    fn walk_cluster(&mut self) -> crate::Result<bool> {
        let Self {
            file,
            blocks,
            track_bytes,
            reach,
            segment,
            number,
            ..
        } = self;
        let mut laced = Vec::new();
        while let Some((id, body, stop)) = ebml_element(file, *reach, segment.1)? {
            *reach = stop;
            if id != CLUSTER {
                continue;
            }
            mkv_cluster(file, body, stop, *number, blocks, track_bytes, &mut laced)?;
            // Per window rather than per file, which is the one thing the two
            // paths do differently: the last lace of a window has no block after
            // it *yet*. Video is never laced -- lacing is a sound-track trick,
            // and `MkvAudio` reads sound off the whole walk -- so this is a
            // difference of principle rather than one anything exercises.
            mkv_spread_laces(blocks, &laced);
            return Ok(true);
        }
        Ok(false)
    }

    /// Where the block a cue names sits in the window, and what the file's own
    /// number for that block is -- `None` when the window does not hold it.
    ///
    /// The second number is the whole subtlety of reading `Cues`. A cue states a
    /// *timestamp*, and a Matroska timestamp is a presentation time: a stream
    /// with B-frames does not store its blocks in that order, so the count of
    /// frames shown before a cue is not the count of blocks written before it.
    /// The difference is exactly the blocks that sit *after* this one in the
    /// file and are shown before it -- the encoder's reordering depth, two or
    /// three frames -- and it is counted here, in the window that block was
    /// found in, rather than assumed to be zero. Assumed zero, a 60-frame HEVC
    /// fixture reports 62 frames and seeks two frames past every cut.
    ///
    /// Both scans are bounded, and that is not an optimisation: the window grows
    /// by a cluster on every sequential read, so a scan of it per cue was a walk
    /// of the whole film per ten seconds of it -- 4.8 s of reading a 4.9 GB file
    /// became 192 s. The block a cue names sits at `cue.index - base` less
    /// whatever was reordered across it, so it is looked for in the [`REORDER`]
    /// blocks ending there and only a cue that is not where it says it is costs
    /// the full scan -- once, because that answer degrades to the whole walk.
    fn anchor(&self, cue: Cue) -> Option<(usize, usize)> {
        let hint = cue.index.saturating_sub(self.base);
        let lo = hint.saturating_sub(REORDER);
        let hi = (hint + 1).min(self.blocks.len());
        let at = self.blocks[lo..hi]
            .iter()
            .position(|b| b.ts == cue.time)
            .map(|at| lo + at)
            .or_else(|| self.blocks.iter().position(|b| b.ts == cue.time))?;
        // Same bound on the other side: a picture shown before the cue's own is
        // within the encoder's reorder depth of it in storage, which is what
        // `REORDER` is. Counting to the end of the window instead only ever
        // added blocks nothing reordered.
        let tail = self.blocks.len().min(at + 1 + REORDER);
        let lag = self.blocks[at + 1..tail]
            .iter()
            .filter(|b| b.ts < cue.time)
            .count();
        Some((at, cue.index.checked_sub(lag)?))
    }

    /// Every cue the window reaches names a block whose timestamp it states, at
    /// the index the arithmetic says.
    ///
    /// This is what makes a cue index safe to count frames off: `CueTime` over
    /// `DefaultDuration` is a frame number only while the track really runs at
    /// that rate, and a file where it does not -- a variable-rate capture, a
    /// muxer whose cue times name no block at all -- is caught here, wherever
    /// the walk meets a cue, and degrades to the whole walk rather than seeking
    /// to a frame that is not the one asked for.
    ///
    /// Cues within [`REORDER`] of the far end of the window are left for the
    /// next cluster: their own reordering has not been walked yet, so checking
    /// them here would fail a file that is perfectly consistent.
    ///
    /// Each cue is checked **once**, in index order, [`Self::checked`] being how
    /// far that has got: a sequential read calls this per cluster, and rechecking
    /// every cue the window had already covered turned reading a film into
    /// quadratic work. A cue the window skipped past -- one before `base` after a
    /// jump -- cannot be checked against blocks nobody walked, so it is stepped
    /// over rather than failed; the cue a seek actually anchors on is checked by
    /// [`Self::jump`] itself.
    fn cues_agree(&mut self) -> bool {
        let end = (self.base + self.blocks.len()).saturating_sub(REORDER);
        while let Some(&cue) = self.cues.get(self.checked) {
            if cue.index >= end {
                break;
            }
            self.checked += 1;
            if cue.index < self.base {
                continue;
            }
            if !matches!(self.anchor(cue), Some((at, index)) if self.base + at == index) {
                return false;
            }
        }
        true
    }

    /// Drops the window and rebuilds it at `cue`'s cluster: the one place the
    /// file is read out of order, and the whole point of parsing `Cues`.
    ///
    /// The cue's own block is found in that cluster by the timestamp the cue
    /// states, and [`Self::anchor`]'s number for it is what fixes the window's
    /// `base` -- so a cue naming the third block of its cluster still leaves the
    /// two before it numbered as the file numbers them.
    fn jump(&mut self, cue: Cue) -> crate::Result<()> {
        self.blocks.clear();
        self.track_bytes.clear();
        self.base = cue.index;
        self.reach = cue.cluster;
        self.walk_cluster()?;
        // Enough of the file past the cue's own block to count what was
        // reordered around it, which the last cluster of a segment may not have.
        while self
            .blocks
            .iter()
            .position(|b| b.ts == cue.time)
            .is_some_and(|at| self.blocks.len() < at + 1 + REORDER)
            && self.walk_cluster()?
        {}
        match self.anchor(cue) {
            Some((at, index)) if index >= at => self.base = index - at,
            // The cluster the cue points at does not hold the block it names, or
            // names it at an index the file cannot have: the index is not
            // describing this file, so stop believing it.
            _ => self.complete_index()?,
        }
        Ok(())
    }

    /// Walks clusters until the window holds the file's block `want`, or until
    /// the segment runs out of them.
    fn extend_to(&mut self, want: usize) -> crate::Result<()> {
        while !self.complete && self.base + self.blocks.len() <= want {
            if !self.walk_cluster()? {
                break;
            }
            if !self.cues_agree() {
                return self.complete_index();
            }
        }
        Ok(())
    }

    /// The window that holds the file's block `frame`, starting at a keyframe at
    /// or before it -- the cue at or before `frame`, which is where the walk
    /// this replaces would have found one too.
    ///
    /// Reached forward where the window already runs that far (playing on
    /// through a cue costs no jump), rebuilt at the cue where it does not.
    fn window_for(&mut self, frame: usize) -> crate::Result<()> {
        if self.complete {
            return Ok(());
        }
        // Binary search, the cues being sorted by index: a 2160p film indexes
        // eleven thousand keyframes and a seek asks this per call.
        let Some(cue) = self.cues.partition_point(|c| c.index <= frame).checked_sub(1) else {
            // A file cued from somewhere after its own first frame, seeked to
            // before the first cue: nothing indexes that stretch, so the walk
            // does.
            return self.complete_index();
        };
        let cue = self.cues[cue];
        if !(self.base..self.base + self.blocks.len()).contains(&cue.index) {
            self.jump(cue)?;
        }
        self.extend_to(frame)
    }

    /// Next access unit in decode order: the block verbatim for AV1, which is
    /// one temporal unit already, and for VP9, whose block is one (super)frame;
    /// Annex-B for HEVC and H.264, whose blocks hold the same length-prefixed
    /// NALs an mp4 sample does.
    fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
        // Reading off the end of the window is not the end of the track: it is
        // the next cluster, not yet walked. The end of the track is the end of
        // the segment, which is what `extend_to` stops at.
        if self.next >= self.base + self.blocks.len() {
            self.extend_to(self.next)?;
        }
        let Some(&block) = self
            .next
            .checked_sub(self.base)
            .and_then(|i| self.blocks.get(i))
        else {
            return Ok(None);
        };
        self.next += 1;
        let head = if block.key { self.config.len() } else { 0 };
        let Self {
            file,
            scratch,
            unpack,
            ..
        } = self;
        scratch.resize(block.len, 0);
        read_exact_at(file, block.at, scratch)?;
        // What the muxer stripped or compressed, put back before anything reads
        // the bytes as a codec's: a stripped block is not a NAL and an inflated
        // one is not where it was read from.
        unpack.frame(scratch)?;
        if matches!(self.codec, Codec::Av1 | Codec::Vp9) {
            let mut au = Vec::with_capacity(head + self.scratch.len());
            au.extend_from_slice(&self.config[..head]);
            au.extend_from_slice(&self.scratch);
            return Ok(Some(au));
        }
        let mut au = Vec::with_capacity(head + self.scratch.len() + 16);
        au.extend_from_slice(&self.config[..head]);
        append_annex_b(&self.scratch, self.nal_length, &mut au)?;
        Ok(Some(au))
    }

    /// As [`Mp4Demuxer::seek_to_sync_at_or_before`]. Never negative: Matroska
    /// has no edit list, so block 0 is frame 0.
    fn seek_to_sync_at_or_before(&mut self, frame: u32) -> i64 {
        let target = (frame as usize).min(self.frames.saturating_sub(1));
        // The window that holds it, off the `Cues`. A file that cannot be
        // indexed that way answers here by having been walked whole -- either at
        // open or by this call.
        //
        // A read that fails while rebuilding the window leaves the window it had,
        // which is a *different part of the file* -- answering the seek off it
        // would land somewhere the caller did not ask for. So the fallback is
        // named rather than swallowed: the whole walk, the one index that does
        // not depend on the window. If the file cannot be read at all that fails
        // too, and then the window is genuinely all there is; the same error
        // comes back out of the next `next_access_unit`, which has a `Result` to
        // carry it and is where a caller learns the file went away.
        if self.window_for(target).is_err() {
            let _ = self.complete_index();
        }
        // The keyframe shown at or before the target -- not the one *stored*
        // before it, which is a different block on any stream that codes
        // pictures out of order ([`sync_display_order`]).
        let target = target as i64;
        let syncs = self.window_syncs();
        let (display, at) = match syncs.partition_point(|&(display, _)| display <= target) {
            // Nothing in the window shows at or before the target: the earliest
            // sync point it holds, which is the only place a decoder can be
            // started from anyway. The window starts at a cue, which *is* a
            // keyframe, so that is the block the whole walk would land on too.
            0 => syncs.first().copied().unwrap_or((self.base as i64, 0)),
            i => syncs[i - 1],
        };
        self.next = self.base + at;
        display
    }

    /// Every keyframe of the window paired with the display index it really has
    /// in the *file*: `(display, position in the window)`, ascending.
    ///
    /// Which is the one thing a window makes harder than a walk. A picture's
    /// index everywhere above this tier is its rank on screen, and that rank is a
    /// statement about the whole film while this holds ten seconds of it. What
    /// ties the two together is the cue: `Cue::index` *is* a display rank, the
    /// muxer having counted it over the whole file, so the window is ranked among
    /// itself by timestamp ([`sync_display_order`], the same function the mp4
    /// side uses) and that ranking is shifted onto the cue's own number. A file
    /// walked whole is the same rule with the shift at zero -- its window is the
    /// file, and this is then `sync_display_order` exactly.
    ///
    /// Kept for the window it was computed over: a seek into a file that has no
    /// usable cues ranks the whole walk, and scrubbing must not pay that twice.
    fn window_syncs(&mut self) -> &[(i64, usize)] {
        let window = (self.base, self.blocks.len());
        if self.syncs_for == Some(window) {
            return &self.syncs;
        }
        let times: Vec<i64> = self.blocks.iter().map(|b| b.ts).collect();
        let ranked = sync_display_order(&times, |i| self.blocks[i].key);
        let shift = self.window_shift(&ranked);
        self.syncs = ranked
            .into_iter()
            .map(|(display, at)| (i64::from(display) + shift, at as usize))
            .collect();
        self.syncs_for = Some(window);
        &self.syncs
    }

    /// What the window's own ranking has to be moved by to be the file's: the
    /// display index a cue states for one block of the window, less the rank the
    /// window gives that same block. Zero for a file walked whole.
    fn window_shift(&self, ranked: &[(u32, u32)]) -> i64 {
        if self.complete {
            return 0;
        }
        let held = self.base..self.base + self.blocks.len();
        let first = self.cues.partition_point(|c| c.index < held.start);
        for &cue in &self.cues[first..] {
            if !held.contains(&cue.index) {
                break;
            }
            let Some((at, _)) = self.anchor(cue) else {
                continue;
            };
            if let Some(&(display, _)) = ranked.iter().find(|&&(_, pos)| pos as usize == at) {
                return cue.index as i64 - i64::from(display);
            }
        }
        // No cue of the window names a block it holds, which is the index this
        // demuxer would already have degraded to the walk over. Until it does,
        // the window's own numbering is the file's -- exact for anything not
        // reordered, and never more than the reorder depth out.
        self.base as i64
    }

    /// The picture track's `CodecPrivate` as the file holds it: what a copied
    /// track is declared with, so the record and the blocks under it come out of
    /// one file rather than two ([`MkvVideo::private`]).
    pub fn codec_private(&self) -> &[u8] {
        &self.private
    }

    /// How many coded blocks the picture track has -- the count
    /// [`crate::VideoMeta::frame_count`] is, indexed the same way every seek and
    /// every clip in point already indexes it.
    ///
    /// The track's own count and not the open window's: a file opened off its
    /// `Cues` holds ten seconds of blocks and knows the length of all of them
    /// ([`Self::count_frames`]), which is the number a clip was cut against.
    pub fn block_count(&self) -> usize {
        self.frames
    }

    /// Whether block `index` is one a decoder may be started from, which is what
    /// makes it a place a copy may begin or end.
    ///
    /// Asks the whole index, because it is asked about any block of the file and
    /// a window holds one stretch of it. That is the walk this demuxer otherwise
    /// avoids -- paid once per source, by a caller that is about to read every
    /// block anyway, and cheap on the second export of the same file
    /// ([`Self::complete_index`] and the sidecar under it). A file that cannot be
    /// walked answers `false`, which is a copy refused rather than a copy of the
    /// wrong bytes.
    pub fn is_sync(&mut self, index: usize) -> bool {
        if self.complete_index().is_err() {
            return false;
        }
        self.blocks.get(index).is_some_and(|b| b.key)
    }

    /// Whether the blocks are the codec's own bytes -- no `ContentEncodings`
    /// stripping or compression over them. A copy of anything else would hand
    /// the muxer bytes it would have to re-pack, which is no longer a copy.
    pub fn plain_blocks(&self) -> bool {
        self.unpack == Unpack::None
    }

    /// Block `index` exactly as it sits in the file: its bytes, whether it is a
    /// sync point and when it is shown, in nanoseconds off the file's own clock.
    ///
    /// Presentation and not decode: Matroska blocks are stored in decode order
    /// and timestamped in display order, so a stream with B-frames hands back
    /// timestamps that step backwards -- which is exactly what a copy has to
    /// preserve, and why this is not "block index times the frame duration".
    ///
    /// The bytes are borrowed from the read scratch and live until the next
    /// call, as [`Self::next_access_unit`]'s do.
    ///
    /// `index` is the file's own block number, so this holds the whole index
    /// rather than whatever window is open ([`Self::is_sync`] says why, and this
    /// is the caller that makes the walk worth it: a copy reads every block).
    pub fn coded_block(&mut self, index: usize) -> crate::Result<Option<CodedBlock<'_>>> {
        self.complete_index()?;
        let Some(&block) = self.blocks.get(index) else {
            return Ok(None);
        };
        // The cursor moves with it, so a copy that stops and hands the file back
        // to a decoder leaves it where a sequential read would.
        self.next = index + 1;
        let Self {
            file,
            scratch,
            unpack,
            ..
        } = self;
        scratch.resize(block.len, 0);
        read_exact_at(file, block.at, scratch)?;
        unpack.frame(scratch)?;
        Ok(Some(CodedBlock {
            bytes: &self.scratch,
            key: block.key,
            ts_ns: block.ts * self.timestamp_scale as i64,
        }))
    }
}

/// One coded picture as a file holds it, for the export's copy path
/// ([`MkvDemuxer::coded_block`]): the very bytes the muxer writes back out, the
/// sync flag a container states beside them, and when the picture is shown.
pub struct CodedBlock<'a> {
    pub bytes: &'a [u8],
    pub key: bool,
    pub ts_ns: i64,
}

/// Every block of `path`'s picture track a decoder may be started from, by the
/// block index the whole engine counts a source's frames in -- the frames a cut
/// may be placed on for the export to copy the film instead of coding it again
/// ([`crate::export::planned_seats`] is the same question asked of a whole
/// timeline).
///
/// Empty for anything that is not Matroska, which is exactly the set of files
/// the copy path refuses anyway, and for a file whose clusters cannot be walked.
///
/// Costs that walk, once per file (6.7 s on a 12.9 GB film, then a sidecar read
/// of a few milliseconds -- [`MkvDemuxer::is_sync`]): ask it off a render
/// thread.
pub fn sync_points(path: &Path) -> Vec<u32> {
    if !is_matroska(path) {
        return Vec::new();
    }
    let Ok((_, Demuxer::Mkv(mut demuxer))) = Demuxer::open(path) else {
        return Vec::new();
    };
    // `is_sync` completes the index on its first call, so the count is asked
    // after it: before that it is the open window's, not the file's.
    if !demuxer.is_sync(0) && demuxer.block_count() == 0 {
        return Vec::new();
    }
    (0..demuxer.block_count() as u32)
        .filter(|&i| demuxer.is_sync(i as usize))
        .collect()
}

/// The codec id of `path`'s first audio track (`A_AAC`, `A_OPUS`, ...), or
/// `None` for a Matroska file with no sound at all. Header only, and only worth
/// calling once a session has come up silent: a Matroska file's sound is read
/// by symphonia, or by the Dolby decoder where it is AC-3 ([`MkvAudio`]), and
/// this exists to say which codec neither of them took.
pub fn matroska_audio_codec(path: &Path) -> crate::Result<Option<String>> {
    Ok(matroska_audio_tracks(path)?
        .into_iter()
        .next()
        .map(|t| t.codec))
}

/// One audio `TrackEntry` of a Matroska file, as the header describes it.
pub struct MkvAudioTrack {
    /// `TrackNumber`: what this track's blocks are stamped with ([`MkvAudio`]),
    /// and the id symphonia keeps for the same track. *Not* a position -- a
    /// file's first audio track is number 2 as often as not.
    pub number: u64,
    /// The Matroska codec id: `A_AAC`, `A_EAC3`, `A_DTS`, ...
    pub codec: String,
    /// `TrackLanguage`, `"und"` for an entry that declares none (the spec's own
    /// default), which is what a muxer writes when nobody said.
    pub language: String,
    /// `TrackName` -- "Japanese 5.1", "Commentary" -- empty when there is none.
    pub name: String,
    /// What this track's blocks have to be put back through.
    unpack: Unpack,
}

/// Every audio track of a Matroska file, in file order: the numbering
/// [`crate::AudioSession::probe_streams`] hands out, and the one a dual-audio
/// remux is picked from. Header only, like [`matroska_audio_codec`], and an
/// empty list is a file with no sound at all rather than an error.
pub fn matroska_audio_tracks(path: &Path) -> crate::Result<Vec<MkvAudioTrack>> {
    let mut file = File::open(path)?;
    let end = file.metadata()?.len();
    let segment = mkv_segment(&mut file, end)?;
    Ok(mkv_tracks(&mut file, segment)?.1)
}

/// The AC-3 / E-AC-3 sound of a Matroska file: every block of its first audio
/// track, in storage order, with the timestamp a seek resolves against. The
/// same walk the picture costs ([`MkvDemuxer`]), on the other track number, and
/// the two keep their own file handle and their own index -- the sound is
/// opened by the audio worker and the picture by the decoder, never together.
///
/// Matroska carries no sample table, so a block's own timestamp is the only
/// thing a window can be resolved against; [`Self::block_at`] is what an `stts`
/// lookup is on the mp4 side.
pub struct MkvAudio {
    file: File,
    blocks: Vec<Block>,
    /// The `oxideav-ac3` codec id these blocks are packets of: `ac3` or `eac3`.
    pub codec: &'static str,
    /// Seconds one `TimestampScale` tick is worth, resolved once at open.
    secs_per_tick: f64,
    /// As [`MkvDemuxer::unpack`], on the sound track.
    unpack: Unpack,
}

impl MkvAudio {
    /// `Ok(None)` when `path` has no audio track at all, or when its first one
    /// is in a codec nothing here decodes -- that file is the silent source it
    /// has always been, and [`crate::audio::AudioSession::unsupported`] names
    /// the codec. An `Err` is a track that *is* AC-3 and could not be read.
    ///
    /// The file's *first* audio track, which is the one this has always read.
    pub fn open(path: &Path) -> crate::Result<Option<Self>> {
        Self::open_inner(path, None)
    }

    /// The audio track stamped `number` ([`MkvAudioTrack::number`], not a
    /// position), for a file carrying more than one -- a dual-audio remux, where
    /// which track is heard is the user's pick and not the muxer's order.
    /// `Ok(None)` for a number this file does not have, or one whose codec is
    /// not AC-3, exactly as [`open`](Self::open) answers for the first track.
    pub fn open_track(path: &Path, number: u64) -> crate::Result<Option<Self>> {
        Self::open_inner(path, Some(number))
    }

    fn open_inner(path: &Path, number: Option<u64>) -> crate::Result<Option<Self>> {
        let mut file = File::open(path)?;
        let end = file.metadata()?.len();
        let segment = mkv_segment(&mut file, end)?;
        let (_, audio, _, timestamp_scale, _) = mkv_tracks(&mut file, segment)?;
        let Some(MkvAudioTrack {
            number,
            codec,
            unpack,
            ..
        }) = (match number {
            Some(number) => audio.into_iter().find(|t| t.number == number),
            None => audio.into_iter().next(),
        })
        else {
            return Ok(None);
        };
        let codec = match codec.as_str() {
            "A_AC3" => "ac3",
            "A_EAC3" => "eac3",
            _ => return Ok(None),
        };
        // Named here rather than at the walk: a sound track this cannot put back
        // together is an error for the *audio* session, and the picture of the
        // same file still opens.
        if let Unpack::Refused(why) = &unpack {
            return Err(why.clone().into());
        }
        let (blocks, _, _) = mkv_blocks(path, &mut file, segment, number)?;
        if blocks.is_empty() {
            return Err("the AC-3 track in this Matroska file has no blocks".into());
        }
        Ok(Some(Self {
            file,
            blocks,
            codec,
            secs_per_tick: timestamp_scale as f64 / 1e9,
            unpack,
        }))
    }

    /// How many blocks the track has; one syncframe each, a laced block having
    /// been split into one per frame by [`mkv_blocks`].
    pub fn blocks(&self) -> usize {
        self.blocks.len()
    }

    /// The bytes of block `index` -- one whole packet for the decoder, an
    /// E-AC-3 frame's dependent substreams included. `None` past the last one.
    pub fn frame(&mut self, index: usize) -> crate::Result<Option<Vec<u8>>> {
        let Some(&block) = self.blocks.get(index) else {
            return Ok(None);
        };
        let mut bytes = vec![0u8; block.len];
        read_exact_at(&mut self.file, block.at, &mut bytes)?;
        self.unpack.frame(&mut bytes)?;
        Ok(Some(bytes))
    }

    /// When block `index` is presented, in seconds on the file's own clock. The
    /// last block's time for anything past the end, which is what a duration
    /// wants.
    pub fn secs(&self, index: usize) -> f64 {
        let index = index.min(self.blocks.len().saturating_sub(1));
        self.blocks.get(index).map_or(0.0, |b| b.ts as f64) * self.secs_per_tick
    }

    /// The block a decoder is started from to hear `secs`: the last one at or
    /// before it, `pre_roll` blocks earlier so the decoder has something to warm
    /// up on before the audible part. Blocks are in storage order, which for a
    /// sound track is presentation order (no reordering exists in AC-3).
    pub fn block_at(&self, secs: f64, pre_roll: usize) -> usize {
        let ticks = (secs / self.secs_per_tick) as i64;
        self.blocks
            .partition_point(|b| b.ts <= ticks)
            .saturating_sub(1 + pre_roll)
    }
}

/// What the `Tracks` element says about the video track.
struct MkvVideo {
    number: u64,
    width: u32,
    height: u32,
    /// What the track's `Colour` element declared, empty when it has none.
    tags: Tags,
    /// What that same element said about brightness (MaxCLL and the mastering
    /// display), likewise empty when it said nothing.
    light: ContentLight,
    default_duration: Option<u64>,
    timestamp_scale: u64,
    codec: Codec,
    config: Vec<u8>,
    /// `CodecPrivate` as the file holds it -- the `hvcC`/`avcC`/`av1C` record
    /// itself, before [`parse_hvcc`] and its siblings turn it into the
    /// parameter sets `config` carries. Kept because a copy hands the very same
    /// record to the muxer ([`MkvDemuxer::codec_private`]): a track written from
    /// a re-derived record would declare a stream subtly other than the blocks
    /// under it.
    private: Vec<u8>,
    nal_length: usize,
    bit_depth: u8,
    unpack: Unpack,
}

/// What a track's `ContentEncodings` element did to every one of its frames,
/// undone here on the way back out. [`Unpack::None`] is every file this project
/// writes and most of what it reads; the rest is what a remuxer does on the way
/// to shaving bytes off a disc rip.
#[derive(Debug, Clone, Default, PartialEq)]
enum Unpack {
    #[default]
    None,
    /// Header stripping (`ContentCompAlgo` 3): bytes every frame of the track
    /// begins with, cut off by the muxer and written once into
    /// `ContentCompSettings`. A decoder handed the rest sees garbage -- one
    /// stripped zero byte is the whole difference between a film that plays and
    /// a film that does not.
    Prepend(Vec<u8>),
    /// zlib (`ContentCompAlgo` 0), which mkvmerge compressed subtitle tracks
    /// with by default for years.
    Zlib,
    /// A scheme this cannot undo, in the words a caller refuses with. Carried
    /// rather than raised where the tracks are walked, so one unreadable *audio*
    /// track does not refuse a file whose picture is fine.
    Refused(String),
}

impl Unpack {
    /// One frame put back the way the encoder wrote it, in place: a block is a
    /// megabyte of keyframe often enough that it is not worth copying whole for
    /// the sake of a byte at the front.
    fn frame(&self, buf: &mut Vec<u8>) -> crate::Result<()> {
        match self {
            Self::None => {}
            Self::Prepend(head) => drop(buf.splice(..0, head.iter().copied())),
            Self::Zlib => {
                // With a ceiling on the way out: a frame is a megabyte of
                // keyframe at worst, and a crafted block must not be able to
                // inflate into all the memory there is.
                const LIMIT: usize = 64 << 20;
                *buf = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(buf, LIMIT)
                    .map_err(|e| format!("a zlib Matroska block did not inflate: {e}"))?;
            }
            Self::Refused(why) => return Err(why.clone().into()),
        }
        Ok(())
    }
}

/// The `ContentEncodings` of one `TrackEntry`. Anything this cannot undo comes
/// back as [`Unpack::Refused`] with the sentence naming the feature it wanted: a
/// compressed track read as if it were plain decodes into garbage, and that is
/// the one thing this must not do quietly.
fn mkv_content_encoding(file: &mut File, body: u64, end: u64) -> crate::Result<Unpack> {
    let mut found: Option<Unpack> = None;
    let mut at = body;
    while let Some((id, body, stop)) = ebml_element(file, at, end)? {
        at = stop;
        if id != CONTENT_ENCODING {
            continue;
        }
        // Scope 1 -- every frame of the track -- is the default and the only one
        // written in practice; the others encode the `CodecPrivate` or the next
        // track, and a `CodecPrivate` this cannot read is a track it cannot open
        // either way.
        let (mut scope, mut kind, mut algo, mut settings) = (1, 0, None, Vec::new());
        let mut child = body;
        while let Some((id, body, stop)) = ebml_element(file, child, stop)? {
            match id {
                CONTENT_ENCODING_SCOPE => scope = ebml_uint(file, body, stop)?,
                CONTENT_ENCODING_TYPE => kind = ebml_uint(file, body, stop)?,
                CONTENT_COMPRESSION => {
                    let mut child = body;
                    while let Some(e) = ebml_element(file, child, stop)? {
                        match e.0 {
                            CONTENT_COMP_ALGO => algo = Some(ebml_uint(file, e.1, e.2)?),
                            CONTENT_COMP_SETTINGS => settings = ebml_bytes(file, e.1, e.2)?,
                            _ => {}
                        }
                        child = e.2;
                    }
                }
                _ => {}
            }
            child = stop;
        }
        let refuse = |what: &str| {
            Unpack::Refused(format!(
                "{what} in a Matroska file are not supported — this track cannot be read back"
            ))
        };
        let unpack = match (kind, scope, algo.unwrap_or(0)) {
            (1, _, _) => refuse("encrypted tracks"),
            (_, s, _) if s & 1 == 0 => refuse("tracks whose headers are the compressed part"),
            // 3 is header stripping and 0 is zlib; 1 (bzlib) and 2 (lzo1x) are
            // named rather than guessed at -- there is no decompressor for
            // either here, and nothing has written one in twenty years.
            (_, _, 3) => Unpack::Prepend(settings),
            (_, _, 0) => Unpack::Zlib,
            (_, _, 1) => refuse("bzlib-compressed tracks"),
            (_, _, 2) => refuse("lzo1x-compressed tracks"),
            (_, _, algo) => refuse(&format!("tracks compressed with algorithm {algo}")),
        };
        found = Some(match found {
            // Chained encodings -- compressed *and* encrypted, say -- are legal
            // and written by nothing. Undoing one of the two is worse than
            // saying so.
            Some(_) => refuse("chained content encodings"),
            None => unpack,
        });
    }
    Ok(found.unwrap_or_default())
}

// Matroska element IDs, written with the leading length marker they carry in
// the file, which is what `ebml_element` reads them back as.
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2AD7B1;
const DURATION: u32 = 0x4489;
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
// The `Colour` element and the three of its children this reads. Byte-verified
// against real files rather than taken off a spec table: published tables that
// list Range as 0x55B3 are describing ChromaSubsamplingHorz.
const COLOUR: u32 = 0x55B0;
const MATRIX_COEFFICIENTS: u32 = 0x55B1;
const RANGE: u32 = 0x55B9;
const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
// ...and how bright it says its pictures get: two whole-nit integers beside the
// code points, and a `MasteringMetadata` container whose ten children are EBML
// *floats* -- eight chromaticities nothing here reads and the two luminances it
// does. Byte-verified the same way, against an ffmpeg-written HDR fixture.
const MAX_CLL: u32 = 0x55BC;
const MAX_FALL: u32 = 0x55BD;
const MASTERING_METADATA: u32 = 0x55D0;
const LUMINANCE_MAX: u32 = 0x55D9;
const LUMINANCE_MIN: u32 = 0x55DA;
const TRACK_LANGUAGE: u32 = 0x22B59C;
/// What a modern muxer states a language with instead ([`mkv_language`]): the
/// legacy element above holds an ISO 639-2 code and nothing else, this one a
/// whole BCP-47 tag (`en`, `pt-BR`, `zh-Hans`).
const TRACK_LANGUAGE_BCP47: u32 = 0x22B59D;
const TRACK_NAME: u32 = 0x536E;
const CONTENT_ENCODINGS: u32 = 0x6D80;
const CONTENT_ENCODING: u32 = 0x6240;
const CONTENT_ENCODING_SCOPE: u32 = 0x5032;
const CONTENT_ENCODING_TYPE: u32 = 0x5033;
const CONTENT_COMPRESSION: u32 = 0x5034;
const CONTENT_COMP_ALGO: u32 = 0x4254;
const CONTENT_COMP_SETTINGS: u32 = 0x4255;
// The seek index and the table of contents that points at it. `SeekHead` is
// how a file whose `Cues` sit at the far end -- which is where every muxer but
// `-cues_to_front` writes them -- is found without reading what is in between.
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const CUES: u32 = 0x1C53_BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TIME: u32 = 0xB3;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_TRACK: u32 = 0xF7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;
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

/// The video track of `segment`, **every** audio track in file order, the codec
/// id of a video track this cannot read, and the segment's `TimestampScale` in
/// nanoseconds. The audio list is what [`MkvAudio`] reads a sound track from and
/// what [`matroska_audio_tracks`] hands a stream picker; it and the other codec
/// id are what tell a user why a file plays silent
/// ([`crate::audio::AudioSession::unsupported`]) or refuses to open at all.
///
/// Header only: the walk stops at the first `Cluster`, so this costs a handful
/// of seeks whatever the file weighs.
fn mkv_tracks(
    file: &mut File,
    segment: (u64, u64),
) -> crate::Result<(
    Option<MkvVideo>,
    Vec<MkvAudioTrack>,
    Option<String>,
    u64,
    Option<f64>,
)> {
    let (mut video, mut other) = (None, None);
    let mut audio = Vec::new();
    let mut timestamp_scale = 1_000_000;
    // `Info.Duration`, in `TimestampScale` ticks: how long the *file* is, which
    // is not how long its picture is -- see [`probe_bitrate`], which divides by
    // this. Written by every muxer in practice and optional by the spec, so the
    // absence of it is a `None` and not a zero.
    let mut duration = None;
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(file, at, segment.1)? {
        match id {
            CLUSTER => break,
            INFO => {
                let mut at = body;
                while let Some(e) = ebml_element(file, at, stop)? {
                    match e.0 {
                        TIMESTAMP_SCALE => timestamp_scale = ebml_uint(file, e.1, e.2)?.max(1),
                        DURATION => duration = Some(ebml_float(file, e.1, e.2)?),
                        _ => {}
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
                            // Every one of them, in the order the `Tracks`
                            // element lists them: that order *is* the stream
                            // numbering everything above hands out, so a
                            // careless filter here plays the wrong language.
                            MkvEntry::Audio(track) => audio.push(track),
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
    // Ticks to seconds: `TimestampScale` is nanoseconds a tick is worth.
    let secs = duration.filter(|d| *d > 0.0).map(|d| d * timestamp_scale as f64 / 1e9);
    Ok((video, audio, other, timestamp_scale, secs))
}

/// What one `TrackEntry` turned out to be.
enum MkvEntry {
    Video(MkvVideo),
    /// A video track in a codec this does not read, by the name it gives itself.
    OtherVideo(String),
    Audio(MkvAudioTrack),
    /// Subtitles, buttons: nobody's here.
    Other,
}

/// What language a `TrackEntry` states, as the three-letter ISO 639-2 code the
/// rest of this engine speaks -- the one the muxer writes back into
/// `TRACK_LANGUAGE` and the one an mp4's `mdhd` packs into 16 bits.
///
/// **`LanguageBCP47` wins**, which is the spec's own precedence and not a
/// preference: a modern file states its languages there and leaves the legacy
/// element out, so reading only the old one lost them. One real 4K web remux
/// carries 37 `LanguageBCP47` elements against 33 legacy
/// ones and every English track of it is BCP-47 only -- they all used to come in
/// as `und` and export with no language at all.
///
/// A tag is cut to its primary subtag and mapped by [`ISO_639_1_TO_2`], so `en`
/// and `en-US` are both `eng`: the region is not the language, and neither the
/// Matroska element nor the mp4 field can hold one. Three letters already
/// (`fil`, and every ISO 639-3 tag) are kept as they are. Anything else -- a
/// private `x-…` tag, a tag nothing maps -- falls back to the legacy element
/// rather than throwing the file's word away.
///
/// `und` for a track that states neither. The spec's *default* is `eng`
/// (measured: ffmpeg 8.1.2 reports `eng` for a `TrackEntry` with no `Language`
/// element, and reads no `LanguageBCP47` at all), but writing English into a
/// track whose file never said so is this engine's claim and not the file's --
/// a Japanese film's untitled track would export labelled English. What is
/// *stated* is kept; the default is left to the readers that want it.
fn mkv_language(legacy: &str, bcp47: &str) -> String {
    let primary = bcp47
        .split('-')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mapped = match primary.len() {
        2 => ISO_639_1_TO_2
            .iter()
            .find(|(two, _)| *two == primary)
            .map(|(_, three)| (*three).to_owned()),
        3 => Some(primary),
        _ => None,
    };
    match mapped {
        Some(code) => code,
        None if !legacy.is_empty() => legacy.to_owned(),
        None => "und".into(),
    }
}

/// Every ISO 639-1 code and the 639-2 (terminology) one it names the same
/// language with -- what [`mkv_language`] maps a BCP-47 primary subtag through.
///
/// The whole standard and not the languages a test happened to use: generated
/// from `iso-codes`' own `iso_639-2.json`, so a Kannada track is as readable as
/// an English one.
#[rustfmt::skip]
const ISO_639_1_TO_2: &[(&str, &str)] = &[
    ("aa", "aar"), ("ab", "abk"), ("ae", "ave"), ("af", "afr"),
    ("ak", "aka"), ("am", "amh"), ("an", "arg"), ("ar", "ara"),
    ("as", "asm"), ("av", "ava"), ("ay", "aym"), ("az", "aze"),
    ("ba", "bak"), ("be", "bel"), ("bg", "bul"), ("bi", "bis"),
    ("bm", "bam"), ("bn", "ben"), ("bo", "bod"), ("br", "bre"),
    ("bs", "bos"), ("ca", "cat"), ("ce", "che"), ("ch", "cha"),
    ("co", "cos"), ("cr", "cre"), ("cs", "ces"), ("cu", "chu"),
    ("cv", "chv"), ("cy", "cym"), ("da", "dan"), ("de", "deu"),
    ("dv", "div"), ("dz", "dzo"), ("ee", "ewe"), ("el", "ell"),
    ("en", "eng"), ("eo", "epo"), ("es", "spa"), ("et", "est"),
    ("eu", "eus"), ("fa", "fas"), ("ff", "ful"), ("fi", "fin"),
    ("fj", "fij"), ("fo", "fao"), ("fr", "fra"), ("fy", "fry"),
    ("ga", "gle"), ("gd", "gla"), ("gl", "glg"), ("gn", "grn"),
    ("gu", "guj"), ("gv", "glv"), ("ha", "hau"), ("he", "heb"),
    ("hi", "hin"), ("ho", "hmo"), ("hr", "hrv"), ("ht", "hat"),
    ("hu", "hun"), ("hy", "hye"), ("hz", "her"), ("ia", "ina"),
    ("id", "ind"), ("ie", "ile"), ("ig", "ibo"), ("ii", "iii"),
    ("ik", "ipk"), ("io", "ido"), ("is", "isl"), ("it", "ita"),
    ("iu", "iku"), ("ja", "jpn"), ("jv", "jav"), ("ka", "kat"),
    ("kg", "kon"), ("ki", "kik"), ("kj", "kua"), ("kk", "kaz"),
    ("kl", "kal"), ("km", "khm"), ("kn", "kan"), ("ko", "kor"),
    ("kr", "kau"), ("ks", "kas"), ("ku", "kur"), ("kv", "kom"),
    ("kw", "cor"), ("ky", "kir"), ("la", "lat"), ("lb", "ltz"),
    ("lg", "lug"), ("li", "lim"), ("ln", "lin"), ("lo", "lao"),
    ("lt", "lit"), ("lu", "lub"), ("lv", "lav"), ("mg", "mlg"),
    ("mh", "mah"), ("mi", "mri"), ("mk", "mkd"), ("ml", "mal"),
    ("mn", "mon"), ("mr", "mar"), ("ms", "msa"), ("mt", "mlt"),
    ("my", "mya"), ("na", "nau"), ("nb", "nob"), ("nd", "nde"),
    ("ne", "nep"), ("ng", "ndo"), ("nl", "nld"), ("nn", "nno"),
    ("no", "nor"), ("nr", "nbl"), ("nv", "nav"), ("ny", "nya"),
    ("oc", "oci"), ("oj", "oji"), ("om", "orm"), ("or", "ori"),
    ("os", "oss"), ("pa", "pan"), ("pi", "pli"), ("pl", "pol"),
    ("ps", "pus"), ("pt", "por"), ("qu", "que"), ("rm", "roh"),
    ("rn", "run"), ("ro", "ron"), ("ru", "rus"), ("rw", "kin"),
    ("sa", "san"), ("sc", "srd"), ("sd", "snd"), ("se", "sme"),
    ("sg", "sag"), ("si", "sin"), ("sk", "slk"), ("sl", "slv"),
    ("sm", "smo"), ("sn", "sna"), ("so", "som"), ("sq", "sqi"),
    ("sr", "srp"), ("ss", "ssw"), ("st", "sot"), ("su", "sun"),
    ("sv", "swe"), ("sw", "swa"), ("ta", "tam"), ("te", "tel"),
    ("tg", "tgk"), ("th", "tha"), ("ti", "tir"), ("tk", "tuk"),
    ("tl", "tgl"), ("tn", "tsn"), ("to", "ton"), ("tr", "tur"),
    ("ts", "tso"), ("tt", "tat"), ("tw", "twi"), ("ty", "tah"),
    ("ug", "uig"), ("uk", "ukr"), ("ur", "urd"), ("uz", "uzb"),
    ("ve", "ven"), ("vi", "vie"), ("vo", "vol"), ("wa", "wln"),
    ("wo", "wol"), ("xh", "xho"), ("yi", "yid"), ("yo", "yor"),
    ("za", "zha"), ("zh", "zho"), ("zu", "zul"),
];

/// One `TrackEntry`, read for what it is.
fn mkv_track_entry(
    file: &mut File,
    body: u64,
    end: u64,
    timestamp_scale: u64,
) -> crate::Result<MkvEntry> {
    let (mut number, mut kind, mut codec, mut default_duration) = (0, 0, String::new(), None);
    let (mut width, mut height, mut config) = (0, 0, Vec::new());
    let (mut language, mut name, mut bcp47) = (String::new(), String::new(), String::new());
    let mut tags = Tags::default();
    let mut light = ContentLight::default();
    let mut unpack = Unpack::None;
    let mut at = body;
    while let Some((id, body, stop)) = ebml_element(file, at, end)? {
        match id {
            TRACK_NUMBER => number = ebml_uint(file, body, stop)?,
            CONTENT_ENCODINGS => unpack = mkv_content_encoding(file, body, stop)?,
            TRACK_TYPE => kind = ebml_uint(file, body, stop)?,
            TRACK_LANGUAGE => language = string_of(file, body, stop)?,
            TRACK_LANGUAGE_BCP47 => bcp47 = string_of(file, body, stop)?,
            TRACK_NAME => name = string_of(file, body, stop)?,
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
                        // `Colour`: what the file says its picture's numbers
                        // mean, in the same H.273 code points the bitstream
                        // uses. An element the file leaves out stays 0, which
                        // is "unspecified" in every one of those tables and
                        // falls to the tier below (see [`crate::colorspace`]).
                        COLOUR => {
                            let (mut matrix, mut transfer, mut range) = (0, 0, 0);
                            let mut at = e.1;
                            while let Some(c) = ebml_element(file, at, e.2)? {
                                match c.0 {
                                    MATRIX_COEFFICIENTS => matrix = ebml_uint(file, c.1, c.2)?,
                                    TRANSFER_CHARACTERISTICS => {
                                        transfer = ebml_uint(file, c.1, c.2)?
                                    }
                                    RANGE => range = ebml_uint(file, c.1, c.2)?,
                                    MAX_CLL => {
                                        light.max_cll = nits(ebml_uint(file, c.1, c.2)? as f64)
                                    }
                                    MAX_FALL => {
                                        light.max_fall = nits(ebml_uint(file, c.1, c.2)? as f64)
                                    }
                                    MASTERING_METADATA => {
                                        let mut at = c.1;
                                        while let Some(m) = ebml_element(file, at, c.2)? {
                                            match m.0 {
                                                LUMINANCE_MAX => {
                                                    light.mastering_max =
                                                        nits(ebml_float(file, m.1, m.2)?)
                                                }
                                                LUMINANCE_MIN => {
                                                    light.mastering_min =
                                                        nits(ebml_float(file, m.1, m.2)?)
                                                }
                                                // The eight chromaticities: the
                                                // display's gamut, which this
                                                // engine cannot act on. Walked
                                                // past, never refused.
                                                _ => {}
                                            }
                                            at = m.2;
                                        }
                                    }
                                    _ => {}
                                }
                                at = c.2;
                            }
                            tags = Tags::from_codes(matrix, transfer, range);
                        }
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
    //
    // Which codec the id names is [`mkv_codec`]'s answer and not a match arm of
    // its own, so this cannot know a codec the mp4 side does not; the match below
    // is exhaustive over [`Codec`] for the other half of it -- a variant added to
    // the enum fails to compile here rather than falling through to a refusal.
    if kind == 2 {
        return Ok(MkvEntry::Audio(MkvAudioTrack {
            number,
            codec,
            // Whichever element the file states it in, and `und` for one that
            // states neither ([`mkv_language`]) -- the same answer the subtitle
            // walk gets, so a dual-audio remux and its subtitles agree.
            language: mkv_language(&language, &bcp47),
            name,
            unpack,
        }));
    }
    if kind != 1 {
        return Ok(MkvEntry::Other);
    }
    // The inner match carries no wildcard on purpose: a codec added to [`Codec`]
    // fails to compile here rather than falling through to a refusal, which is
    // how a VP9 `.webm` came to be refused by a build whose VA-API VP9 decoder
    // was wired.
    let Some(known) = mkv_codec(&codec) else {
        return Ok(MkvEntry::OtherVideo(codec));
    };
    // The record itself, before any of the arms below reads it apart.
    let private = config.clone();
    let (codec, nal_length, config, bit_depth) = match known {
        Codec::Av1 => {
            let (sets, bit_depth) = parse_av1c(&config)?;
            (Codec::Av1, 4, sets, bit_depth)
        }
        Codec::Hevc => {
            let (nal_length, sets, bit_depth) = parse_hvcc(&config)?;
            (Codec::Hevc, nal_length, sets, bit_depth)
        }
        // The `avcC` beside them, and the same story: length-prefixed blocks and
        // the SPS/PPS out of the record. Taken as 8-bit, which is what the whole
        // H.264 path here assumes of an mp4's `avc1` too.
        Codec::H264 => {
            let (nal_length, sets) = parse_avcc(&config)?;
            (Codec::H264, nal_length, sets, 8)
        }
        // A VP9 block is one self-contained (super)frame: no length prefixes to
        // strip and no configuration record to re-inject -- a `.webm` writes no
        // `CodecPrivate` at all, and the `vpcC` an mp4 writes is not bitstream
        // and would corrupt the frame if it were prepended. The depth is not in
        // the container either, so it is read off the first keyframe by
        // [`Demuxer::fill_vp9_depth`]; 8 here is what that probe starts from.
        Codec::Vp9 => (Codec::Vp9, 0, Vec::new(), 8),
    };
    Ok(MkvEntry::Video(MkvVideo {
        number,
        width,
        height,
        tags,
        light,
        default_duration,
        timestamp_scale,
        codec,
        config,
        private,
        nal_length,
        bit_depth,
        unpack,
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

/// Why an mp4 came back with no picture, by the fourcc of the video track it
/// does carry: `avc3` and `mp4v` are files with video in them, and telling their
/// owner there is none sends them looking for a fault that is not in the file.
/// [`UNSUPPORTED`]'s reason where the table has one, the fourcc itself where it
/// does not.
fn mp4_no_video<R>(path: &Path, reader: &Mp4Reader<R>) -> crate::Error {
    let found = reader
        .moov
        .traks
        .iter()
        .filter(|trak| {
            matches!(
                TrackType::try_from(&trak.mdia.hdlr.handler_type),
                Ok(TrackType::Video)
            )
        })
        .find_map(|trak| sample_entry(path, trak.tkhd.track_id).ok())
        .map(|(kind, _)| String::from_utf8_lossy(&kind).trim().to_string());
    match found {
        Some(kind) => match UNSUPPORTED.iter().find(|(id, _)| *id == kind) {
            Some((_, why)) => format!("{kind} video in this mp4 is not supported — {why}"),
            None => {
                format!("{kind} video in this mp4 is not supported — H.264, HEVC, VP9 and AV1 are")
            }
        },
        None => "no H.264, HEVC, VP9 or AV1 video track in file".to_string(),
    }
    .into()
}

/// Bits per luma sample out of a VP9 keyframe's uncompressed header (VP9
/// bitstream spec §6.2 `uncompressed_header` and §6.4.1 `color_config`), which is
/// the one place a VP9 stream states its own depth -- see
/// [`Demuxer::fill_vp9_depth`] for why neither container is asked.
///
/// `None` when this access unit cannot answer -- not a keyframe, a
/// `show_existing_frame`, a truncated block -- which leaves the caller's 8-bit
/// default standing rather than guessing; profile 0 and 1 *are* 8-bit by
/// definition, and they are all a `.webm` off the web ever is.
fn vp9_bit_depth(au: &[u8]) -> crate::Result<Option<u8>> {
    /// `count` bits, MSB first, from bit offset `at`. Up to 24 at a time, which
    /// is the frame sync code and the widest field read here.
    fn bits(au: &[u8], at: &mut usize, count: usize) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            let byte = *au.get(*at / 8)?;
            value = value << 1 | u32::from(byte >> (7 - *at % 8) & 1);
            *at += 1;
        }
        Some(value)
    }
    let at = &mut 0;
    // frame_marker, then the profile as two bits written low one first; profile 3
    // spends a reserved bit before the rest.
    if bits(au, at, 2) != Some(2) {
        return Ok(None);
    }
    let (Some(low), Some(high)) = (bits(au, at, 1), bits(au, at, 1)) else {
        return Ok(None);
    };
    let profile = high << 1 | low;
    if profile == 3 && bits(au, at, 1).is_none() {
        return Ok(None);
    }
    // show_existing_frame: such a frame is a reference already decoded and its
    // header stops right here. Then frame_type (0 is a keyframe), show_frame and
    // error_resilient_mode, and only a keyframe carries the colour config.
    if bits(au, at, 1) != Some(0) || bits(au, at, 1) != Some(0) {
        return Ok(None);
    }
    if bits(au, at, 2).is_none() {
        return Ok(None);
    }
    if bits(au, at, 24) != Some(0x49_8342) {
        return Ok(None);
    }
    if profile < 2 {
        return Ok(Some(8));
    }
    match bits(au, at, 1) {
        Some(0) => Ok(Some(10)),
        Some(_) => {
            Err("12-bit VP9 is not supported — 8- and 10-bit are what the decoder carries".into())
        }
        None => Ok(None),
    }
}

/// How many blocks past a cue's own the walk looks to count what the encoder
/// reordered around it ([`MkvDemuxer::anchor`]). Wider than any real stream
/// needs: H.264 and HEVC cap `max_num_reorder_frames` at 16, and what a muxer
/// actually writes is two or three.
///
/// corner-cut: a stream that reorders further than this would have its blocks
/// numbered a frame or two off. The upgrade path is the bitstream's own
/// `max_num_reorder_frames` out of the SPS, which this demuxer does not parse;
/// [`MkvDemuxer::cues_agree`] is what stands between that file and a wrong seek
/// in the meantime -- it degrades to the whole walk instead.
const REORDER: usize = 32;

/// One `CuePoint` of the picture track: the timestamp it names, which block of
/// the track that timestamp *is*, and where the `Cluster` holding it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cue {
    /// `CueTime`, in `TimestampScale` ticks -- the block's own timestamp, which
    /// is what picks it out of its cluster once that cluster is walked.
    time: i64,
    /// How many frames are *shown* before it: `CueTime` over the track's
    /// `DefaultDuration`. Matroska indexes time and everything above this
    /// demuxer counts frames, so this one division is what the lazy index rests
    /// on -- and it is a display count, which on a stream with B-frames is not
    /// the block's place in the file ([`MkvDemuxer::anchor`] takes the
    /// difference out). [`MkvDemuxer::cues_agree`] checks the whole arithmetic
    /// against the file's own blocks wherever the walk meets a cue.
    index: usize,
    /// Absolute file offset of that `Cluster`; `CueClusterPosition` states it
    /// relative to the start of the segment's payload.
    cluster: u64,
}

/// The picture track's `Cues` in index order, empty for a file carrying none
/// this can use -- Matroska's own seek index, which is what a player reads
/// instead of the film to know where its keyframes are.
///
/// Approach referenced from `matroska-demuxer 0.8.1` (hasenbanck), which
/// resolves a seek the same way: the `CueTrackPositions` of the wanted track,
/// its `CueClusterPosition` off the segment, then that cluster. The parsing is
/// this file's own EBML reader -- no dependency is added for four elements.
///
/// `ticks_per_frame` is the track's `DefaultDuration` in `TimestampScale`
/// ticks. Without it a cue time cannot become a frame number at all, which is
/// why the caller keeps the whole walk for a track that declares no duration.
fn mkv_cues(
    file: &mut File,
    segment: (u64, u64),
    number: u64,
    ticks_per_frame: f64,
) -> crate::Result<Vec<Cue>> {
    let Some((body, end)) = mkv_cues_element(file, segment)? else {
        return Ok(Vec::new());
    };
    // Read whole and parsed in memory, not element by element through the file:
    // a 2160p film's index is ten thousand cue points and a hundred thousand
    // EBML elements, and one `pread` apiece is 120 ms of open against 5 ms. The
    // ceiling is what keeps a crafted header from asking for all the memory
    // there is -- an hour of 24 fps cues to about 200 KB.
    const LIMIT: u64 = 64 << 20;
    if end.saturating_sub(body) > LIMIT {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; (end - body) as usize];
    read_exact_at(file, body, &mut buf)?;
    let mut cues = Vec::new();
    let mut at = 0;
    while let Some((id, body, stop)) = ebml_in(&buf, at) {
        at = stop;
        if id != CUE_POINT {
            continue;
        }
        let (mut time, mut cluster) = (None, None);
        let mut child = body;
        while let Some((id, body, stop)) = ebml_in(&buf[..stop], child) {
            child = stop;
            match id {
                CUE_TIME => time = Some(uint_in(&buf, body, stop) as i64),
                // One `CuePoint` carries a position per track -- a film's
                // picture and its sound are cued at the same instant -- so the
                // picture's is picked by `CueTrack` and the others skipped.
                // Taking them all is an index three times too long, two thirds
                // of it naming another track's clusters: his AV1 remux writes
                // 3928 `CueTrackPositions` for 1174 keyframes.
                CUE_TRACK_POSITIONS => {
                    let (mut track, mut pos) = (None, None);
                    let mut child = body;
                    while let Some((id, body, stop)) = ebml_in(&buf[..stop], child) {
                        child = stop;
                        match id {
                            CUE_TRACK => track = Some(uint_in(&buf, body, stop)),
                            CUE_CLUSTER_POSITION => pos = Some(uint_in(&buf, body, stop)),
                            _ => {}
                        }
                    }
                    if track == Some(number) {
                        cluster = cluster.or(pos);
                    }
                }
                _ => {}
            }
        }
        if let (Some(time), Some(cluster)) = (time, cluster) {
            cues.push(Cue {
                time,
                index: (time as f64 / ticks_per_frame).round().max(0.0) as usize,
                cluster: segment.0 + cluster,
            });
        }
    }
    // In index order whatever order the file wrote them in: the seek picks the
    // last cue at or before a frame, which is a statement about a sorted list.
    cues.sort_unstable_by_key(|c| c.index);
    Ok(cues)
}

/// Body range of the segment's `Cues` element: found among the header elements
/// where a muxer wrote it in front of the clusters (`ffmpeg -cues_to_front`),
/// and through the `SeekHead` where it wrote it at the far end, which is where
/// nearly every file has it. `None` for a file with neither -- a live capture,
/// a stream remuxed without an index -- which is the file that keeps the walk.
fn mkv_cues_element(file: &mut File, segment: (u64, u64)) -> crate::Result<Option<(u64, u64)>> {
    let mut seek = None;
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(file, at, segment.1)? {
        at = stop;
        match id {
            CUES => return Ok(Some((body, stop))),
            SEEK_HEAD => seek = seek.or(mkv_seek_position(file, body, stop, CUES)?),
            // Past here the file is clusters, which is what this exists not to
            // read.
            CLUSTER => break,
            _ => {}
        }
    }
    let Some(pos) = seek else {
        return Ok(None);
    };
    // The pointer is believed only as far as the element it lands on: a stale
    // `SeekHead` -- a file edited by a tool that moved the index and left the
    // table behind -- points at something that is not `Cues`, and reading that
    // as one is a seek into the middle of a frame.
    match ebml_element(file, segment.0 + pos, segment.1)? {
        Some((CUES, body, stop)) => Ok(Some((body, stop))),
        _ => Ok(None),
    }
}

/// Where a `SeekHead` says the element with id `want` is, relative to the start
/// of the segment's payload. Only the one level: a `SeekHead` pointing at
/// another `SeekHead` (what a muxer writes when it appends an index later) is
/// not followed, and that file falls back to the walk rather than to a guess.
fn mkv_seek_position(
    file: &mut File,
    body: u64,
    end: u64,
    want: u32,
) -> crate::Result<Option<u64>> {
    let mut at = body;
    while let Some((id, body, stop)) = ebml_element(file, at, end)? {
        at = stop;
        if id != SEEK {
            continue;
        }
        let (mut found, mut pos) = (None, None);
        let mut child = body;
        while let Some((id, body, stop)) = ebml_element(file, child, stop)? {
            child = stop;
            match id {
                // The id is written as the element's own bytes, marker bit and
                // all -- the same number this file's constants are.
                SEEK_ID => found = Some(ebml_uint(file, body, stop)?),
                SEEK_POSITION => pos = Some(ebml_uint(file, body, stop)?),
                _ => {}
            }
        }
        if found == Some(u64::from(want)) && pos.is_some() {
            return Ok(pos);
        }
    }
    Ok(None)
}

/// Every block of track `number`, in storage order, with the presentation span
/// (first and last timestamp in `TimestampScale` ticks) the frame-rate fallback
/// needs -- and, third, how many block bytes *every* track of the file spends,
/// `(TrackNumber, bytes)` in first-seen order.
///
/// That third answer is why this is one pass and not two: a block's length is
/// already parsed out of its header here, on the way to deciding it belongs to
/// another track and skipping it, so the sound track's size costs a lookup in a
/// list of two or three rather than a second walk of the segment (six seconds of
/// one, on a 12 GB film). [`probe_bitrate`] is what reads it.
///
/// Only element headers are read: a block's payload is seeked over and fetched
/// later by [`MkvDemuxer::next_access_unit`], which is what keeps this an index
/// pass rather than a read of the whole file.
/// The walk itself is [`mkv_walk_blocks`]; this is the sidecar cache in front of
/// it. What still reaches it is what the `Cues` cannot answer -- the sound
/// track, which Matroska indexes not at all, and a picture whose index is absent
/// or caught lying -- and those pay the whole walk on a file the user opens
/// again and again. So the walk's answer is written beside the user's other
/// caches and read back in milliseconds while the file it indexes is untouched.
fn mkv_blocks(path: &Path, file: &mut File, segment: (u64, u64), number: u64) -> crate::Result<Index> {
    let key = IndexKey::of(path, file, segment, number);
    let at = key.as_ref().and_then(|key| key.sidecar());
    if let Some((key, at)) = key.as_ref().zip(at.as_ref())
        && let Some(index) = read_index(key, at)
    {
        return Ok(index);
    }
    let index = mkv_walk_blocks(file, segment, number)?;
    if let Some((key, at)) = key.as_ref().zip(at.as_ref()) {
        // A cache that cannot be written is a slow open, not a failed one.
        let _ = write_index(key, at, &index);
    }
    Ok(index)
}

/// What one walk of the segment answers: the track's blocks, the span its
/// timestamps cover, and every track's byte count. Named because it is also what
/// the sidecar cache stores.
type Index = (Vec<Block>, Option<(i64, i64)>, Vec<(u64, u64)>);

fn mkv_walk_blocks(file: &mut File, segment: (u64, u64), number: u64) -> crate::Result<Index> {
    let mut blocks = Vec::new();
    // A file has a handful of tracks, so this is a shorter linear scan than a
    // hash would be a hash.
    let mut track_bytes: Vec<(u64, u64)> = Vec::new();
    // Where each laced block's frames landed, so their timestamps can be spread
    // once the block after is known -- see the fixup below.
    let mut laced: Vec<(usize, usize)> = Vec::new();
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(file, at, segment.1)? {
        at = stop;
        if id != CLUSTER {
            continue;
        }
        mkv_cluster(
            file,
            body,
            stop,
            number,
            &mut blocks,
            &mut track_bytes,
            &mut laced,
        )?;
    }
    mkv_spread_laces(&mut blocks, &laced);
    let span = blocks
        .iter()
        .map(|b| b.ts)
        .min()
        .zip(blocks.iter().map(|b| b.ts).max());
    Ok((blocks, span, track_bytes))
}

/// One `Cluster`'s blocks, appended: those of track `number` to `blocks`, and
/// every track's byte count to `track_bytes`. The laces met on the way are
/// recorded in `laced` for [`mkv_spread_laces`], which the caller runs once its
/// walk is over -- a lace's real span is measured against the block *after* it,
/// which the next cluster may hold.
///
/// Split out of [`mkv_blocks`] so the lazy index ([`MkvDemuxer::walk_cluster`])
/// walks a cluster by exactly the code the whole-segment walk does: two readers
/// of one container that disagreed about what a block is would be two different
/// files to seek in.
fn mkv_cluster(
    file: &mut File,
    body: u64,
    end: u64,
    number: u64,
    blocks: &mut Vec<Block>,
    track_bytes: &mut Vec<(u64, u64)>,
    laced: &mut Vec<(usize, usize)>,
) -> crate::Result<()> {
    let mut cluster_ts = 0i64;
    let mut child = body;
    while let Some((id, body, stop)) = ebml_element(file, child, end)? {
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
        // Before the track filter, so this counts the whole file: the block
        // as it sits on disk, lace header included, which is what that track
        // costs the container. No read is added -- `len` came out of the
        // header that was parsed to get here.
        match track_bytes.iter_mut().find(|(n, _)| *n == block.number) {
            Some((_, bytes)) => *bytes += block.len as u64,
            None => track_bytes.push((block.number, block.len as u64)),
        }
        if block.number != number {
            continue;
        }
        let ts = cluster_ts + i64::from(block.rel);
        if block.flags & 0x06 == 0 {
            blocks.push(Block {
                at: block.at,
                len: block.len,
                key,
                ts,
            });
            continue;
        }
        // A laced block is several frames behind a header of sizes: one
        // `Block` each, and only the first of them is a point a decoder can
        // be started from -- the rest are inside the same lace.
        let start = blocks.len();
        for (i, (at, len)) in mkv_lace(file, &block)?.into_iter().enumerate() {
            blocks.push(Block {
                at,
                len,
                key: key && i == 0,
                ts,
            });
        }
        laced.push((start, blocks.len() - start));
    }
    Ok(())
}

/// The lace fixup: a laced block writes one timestamp for all its frames, and
/// stacking six E-AC-3 frames on one instant is a sound track that drifts a
/// fifth of a second away from the picture inside a cluster. The gap to the
/// next block of the same track is what the lace really spans, so the frames
/// are spread across it; the last lace of a walk has no block after it and
/// keeps the step the one before it measured.
fn mkv_spread_laces(blocks: &mut [Block], laced: &[(usize, usize)]) {
    let mut step = 0.0;
    for &(start, count) in laced {
        let first = blocks[start].ts;
        if let Some(next) = blocks
            .get(start + count)
            .map(|b| b.ts)
            .filter(|&next| next > first)
        {
            step = (next - first) as f64 / count as f64;
        }
        for i in 1..count {
            blocks[start + i].ts = first + (i as f64 * step).round() as i64;
        }
    }
}

/// Magic and format version of the sidecar index. The last byte is the version:
/// bump it and every file an older build wrote is a miss, which is the whole
/// migration story -- a sidecar is rewritten, never upgraded.
///
/// **Bump it whenever the walk's output changes.** The key below binds a sidecar
/// to the *file* -- its bytes, its times, its track -- and to nothing about the
/// code that wrote it, so a build whose blocks, timestamps or lace spreading
/// differ from the one that filled this directory will believe every stale
/// record in it. This byte is the only thing standing there, and it is bumped by
/// hand.
const INDEX_MAGIC: &[u8; 8] = b"EDMKVIX2";

/// What a sidecar index is valid for and nothing else: the file it was walked
/// from, down to the byte and the modification time, and the track it indexes.
/// Every field is written into the sidecar and compared on the way back, so a
/// re-encode, an append, a truncation or a hash collision on the file name are
/// all one thing -- a miss, and the walk runs.
struct IndexKey {
    /// Canonical, so a symlink and its target share one entry rather than
    /// walking the same bytes twice under two names.
    path: PathBuf,
    len: u64,
    /// Seconds since the epoch and nanoseconds within it; the seconds are signed
    /// because a file can be stamped before 1970 and that is not an error.
    mtime: (i64, u32),
    segment: (u64, u64),
    number: u64,
}

impl IndexKey {
    /// `None` when the file cannot be stat'd or the platform has no modification
    /// time, which is a file that gets the walk every time rather than a cache
    /// nothing can invalidate.
    fn of(path: &Path, file: &File, segment: (u64, u64), number: u64) -> Option<Self> {
        let meta = file.metadata().ok()?;
        let mtime = match meta.modified().ok()?.duration_since(UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
            Err(e) => (
                -(e.duration().as_secs() as i64),
                e.duration().subsec_nanos(),
            ),
        };
        Some(Self {
            path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            len: meta.len(),
            mtime,
            segment,
            number,
        })
    }

    /// Where this key's sidecar lives. The name is the path's hash and the track
    /// number, *not* the size or the time: an edited file overwrites its own
    /// entry instead of leaving the old one behind, which is what keeps the
    /// directory one file per track of every Matroska file ever opened.
    ///
    /// corner-cut: nothing evicts. That is bounded (a few MB for a feature film's
    /// picture track) and the directory is the user's own cache to delete, but a
    /// size-capped LRU sweep at open is the upgrade path if it ever matters.
    fn sidecar(&self) -> Option<PathBuf> {
        let dir = index_dir(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))?;
        Some(dir.join(format!(
            "{:016x}-{}.idx",
            fnv1a(self.path.as_os_str().as_encoded_bytes()),
            self.number
        )))
    }
}

/// The path rule, with the environment handed in so it can be checked, as
/// `keymap::config_path_in` is on the config side. An empty `XDG_CACHE_HOME` is
/// one the spec says to ignore; with no `HOME` either there is nowhere to put a
/// cache, and `None` -- always walk -- beats littering the working directory.
fn index_dir(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    xdg.filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .map(|dir| dir.join("edith").join("mkvindex"))
}

/// FNV-1a, 64-bit: what names a sidecar and what checks it. Not a cryptographic
/// hash and not asked to be -- nothing here is adversarial. It is the guard that
/// turns a half-written or scrambled file into a miss; a *name* collision is
/// caught by the key inside the file, which is compared field by field.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A cursor over an encoded index. Every read is bounds-checked and hands back
/// `None` past the end, so a truncated file runs out of bytes rather than off
/// the end of one, and one `?` per field is the whole validation.
struct Take<'a>(&'a [u8]);

impl<'a> Take<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }
}

/// The index as bytes: the key first, then the walk's three answers. Little
/// endian throughout and hand-rolled, because this tree carries no serializer
/// and a flat record of fixed-width fields does not need one.
fn encode_index(key: &IndexKey, index: &Index) -> Vec<u8> {
    let (blocks, span, track_bytes) = index;
    let path = key.path.as_os_str().as_encoded_bytes();
    let mut out = Vec::with_capacity(80 + path.len() + blocks.len() * 25 + track_bytes.len() * 16);
    out.extend_from_slice(&key.len.to_le_bytes());
    out.extend_from_slice(&key.mtime.0.to_le_bytes());
    out.extend_from_slice(&key.mtime.1.to_le_bytes());
    out.extend_from_slice(&key.segment.0.to_le_bytes());
    out.extend_from_slice(&key.segment.1.to_le_bytes());
    out.extend_from_slice(&key.number.to_le_bytes());
    out.extend_from_slice(&(path.len() as u64).to_le_bytes());
    out.extend_from_slice(path);
    // A `None` span is written at the same width as a present one, so the record
    // stays a table of fixed-width fields.
    out.push(u8::from(span.is_some()));
    let (first, last) = span.unwrap_or((0, 0));
    out.extend_from_slice(&first.to_le_bytes());
    out.extend_from_slice(&last.to_le_bytes());
    out.extend_from_slice(&(track_bytes.len() as u64).to_le_bytes());
    for (number, bytes) in track_bytes {
        out.extend_from_slice(&number.to_le_bytes());
        out.extend_from_slice(&bytes.to_le_bytes());
    }
    out.extend_from_slice(&(blocks.len() as u64).to_le_bytes());
    for block in blocks {
        out.extend_from_slice(&block.at.to_le_bytes());
        out.extend_from_slice(&(block.len as u64).to_le_bytes());
        out.extend_from_slice(&block.ts.to_le_bytes());
        out.push(u8::from(block.key));
    }
    out
}

/// The sidecar at `at`, if it is this key's and intact. `None` for every other
/// case there is -- absent, unreadable, another format version, a checksum that
/// does not match, a key that does not, a record that ends early or one with
/// bytes left over -- and every one of them means the same thing to the caller:
/// walk the file and write this again.
fn read_index(key: &IndexKey, at: &Path) -> Option<Index> {
    let bytes = std::fs::read(at).ok()?;
    let (head, body) = bytes.split_at_checked(16)?;
    if &head[..8] != INDEX_MAGIC || u64::from_le_bytes(head[8..].try_into().ok()?) != fnv1a(body) {
        return None;
    }
    let mut take = Take(body);
    if take.u64()? != key.len
        || take.i64()? != key.mtime.0
        || take.u32()? != key.mtime.1
        || take.u64()? != key.segment.0
        || take.u64()? != key.segment.1
        || take.u64()? != key.number
    {
        return None;
    }
    let path_len = usize::try_from(take.u64()?).ok()?;
    if take.bytes(path_len)? != key.path.as_os_str().as_encoded_bytes() {
        return None;
    }
    let present = take.bytes(1)?[0] != 0;
    let (first, last) = (take.i64()?, take.i64()?);
    let span = present.then_some((first, last));
    let count = usize::try_from(take.u64()?).ok()?;
    // Capacity is what the bytes left can actually hold, never what the record
    // claims: the checksum has already passed, but a count is not a promise.
    let mut track_bytes = Vec::with_capacity(count.min(take.0.len() / 16));
    for _ in 0..count {
        track_bytes.push((take.u64()?, take.u64()?));
    }
    let count = usize::try_from(take.u64()?).ok()?;
    let mut blocks = Vec::with_capacity(count.min(take.0.len() / 25));
    for _ in 0..count {
        blocks.push(Block {
            at: take.u64()?,
            len: usize::try_from(take.u64()?).ok()?,
            ts: take.i64()?,
            key: take.bytes(1)?[0] != 0,
        });
    }
    take.0.is_empty().then_some((blocks, span, track_bytes))
}

/// Writes the sidecar, atomically: a temporary file of its own, fsynced, then
/// renamed over the name readers look under. A reader therefore sees the whole
/// index or no index -- never a half-written one, which the checksum would catch
/// anyway but only after the bytes were read.
fn write_index(key: &IndexKey, at: &Path, index: &Index) -> std::io::Result<()> {
    let dir = at
        .parent()
        .ok_or_else(|| std::io::Error::other("a sidecar path with no directory"))?;
    std::fs::create_dir_all(dir)?;
    let body = encode_index(key, index);
    // Per process *and* per call: the picture and the sound of one file are
    // opened by different threads, and two writers must not share a part file.
    static NTH: AtomicU64 = AtomicU64::new(0);
    let part = dir.join(format!(
        "{}.{}.part",
        std::process::id(),
        NTH.fetch_add(1, Ordering::Relaxed)
    ));
    let result = File::create(&part)
        .and_then(|mut f| {
            f.write_all(INDEX_MAGIC)?;
            f.write_all(&fnv1a(&body).to_le_bytes())?;
            f.write_all(&body)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&part, at));
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}


/// The frames inside a laced block: one `(offset, length)` each, in order.
///
/// Lacing packs several frames -- always whole ones, and only ones nothing else
/// references -- into a single block behind a header of sizes, in one of the
/// three shapes the spec defines. Video muxers write none, but a streaming
/// service's audio is laced by the thousand: every E-AC-3 block of a WEB remux
/// is fixed-laced, and a reader that refuses those is a film that plays silent.
fn mkv_lace(file: &mut File, block: &BlockHeader) -> crate::Result<Vec<(u64, usize)>> {
    // The frame count is one byte and there are at most 256 frames, each size
    // at most 8 bytes of vint (or a run of 0xFF bytes in Xiph, which is bounded
    // by the block itself): the header cannot be longer than this.
    let head_len = (1 + 256 * 9).min(block.len);
    let mut head = vec![0u8; head_len];
    read_exact_at(file, block.at, &mut head)?;
    let count = usize::from(
        *head
            .first()
            .ok_or("a laced Matroska block with no frames")?,
    ) + 1;
    let short = || crate::Error::from("a Matroska lace header past the end of its block");
    // The cursor into the lace header, which is where the frames start once the
    // sizes have been read.
    let mut at = 1usize;
    let mut sizes = Vec::with_capacity(count);
    match (block.flags >> 1) & 0x03 {
        // Xiph: every size but the last as a run of 255s and a remainder.
        1 => {
            for _ in 1..count {
                let mut size = 0usize;
                loop {
                    let &b = head.get(at).ok_or_else(short)?;
                    at += 1;
                    size += usize::from(b);
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        // Fixed: no sizes written at all, the frames divide the rest evenly.
        2 => {
            let rest = block.len - at;
            if rest % count != 0 {
                return Err("a fixed-lace Matroska block that does not divide evenly".into());
            }
            sizes = vec![rest / count; count - 1];
        }
        // EBML: the first size outright, the rest as differences from the one
        // before -- signed vints, which is the same encoding with the middle of
        // its range taken as zero.
        3 => {
            let mut size = 0i64;
            for i in 1..count {
                let (raw, len) = ebml_vint(head.get(at..).ok_or_else(short)?, true)?;
                at += len;
                size = match i {
                    1 => raw as i64,
                    _ => size + raw as i64 - ((1i64 << (7 * len - 1)) - 1),
                };
                sizes.push(usize::try_from(size).map_err(|_| "a negative Matroska lace size")?);
            }
        }
        // The caller reads the same two bits and only comes here for a lace.
        _ => unreachable!("an unlaced block does not reach the lace reader"),
    }
    let end = block.at + block.len as u64;
    let mut frames = Vec::with_capacity(count);
    let mut off = block.at + at as u64;
    for size in sizes {
        let stop = off
            .checked_add(size as u64)
            .filter(|&s| s <= end)
            .ok_or("a Matroska lace whose frame sizes run past the end of its block")?;
        frames.push((off, size));
        off = stop;
    }
    // The last frame is whatever is left, which is the only length the fixed and
    // Xiph headers write down for it.
    frames.push((off, (end - off) as usize));
    Ok(frames)
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
/// The EBML element at `at` inside a slice already in memory: its id, where its
/// payload starts and where the element ends. `None` at the end of the slice,
/// and for a header claiming more bytes than the slice holds -- which is how a
/// caller bounds a child to its parent, by handing over the parent's bytes.
///
/// The file-backed [`ebml_element`] costs a `pread` an element, which is right
/// for the handful a header is and wrong for the hundred thousand a film's
/// `Cues` are. Same shapes, same unknown-length rule left out: a `Cues` element
/// of unknown length is one nobody has finished writing.
fn ebml_in(buf: &[u8], at: usize) -> Option<(u32, usize, usize)> {
    let head = buf.get(at..).filter(|h| !h.is_empty())?;
    let (id, id_len) = ebml_vint(head, false).ok()?;
    let (size, size_len) = ebml_vint(head.get(id_len..)?, true).ok()?;
    let body = at + id_len + size_len;
    let stop = body.checked_add(usize::try_from(size).ok()?)?;
    (stop <= buf.len()).then_some((u32::try_from(id).ok()?, body, stop))
}

/// An unsigned EBML integer out of a slice, big-endian as they all are.
fn uint_in(buf: &[u8], body: usize, stop: usize) -> u64 {
    buf[body..stop]
        .iter()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

fn ebml_uint(file: &mut File, body: u64, stop: u64) -> crate::Result<u64> {
    Ok(ebml_bytes(file, body, stop)?
        .iter()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b)))
}

/// An EBML float element: IEEE big-endian, 4 or 8 bytes wide (a Matroska muxer
/// writes the luminances as doubles). Zero for any other width, which is what an
/// absent one is by spec and what [`nits`] then reads as "not stated".
fn ebml_float(file: &mut File, body: u64, stop: u64) -> crate::Result<f64> {
    let bytes = ebml_bytes(file, body, stop)?;
    Ok(match bytes.len() {
        4 => f64::from(f32::from_be_bytes(bytes[..4].try_into().unwrap())),
        8 => f64::from_be_bytes(bytes[..8].try_into().unwrap()),
        _ => 0.0,
    })
}

/// A brightness a container stated, or [`None`] when it stated zero -- which is
/// what a muxer writes for "unknown" and what a tone map must not read as a film
/// that peaks at black.
fn nits(value: f64) -> Option<f32> {
    (value > 0.0).then_some(value as f32)
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
/// corner-cut: empty edits are ignored and `media_time` is otherwise taken at face
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

/// Every sync point of a track paired with the *display* index it is really at:
/// `(display index, decode position)`, ascending. `times` holds each access
/// unit's presentation time in **decode** order -- Matroska block timestamps,
/// mp4 `stts + ctts` composition times -- and `sync` says which of them a
/// decoder may be started on.
///
/// A picture's index everywhere above this tier is its rank in *presentation*
/// order, and a stream that codes pictures out of order does not put its sync
/// points at the same rank in the two: an **open-GOP** HEVC stream (x265's
/// default, so every film off a disc or a web rip) writes the RASL leading
/// pictures of a GOP *after* the CRA that opens it in decode order and *before*
/// it on screen, so the keyframe stored 28th is the 30th picture shown. Seeking
/// by its stored position and then counting frames from there lands two
/// pictures late -- and by however many leading pictures that GOP happens to
/// carry elsewhere, which is why no constant can stand in for this.
///
/// The sort is stable, so blocks sharing a timestamp keep decode order.
fn sync_display_order(times: &[i64], sync: impl Fn(usize) -> bool) -> Vec<(u32, u32)> {
    let mut order: Vec<u32> = (0..times.len() as u32).collect();
    order.sort_by_key(|&i| times[i as usize]);
    order
        .into_iter()
        .enumerate()
        .filter(|&(_, decode)| sync(decode as usize))
        .map(|(display, decode)| (display as u32, decode))
        .collect()
}

/// Composition (presentation) time of every sample of an mp4 track, in decode
/// order and media ticks: the `stts` decode times with the `ctts` offsets added
/// on, which is the pair that says a B-frame is shown before the picture stored
/// ahead of it. Both tables arrive as their `(sample_count, value)` runs, the
/// shape [`stts_pairs`] already hands the `stts` over in; the `ctts` runs may
/// cover fewer samples than exist, in which case the rest carry no offset.
fn composition_times(
    stts: impl IntoIterator<Item = (u32, u32)>,
    ctts: impl IntoIterator<Item = (u32, i32)>,
) -> Vec<i64> {
    let mut times = Vec::new();
    let mut decode = 0i64;
    for (count, delta) in stts {
        for _ in 0..count {
            times.push(decode);
            decode += i64::from(delta);
        }
    }
    let mut sample = 0usize;
    for (count, offset) in ctts {
        for _ in 0..count {
            match times.get_mut(sample) {
                Some(t) => *t += i64::from(offset),
                None => return times,
            }
            sample += 1;
        }
    }
    times
}

/// Rank of the sample at 0-based decode index `sample` in presentation order,
/// tie-broken by decode order exactly as [`sync_display_order`]'s stable sort
/// is. Used for frame 0, which an edit list can put at any sample.
fn display_index(times: &[i64], sample: u32) -> u32 {
    let Some(&at) = times.get(sample as usize) else {
        return sample;
    };
    times
        .iter()
        .enumerate()
        .filter(|&(i, &t)| (t, i as u32) < (at, sample))
        .count() as u32
}

/// The latest sync point at or before display index `frame`, out of a
/// [`sync_display_order`] table. `None` when the track's first sync point is
/// already past `frame`, i.e. there is nothing decodable earlier.
fn sync_display_at_or_before(syncs: &[(u32, u32)], frame: u32) -> Option<(u32, u32)> {
    match syncs.partition_point(|&(display, _)| display <= frame) {
        0 => None,
        i => Some(syncs[i - 1]),
    }
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

/// The `colr` box of `track_id`'s sample entry (ISO 14496-12 §12.1.5), which is
/// where an mp4 says what its picture's numbers mean. `nclx` is what every
/// muxer writes now and carries a range flag; `nclc`, QuickTime's older twin,
/// is the same three code points with no range byte after them. Anything else
/// (`rICC`, `prof`: ICC profiles) is not a code-point description at all and is
/// left to the tier below.
///
/// [`None`] rather than an error for a file with no such box, which is most of
/// them -- an untagged mp4 is not a broken one.
fn colr_tags(path: &Path, track_id: u32) -> Option<Tags> {
    let (_, entry) = sample_entry(path, track_id).ok()?;
    // The same fixed 78-byte VisualSampleEntry header `hvcC` sits behind.
    let colr = child(entry.get(78..).unwrap_or_default(), b"colr")?;
    let kind: &[u8; 4] = colr.get(..4)?.try_into().ok()?;
    if kind != b"nclx" && kind != b"nclc" {
        return None;
    }
    let code = |at: usize| -> Option<u64> {
        Some(u64::from(u16::from_be_bytes(
            colr.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    // Matroska's Range codes are 1 limited / 2 full; a `colr` flag is a bit.
    let range = match kind {
        b"nclx" => colr.get(10).map_or(0, |b| 1 + u64::from(b >> 7)),
        _ => 0,
    };
    Some(Tags::from_codes(code(8)?, code(6)?, range))
}

/// The `clli` and `mdcv` boxes of `track_id`'s sample entry: what an mp4 says
/// about how bright its pictures get, beside the `colr` that says what their
/// numbers mean. Neither is a FullBox -- their payloads are the HEVC SEI
/// messages of the same names, byte for byte, and are read by the same pair of
/// readers ([`crate::colorspace::clli`], [`crate::colorspace::mdcv`]).
///
/// Empty rather than an error for the files that carry neither, which is every
/// SDR mp4 there is.
fn mp4_light(path: &Path, track_id: u32) -> ContentLight {
    let Ok((_, entry)) = sample_entry(path, track_id) else {
        return ContentLight::default();
    };
    // Behind the same fixed 78-byte VisualSampleEntry header `colr` sits behind.
    let entry = entry.get(78..).unwrap_or_default();
    let levels = child(entry, b"clli").map(crate::colorspace::clli);
    let mastering = child(entry, b"mdcv").map(crate::colorspace::mdcv);
    levels
        .unwrap_or_default()
        .over(mastering.unwrap_or_default())
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
    /// Every cue block, in storage order, for the codecs [`crate::subtitle`]
    /// reads: the `S_TEXT/*` ones and `S_HDMV/PGS`, whose blocks are display
    /// sets of run-length pictures rather than lines. A codec neither of those
    /// -- VobSub off a DVD -- comes back declared and empty.
    pub cues: Vec<MkvCue>,
    /// Why this track's blocks were not read at all: a `ContentEncodings` this
    /// cannot undo. Listed like a bitmap track rather than raised, so a film
    /// with one encrypted subtitle track still opens with the others.
    pub unsupported: Option<String>,
    /// What every block of this track goes back through; see [`Unpack`].
    unpack: Unpack,
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

/// The Matroska codec id of Blu-ray bitmap subtitles: run-length pictures in
/// display sets, read by [`crate::subtitle`] as [`MkvCue`]s like any other.
pub const PGS: &str = "S_HDMV/PGS";

/// The subtitle tracks of a Matroska file, in file order. An mp4's are a
/// different beast and are read by [`mp4_subtitles`]; anything that is not a
/// Matroska file at all is an error, as it is for [`matroska_audio_codec`].
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
    // The tracks something reads: see `MkvSubtitle::cues`.
    let wanted: Vec<u64> = tracks
        .iter()
        .filter(|t| (t.codec.starts_with("S_TEXT") || t.codec == PGS) && t.unsupported.is_none())
        .map(|t| t.number)
        .collect();
    if !wanted.is_empty() {
        mkv_subtitle_blocks(&mut file, segment, timestamp_scale, &wanted, &mut tracks)?;
    }
    Ok(tracks)
}

/// One timed-text subtitle track of an mp4 -- the `tx3g` sample entry ffmpeg
/// calls `mov_text` -- exactly as its `trak` declares it. The mp4 half of
/// [`MkvSubtitle`]: what the samples *mean* is [`crate::subtitle`]'s business,
/// this is the walk.
#[derive(Debug)]
pub struct Mp4Subtitle {
    /// The `tkhd` track id, which is what a `.edith` row names the track by.
    pub number: u64,
    /// The `mdhd` language, ISO-639-2. `und` where the file states none, which
    /// is the only thing that field can say -- an mp4 packs three letters into
    /// sixteen bits and cannot leave them out.
    pub language: String,
    /// `trak/udta/name`, the title a muxer wrote (`Signs`), empty for the far
    /// more common track that has none. The box ffmpeg reads and writes a
    /// `-metadata:s:s:0 title=` in, and the one [`crate::mux`] patches in.
    pub name: String,
    /// Every sample of the track, in storage order.
    pub samples: Vec<Mp4TextSample>,
}

/// One timed-text sample: when it shows, when it stops, and the bytes.
#[derive(Debug)]
pub struct Mp4TextSample {
    /// Microseconds from the start of the file -- the sample's decode time
    /// scaled by the track's own `mdhd` timescale, which is milliseconds in
    /// what this project writes and microseconds in what ffmpeg writes.
    pub start_us: i64,
    /// Where the next sample begins: a timed-text track states one sample per
    /// instant and leaves no holes, so this is the sample's `stts` duration on
    /// top of its start and not a guess ([`crate::subtitle`]'s Matroska side
    /// has to guess -- see `end_of` there).
    pub end_us: i64,
    /// ISO/IEC 14496-17: the text's length in 16 bits, then the UTF-8 itself.
    /// A payload of `00 00` is a sample of *no text*, which is the stretch
    /// between two cues rather than a cue with nothing in it.
    pub payload: Vec<u8>,
}

/// The timed-text subtitle tracks of an mp4, in track-id order -- the reader
/// that makes an mp4 export of this project's own openable again, and reads a
/// `mov_text` track ffmpeg muxed just the same.
///
/// The samples come out of the `mp4` crate, which walks `stts`/`stsz`/`stco`
/// for any track type; the *title* does not, because that crate's `TrakBox` has
/// no `udta` field at all -- so the one box carrying it is read by hand out of
/// the `moov`, the way [`sample_entry`] reads an `stsd` entry the crate drops.
///
/// A file with no timed-text track comes back empty, never an error; a file that
/// is not an mp4 at all is an error, as [`matroska_subtitles`] is for a
/// non-Matroska.
pub fn mp4_subtitles(path: &Path) -> crate::Result<Vec<Mp4Subtitle>> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut reader = Mp4Reader::read_header(BufReader::new(file), size)?;
    let mut ids: Vec<u32> = reader
        .tracks()
        .iter()
        .filter(|(_, track)| matches!(track.track_type(), Ok(TrackType::Subtitle)))
        .map(|(id, _)| *id)
        .collect();
    // By track id, which is the order the tracks were written in: a `HashMap`
    // hands them over in whatever order it likes, and a row picks a track by
    // its number out of a list a user saw.
    ids.sort_unstable();
    let names = mp4_track_names(path)?;
    let mut tracks = Vec::with_capacity(ids.len());
    for id in ids {
        let track = &reader.tracks()[&id];
        // Off the borrow before the samples, which need the reader itself.
        let language = track.language().to_owned();
        let timescale = i128::from(track.timescale().max(1));
        let count = track.sample_count();
        let mut samples = Vec::with_capacity(count as usize);
        for i in 1..=count {
            // Sample ids count from 1. A sample the tables cannot reach is
            // skipped rather than raised: the other cues of the track are still
            // the words a viewer needs.
            let Some(sample) = reader.read_sample(id, i)? else {
                continue;
            };
            let us = |ticks: u64| (i128::from(ticks) * 1_000_000 / timescale) as i64;
            samples.push(Mp4TextSample {
                start_us: us(sample.start_time),
                end_us: us(sample.start_time + u64::from(sample.duration)),
                payload: sample.bytes.to_vec(),
            });
        }
        tracks.push(Mp4Subtitle {
            number: u64::from(id),
            language,
            name: names
                .iter()
                .find(|(track_id, _)| *track_id == id)
                .map(|(_, name)| name.clone())
                .unwrap_or_default(),
            samples,
        });
    }
    Ok(tracks)
}

/// Each `trak`'s `udta/name`, by track id, for the traks that have one -- an
/// mp4's word for what a track is *called*, beside the `mdhd` language it is
/// in. Read by hand for the reason [`mp4_subtitles`] gives.
fn mp4_track_names(path: &Path) -> crate::Result<Vec<(u32, String)>> {
    let Some(moov) = read_top_level(path, b"moov")? else {
        return Ok(Vec::new());
    };
    Ok(boxes(&moov)
        .filter(|(kind, _)| *kind == b"trak")
        .filter_map(|(_, trak)| {
            let id = child(trak, b"tkhd").and_then(tkhd_track_id)?;
            let name = child(trak, b"udta").and_then(|udta| child(udta, b"name"))?;
            // Raw bytes, no version or flags -- and a writer that terminates the
            // string is not lying about the title, it is padding it.
            let name = String::from_utf8_lossy(name);
            Some((id, name.trim_end_matches('\0').to_owned()))
        })
        .collect())
}

/// One `TrackEntry`, `Some` only for track type 0x11 -- the subtitles.
fn mkv_subtitle_entry(file: &mut File, body: u64, end: u64) -> crate::Result<Option<MkvSubtitle>> {
    const SUBTITLE: u64 = 0x11;
    let (mut number, mut kind, mut codec) = (0, 0, String::new());
    let (mut language, mut name, mut private) = (String::new(), String::new(), Vec::new());
    let mut bcp47 = String::new();
    let mut unpack = Unpack::None;
    let mut at = body;
    while let Some((id, body, stop)) = ebml_element(file, at, end)? {
        match id {
            TRACK_NUMBER => number = ebml_uint(file, body, stop)?,
            CONTENT_ENCODINGS => unpack = mkv_content_encoding(file, body, stop)?,
            TRACK_TYPE => kind = ebml_uint(file, body, stop)?,
            CODEC_ID => codec = string_of(file, body, stop)?,
            CODEC_PRIVATE => private = ebml_bytes(file, body, stop)?,
            TRACK_LANGUAGE => language = string_of(file, body, stop)?,
            TRACK_LANGUAGE_BCP47 => bcp47 = string_of(file, body, stop)?,
            TRACK_NAME => name = string_of(file, body, stop)?,
            _ => {}
        }
        at = stop;
    }
    Ok((kind == SUBTITLE).then(|| MkvSubtitle {
        number,
        codec,
        // Whichever element the file states it in, `und` for a track that states
        // neither -- [`mkv_language`] says why that is not the spec's `eng`.
        language: mkv_language(&language, &bcp47),
        name,
        private,
        cues: Vec::new(),
        unsupported: match &unpack {
            Unpack::Refused(why) => Some(why.clone()),
            _ => None,
        },
        unpack,
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
            let Some(track) = tracks.iter_mut().find(|t| t.number == block.number) else {
                continue;
            };
            // A laced subtitle block is several cues written at one instant. No
            // muxer writes one, but the header is the same one the sound track's
            // blocks carry ([`mkv_lace`]), so reading it costs nothing here.
            let frames = match block.flags & 0x06 {
                0 => vec![(block.at, block.len)],
                _ => mkv_lace(file, &block)?,
            };
            for (at, len) in frames {
                // A megabyte in one cue is a corrupt file, not a subtitle -- a
                // line of dialogue is bytes and a PGS display set of run-length
                // pictures is tens of kilobytes -- and a crafted length may not
                // reach an allocation through here.
                if len > 1 << 20 {
                    return Err("a Matroska subtitle block larger than a megabyte".into());
                }
                let mut payload = vec![0u8; len];
                read_exact_at(file, at, &mut payload)?;
                track.unpack.frame(&mut payload)?;
                track.cues.push(MkvCue {
                    start_us: us(cluster_ts + i64::from(block.rel)),
                    duration_us: duration.map(us),
                    payload,
                });
            }
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

    fn asset(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    /// Every Matroska fixture in `assets`, whichever of them holds a picture
    /// this reads: the audio-only and subtitle-only ones refuse to open and are
    /// not the subject here, so they are skipped by name of their own refusal.
    fn matroska_fixtures() -> Vec<std::path::PathBuf> {
        let mut found: Vec<_> = std::fs::read_dir(asset(""))
            .expect("the assets directory -- run scripts/gen_fixtures.sh")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| is_matroska(p))
            .collect();
        found.sort();
        found
    }

    /// **The lazy index is the same index.** For every Matroska fixture, the
    /// window built off the file's `Cues` and the whole-segment walk it replaced
    /// agree on: the block list, the frame count, and where every one of the
    /// file's frames seeks to.
    ///
    /// This is the claim the `Cues` path rests on -- a seek index read out of
    /// the container is only worth having while it lands on the block the walk
    /// would have landed on -- and it is asserted per frame rather than sampled.
    #[test]
    fn the_cue_index_and_the_whole_walk_are_the_same_index() {
        let (mut cued, mut compared) = (0, 0);
        for path in matroska_fixtures() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let Ok((meta, mut lazy)) = MkvDemuxer::open(&path) else {
                continue;
            };
            compared += 1;
            cued += usize::from(!lazy.complete);
            // The walk, built beside it out of the same file.
            let mut file = File::open(&path).expect(&name);
            let (walk, _, _) = mkv_walk_blocks(&mut file, lazy.segment, lazy.number).expect(&name);

            assert_eq!(
                meta.frame_count as usize,
                walk.len(),
                "{name}: the frame count off the cues is not the walk's"
            );
            // The window grown to the whole file is the whole walk, block for
            // block -- offsets, lengths, keyframe flags and timestamps.
            let mut whole = MkvDemuxer::open(&path).expect(&name).1;
            whole.extend_to(usize::MAX).expect(&name);
            assert_eq!(whole.base, 0, "{name}: a sequential read moved the base");
            assert_eq!(whole.blocks, walk, "{name}: the lazy walk is not the walk");
            // ...and every seek lands where the walk's own rule says, jumping
            // backwards and forwards through the cues to get there. The rule is
            // the walk's *display* order ([`sync_display_order`]): which keyframe
            // is "at or before" a frame is a question about the screen, and the
            // window has to answer it out of ten seconds of the file.
            let syncs = {
                let times: Vec<i64> = walk.iter().map(|b| b.ts).collect();
                sync_display_order(&times, |i| walk[i].key)
            };
            for frame in (0..walk.len()).chain((0..walk.len()).rev()) {
                let landed = lazy.seek_to_sync_at_or_before(frame as u32);
                let (display, want) =
                    sync_display_at_or_before(&syncs, frame as u32).unwrap_or((0, 0));
                assert_eq!(
                    landed,
                    i64::from(display),
                    "{name}: seek to frame {frame} named a picture the walk does not"
                );
                assert_eq!(
                    lazy.blocks[lazy.next - lazy.base],
                    walk[want as usize],
                    "{name}: the block frame {frame} lands on"
                );
            }
        }
        assert!(compared >= 12, "only {compared} Matroska fixtures opened");
        assert!(
            cued * 2 > compared,
            "only {cued} of {compared} fixtures took the cue path -- the test is \
             asserting the fallback against itself"
        );
    }

    /// A file with no `Cues` at all -- legal, and what a live capture is -- is
    /// the whole walk it always was, and answers every seek the same way.
    ///
    /// Made by hand rather than by a muxer: neither `ffmpeg` nor `mkvmerge`
    /// writes a Matroska file without an index (ffmpeg's `-cues_to_front` moves
    /// them, it cannot drop them), so the fixture is a real file with the `Cues`
    /// element id and the `SeekHead` pointer at it overwritten by an id nothing
    /// defines. Every byte else is where it was: an EBML reader skips an unknown
    /// element, which is exactly the file that has no index.
    #[test]
    fn a_file_with_no_cues_is_the_walk_it_always_was() {
        let source = asset("test_h264.mkv");
        let mut bytes = std::fs::read(&source).expect("test_h264.mkv");
        let cues = CUES.to_be_bytes();
        // One byte down in the last nibble: still a well-formed 4-byte EBML id,
        // and not one this reader (or the spec) knows.
        let unknown = (CUES - 1).to_be_bytes();
        let mut hidden = 0;
        for at in 0..bytes.len().saturating_sub(4) {
            if bytes[at..at + 4] == cues {
                bytes[at..at + 4].copy_from_slice(&unknown);
                hidden += 1;
            }
        }
        assert!(
            hidden >= 2,
            "the fixture states its Cues neither in a SeekHead nor as an element"
        );
        let path = crate::scratch::Scratch::file("no_cues", "mkv");
        std::fs::write(&path, &bytes).expect("the stripped fixture");

        let (want, mut walked) = MkvDemuxer::open(&source).expect("the cued file");
        let (got, mut fallback) = MkvDemuxer::open(&path).expect("the file with no cues");
        assert!(
            fallback.complete && fallback.cues.is_empty(),
            "a file with no Cues was not walked whole"
        );
        assert!(
            !walked.complete,
            "the source fixture carries no usable Cues"
        );
        assert_eq!(got.frame_count, want.frame_count, "frame count");
        assert_eq!(got.frame_rate, want.frame_rate, "frame rate");
        for frame in 0..got.frame_count {
            assert_eq!(
                fallback.seek_to_sync_at_or_before(frame),
                walked.seek_to_sync_at_or_before(frame),
                "seek to frame {frame} without cues"
            );
        }
    }

    /// A `Cues` element that lies is caught and the file is read anyway: the
    /// whole walk answers, and the caller cannot tell.
    ///
    /// The fixture is a real one with its first `CueTime` overwritten -- one
    /// tick on, so it is still a well-formed integer of the same width naming a
    /// frame number the file could have, and still a timestamp no block in the
    /// file carries. That is the shape of the file this whole path exists to
    /// survive: an index whose arithmetic cannot be checked against a block, from
    /// a variable-rate capture or a muxer with a bug. [`MkvDemuxer::cues_agree`]
    /// refuses it at open and [`MkvDemuxer::complete_index`] takes over, and what
    /// must not differ is the answer -- frame count, and where every frame of the
    /// file seeks to.
    #[test]
    fn a_cue_that_lies_about_its_time_degrades_to_the_walk() {
        let source = asset("test_hevc.mkv");
        let mut bytes = std::fs::read(&source).expect("test_hevc.mkv");
        // Where the first `CueTime` sits, found with the reader's own parsers
        // rather than by scanning the file for a byte pattern.
        let mut file = File::open(&source).expect("test_hevc.mkv");
        let end = file.metadata().expect("stat").len();
        let segment = mkv_segment(&mut file, end).expect("segment");
        let (body, stop) = mkv_cues_element(&mut file, segment)
            .expect("the Cues element")
            .expect("a fixture that carries cues");
        let mut cues = vec![0u8; (stop - body) as usize];
        read_exact_at(&mut file, body, &mut cues).expect("the Cues element's bytes");
        let (at, width) = first_cue_time(&cues).expect("a CueTime in the fixture");
        let was = uint_in(&cues, at, at + width);
        let at = body as usize + at;
        bytes[at..at + width].copy_from_slice(&(was + 1).to_be_bytes()[8 - width..]);
        let path = crate::scratch::Scratch::file("lying_cue", "mkv");
        std::fs::write(&path, &bytes).expect("the fixture with one cue changed");

        let (want, mut walked) = MkvDemuxer::open(&source).expect("the honest file");
        let (got, mut lying) = MkvDemuxer::open(&path).expect("the file whose cue lies");
        assert!(
            !walked.complete && !walked.cues.is_empty(),
            "the source fixture is the cue path, so the comparison means something"
        );
        assert!(
            !lying.complete,
            "the lie is not visible at open -- it is caught where the walk meets it"
        );
        assert_eq!(got.frame_count, want.frame_count, "frame count");
        // Reading the file is what walks past the lying cue, and `cues_agree`
        // catches it there: a cue is only checkable once the blocks around it
        // have been walked, which for a cue at the front of the file is one
        // cluster in.
        let mut read = 0;
        while lying.next_access_unit().expect("read past the lying cue").is_some() {
            read += 1;
        }
        assert_eq!(read, got.frame_count, "every frame of the file came back");
        assert!(
            lying.complete && lying.cues.is_empty(),
            "a cue naming no block was believed"
        );
        for frame in 0..got.frame_count {
            assert_eq!(
                lying.seek_to_sync_at_or_before(frame),
                walked.seek_to_sync_at_or_before(frame),
                "seek to frame {frame} off a cue index that lies"
            );
        }
    }

    /// Offset (into the `Cues` element's own bytes) and width of the first
    /// `CueTime` there, for the test above.
    fn first_cue_time(cues: &[u8]) -> Option<(usize, usize)> {
        let mut at = 0;
        while let Some((id, body, stop)) = ebml_in(cues, at) {
            at = stop;
            if id != CUE_POINT {
                continue;
            }
            let mut child = body;
            while let Some((id, body, stop)) = ebml_in(&cues[..stop], child) {
                child = stop;
                if id == CUE_TIME {
                    return Some((body, stop - body));
                }
            }
        }
        None
    }

    /// The sidecar index, against the walk it stands in for: what comes back out
    /// is the walk's own answer block for block, and every way a sidecar can be
    /// wrong is a miss rather than a wrong answer.
    ///
    /// The cache path itself is handed in here rather than taken from the
    /// environment, which is what lets a truncation and a flipped byte be tested
    /// at all; the end-to-end half below drives [`mkv_blocks`] and therefore the
    /// real cache directory.
    /// The fixture always, plus every path in `VE_INDEX_FILES` (`:`-separated),
    /// which is how this is pointed at the multi-GB films it really has to hold
    /// for without naming one of them in the tree.
    #[test]
    fn a_cached_block_index_is_the_walk_it_replaces() {
        let fixture =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_h264.mkv");
        index_round_trip(&fixture);
        for path in std::env::var("VE_INDEX_FILES")
            .unwrap_or_default()
            .split(':')
            .filter(|p| !p.is_empty())
        {
            index_round_trip(Path::new(path));
        }
    }

    fn index_round_trip(path: &Path) {
        let mut file = File::open(path).expect("the file under test");
        let end = file.metadata().expect("stat").len();
        let segment = mkv_segment(&mut file, end).expect("segment");
        let number = mkv_tracks(&mut file, segment)
            .expect("tracks")
            .0
            .expect("a video track")
            .number;
        let walked = mkv_walk_blocks(&mut file, segment, number).expect("walk");
        assert!(walked.0.len() > 1, "a file with blocks to index");

        let key = IndexKey::of(path, &file, segment, number).expect("a key off the open file");
        let at = crate::scratch::Scratch::file("ve_mkvindex", "idx");
        assert!(read_index(&key, &at).is_none(), "nothing written yet");
        write_index(&key, &at, &walked).expect("write the sidecar");
        let cached = read_index(&key, &at).expect("read it back");
        assert_eq!(cached.0, walked.0, "block for block, byte for byte");
        assert_eq!(cached.1, walked.1, "the same timestamp span");
        assert_eq!(cached.2, walked.2, "the same per-track byte counts");

        // Every shape of a stale key: a file that grew, one re-encoded in place,
        // and the other track of the same file.
        for stale in [
            IndexKey {
                len: key.len + 1,
                ..key_of(&key)
            },
            IndexKey {
                mtime: (key.mtime.0 + 1, key.mtime.1),
                ..key_of(&key)
            },
            IndexKey {
                number: key.number + 1,
                ..key_of(&key)
            },
            IndexKey {
                path: key.path.with_extension("other"),
                ..key_of(&key)
            },
        ] {
            assert!(
                read_index(&stale, &at).is_none(),
                "a key that does not match is a miss, not a wrong index"
            );
        }

        // ...and every shape of a damaged file. A flipped byte anywhere fails the
        // checksum; a truncation runs out of bytes; extra bytes are a record this
        // does not understand.
        let good = std::fs::read(&at).expect("the sidecar");
        for (why, bytes) in [
            ("a flipped byte in the payload", {
                let mut b = good.clone();
                let last = b.len() - 1;
                b[last] ^= 0x01;
                b
            }),
            ("a flipped byte in the key", {
                let mut b = good.clone();
                b[20] ^= 0x80;
                b
            }),
            ("another format version", {
                let mut b = good.clone();
                b[7] = b'0';
                b
            }),
            ("a truncated file", good[..good.len() / 2].to_vec()),
            ("an empty file", Vec::new()),
            ("bytes left over", [good.clone(), vec![0u8; 4]].concat()),
        ] {
            std::fs::write(&at, &bytes).expect("write the damaged sidecar");
            assert!(read_index(&key, &at).is_none(), "{why} is a miss");
        }

        // End to end, through the wrapper the demuxer calls and the real cache
        // directory: the first open writes the sidecar, the second reads it, and
        // neither may differ from the walk.
        for round in 0..2 {
            let (blocks, span, track_bytes) =
                mkv_blocks(path, &mut file, segment, number).expect("cached open");
            assert_eq!(blocks, walked.0, "round {round}");
            assert_eq!(span, walked.1, "round {round}");
            assert_eq!(track_bytes, walked.2, "round {round}");
        }
    }

    /// `IndexKey` is not `Clone` -- nothing in the demuxer needs it to be, and a
    /// test wanting four variants of one key is not a reason to widen it.
    fn key_of(key: &IndexKey) -> IndexKey {
        IndexKey {
            path: key.path.clone(),
            len: key.len,
            mtime: key.mtime,
            segment: key.segment,
            number: key.number,
        }
    }

    /// Where a sidecar goes, by the same rule the config file follows: the
    /// desktop's cache directory, `~/.cache` when it names none, and nowhere at
    /// all rather than the working directory when there is no home either.
    #[test]
    fn the_index_directory_follows_xdg() {
        let dir = |xdg: Option<&str>, home: Option<&str>| {
            index_dir(xdg.map(OsString::from), home.map(OsString::from))
        };
        assert_eq!(
            dir(Some("/x/cache"), Some("/home/u")),
            Some(PathBuf::from("/x/cache/edith/mkvindex"))
        );
        // An empty XDG_CACHE_HOME is one the spec says to ignore.
        assert_eq!(
            dir(Some(""), Some("/home/u")),
            Some(PathBuf::from("/home/u/.cache/edith/mkvindex"))
        );
        assert!(dir(None, Some("/home/u")).expect("a home").is_absolute());
        assert_eq!(dir(None, None), None, "no home, no cache -- walk instead");
        assert_eq!(dir(Some(""), Some("")), None);
    }


    /// What a `TrackEntry` states it is in, whichever of the two elements it
    /// states it in -- [`mkv_language`]'s whole rule, as asserts.
    #[test]
    fn a_track_states_its_language_in_either_element() {
        // The modern element wins over the legacy one, which is the spec's own
        // precedence: his films carry a Bulgarian legacy code beside an English
        // BCP-47 tag on different tracks, and the English ones state nothing
        // else at all.
        assert_eq!(mkv_language("", "en"), "eng");
        assert_eq!(mkv_language("bul", "en"), "eng");
        // A region is not a language: neither Matroska's legacy element nor an
        // mp4's 16-bit field can hold one, so `en-US` is the English it is.
        assert_eq!(mkv_language("", "en-US"), "eng");
        assert_eq!(mkv_language("", "pt-BR"), "por");
        assert_eq!(mkv_language("", "zh-Hans"), "zho");
        // ...and every other language of the table, not the ones a test used.
        assert_eq!(mkv_language("", "kn"), "kan");
        assert_eq!(mkv_language("", "JA"), "jpn");
        // Three letters already: an ISO 639-3 tag is what the code is.
        assert_eq!(mkv_language("", "fil"), "fil");
        // What nothing maps falls back to the file's other word rather than
        // being thrown away, and `und` is only for a file that said neither.
        assert_eq!(mkv_language("fra", "x-pig-latin"), "fra");
        assert_eq!(mkv_language("", "x-pig-latin"), "und");
        assert_eq!(mkv_language("fra", ""), "fra");
        assert_eq!(mkv_language("und", ""), "und");
        assert_eq!(mkv_language("", ""), "und");
        // The table is the standard's own: one entry per ISO 639-1 code, none
        // of them doubled, every code the two and three letters it is.
        let mut seen: Vec<&str> = ISO_639_1_TO_2.iter().map(|(two, _)| *two).collect();
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), count, "a doubled ISO 639-1 code");
        for (two, three) in ISO_639_1_TO_2 {
            assert!(
                two.len() == 2 && three.len() == 3,
                "{two} → {three} is not a 639-1 → 639-2 pair"
            );
        }
    }

    /// Every extension Matroska states, and nothing that is not one: `.mks` is
    /// the subtitles alone and `.mka` the sound alone, both the same bytes the
    /// `.mkv` reader already walks.
    #[test]
    fn every_matroska_extension_is_a_matroska() {
        for name in ["a.mkv", "a.mka", "a.mks", "a.mk3d", "a.webm", "A.MKS"] {
            assert!(is_matroska(Path::new(name)), "{name}");
        }
        for name in ["a.mp4", "a.m4v", "a.mov", "a.srt", "a", "a.mk"] {
            assert!(!is_matroska(Path::new(name)), "{name}");
        }
    }

    /// The capability matrix, as an assert: every codec the decoder layer can be
    /// handed is reachable from *both* container dispatches, and every gap is a
    /// row in [`UNSUPPORTED`] that is genuinely a gap. A codec wired into
    /// `engine-hw` and reachable from one container only is the defect this
    /// exists for -- VP9 was decodable out of an mp4 and refused out of a
    /// `.webm`, with the VA-API VP9 decoder compiled in the whole time.
    ///
    /// The `Codec::` list is written out rather than iterated because there is no
    /// variant list to iterate: what makes it stay complete is that both
    /// dispatches match on [`Codec`] with no wildcard, so a new variant fails to
    /// compile until it has been routed.
    #[test]
    fn every_codec_is_reachable_from_both_containers() {
        for codec in [Codec::H264, Codec::Hevc, Codec::Vp9, Codec::Av1] {
            let row = CODEC_IDS
                .iter()
                .find(|(c, ..)| *c == codec)
                .unwrap_or_else(|| panic!("{} has no row in CODEC_IDS", codec.name()));
            assert_eq!(
                mkv_codec(row.1),
                Some(codec),
                "{} is not reachable from a Matroska file",
                codec.name()
            );
            assert_eq!(
                mp4_codec(row.2),
                Some(codec),
                "{} is not reachable from an mp4",
                codec.name()
            );
        }
        // hev1 and hvc1 are one codec in two fourccs; both are HEVC.
        assert_eq!(mp4_codec(b"hev1"), Some(Codec::Hevc));

        for (id, why) in UNSUPPORTED {
            assert_eq!(mkv_codec(id), None, "{id} is supported after all ({why})");
            if let Ok(fourcc) = <&[u8; 4]>::try_from(id.as_bytes()) {
                assert_eq!(
                    mp4_codec(fourcc),
                    None,
                    "{id} is supported after all ({why})"
                );
            }
        }
        // ...and a codec nothing here reads is refused rather than mistaken for
        // its neighbour in the table.
        assert_eq!(mkv_codec("V_MPEG4/ISO/ASP"), None);
        assert_eq!(mp4_codec(b"jpeg"), None);
    }

    /// The one place a VP9 stream states its own depth. Byte-aligned by
    /// construction: the fields ahead of the frame sync code are exactly 8 bits
    /// for profiles 0-2.
    #[test]
    fn a_vp9_keyframe_states_its_own_bit_depth() {
        // frame_marker 10, profile bits, show_existing 0, frame_type 0 (key),
        // show_frame 1, error_resilient 0, then the sync code 49 83 42.
        let profile0 = [0b1000_0010, 0x49, 0x83, 0x42];
        assert_eq!(vp9_bit_depth(&profile0).unwrap(), Some(8));
        // profile 2 (low 0, high 1), then ten_or_twelve_bit = 0.
        let profile2 = [0b1001_0010, 0x49, 0x83, 0x42, 0b0000_0000];
        assert_eq!(vp9_bit_depth(&profile2).unwrap(), Some(10));
        // ...and with it set: 12-bit, which has no surface pool here and is
        // refused by name rather than decoded into garbage.
        let twelve = [0b1001_0010, 0x49, 0x83, 0x42, 0b1000_0000];
        let refused = vp9_bit_depth(&twelve).unwrap_err().to_string();
        assert!(refused.contains("12-bit VP9"), "{refused}");
        // Nothing to answer with is not an answer: an inter frame, a
        // show_existing_frame, a truncated block and a non-VP9 payload all leave
        // the caller's default standing.
        assert_eq!(
            vp9_bit_depth(&[0b1000_0110, 0x49, 0x83, 0x42]).unwrap(),
            None
        );
        assert_eq!(
            vp9_bit_depth(&[0b1000_1010, 0x49, 0x83, 0x42]).unwrap(),
            None
        );
        assert_eq!(vp9_bit_depth(&profile2[..3]).unwrap(), None);
        assert_eq!(vp9_bit_depth(&[0, 0, 0, 1, 0x65]).unwrap(), None);
        assert_eq!(vp9_bit_depth(&[]).unwrap(), None);
    }

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

    /// The open-GOP seek bug: `test_hevc.mkv`'s own timestamps, in the order the
    /// file stores them. The GOP-opening CRA is block 28 and the two blocks
    /// after it are its leading pictures, shown at 28 and 29 -- so the keyframe
    /// is the *30th* picture, and a seek that answered 28 handed back frame 47
    /// when it was asked for 45 (`hw_decode::seek_matches_linear_every_container`).
    #[test]
    fn a_keyframe_is_indexed_by_when_it_shows_not_where_it_is_stored() {
        // ms timestamps: 0, then the 3-picture reorder x265 writes, then the CRA
        // at 1000 ms with two leading pictures behind it in storage.
        let mut times: Vec<i64> = (0..28).map(|i| i64::from(i) * 100 / 3).collect();
        times.swap(1, 3);
        times.extend([1000, 967, 933]);
        let syncs = sync_display_order(&times, |i| i == 0 || i == 28);
        assert_eq!(syncs, vec![(0, 0), (30, 28)], "the CRA is shown 30th");

        assert_eq!(sync_display_at_or_before(&syncs, 45), Some((30, 28)));
        assert_eq!(
            sync_display_at_or_before(&syncs, 29),
            Some((0, 0)),
            "a leading picture is only reachable from the GOP before it"
        );
        assert_eq!(sync_display_at_or_before(&syncs, 30), Some((30, 28)));
        assert_eq!(sync_display_at_or_before(&syncs, 900), Some((30, 28)));
        assert_eq!(
            sync_display_at_or_before(&[(5, 5)], 2),
            None,
            "nothing decodable before the first sync point"
        );

        // Decode order back when nothing is reordered, which is what keeps every
        // closed-GOP stream exactly where it was.
        let plain: Vec<i64> = (0..8).map(|i| i64::from(i) * 33).collect();
        assert_eq!(
            sync_display_order(&plain, |i| i % 4 == 0),
            vec![(0, 0), (4, 4)]
        );
    }

    /// `ctts` is what makes an mp4's samples arrive out of display order, and a
    /// run it does not cover leaves those samples at their decode time.
    #[test]
    fn composition_times_add_the_ctts_delay() {
        // 4 samples, 100 ticks each; the first two swapped by a 100/-100 pair.
        let times = composition_times([(4u32, 100u32)], [(1u32, 100i32), (1, -100)]);
        assert_eq!(times, vec![100, 0, 200, 300]);
        assert_eq!(display_index(&times, 0), 1, "sample 1 is shown second");
        assert_eq!(display_index(&times, 1), 0);
        assert_eq!(display_index(&times, 3), 3);
        assert_eq!(
            composition_times([(2u32, 100u32)], []),
            vec![0, 100],
            "no ctts run at all leaves decode time standing"
        );
        assert_eq!(
            composition_times([(2u32, 100u32)], [(9u32, 50i32)]),
            vec![50, 150],
            "a ctts run past the last sample does not panic"
        );
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
