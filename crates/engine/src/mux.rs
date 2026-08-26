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
//! whose sound was silently left behind -- and both carry its subtitles, an mp4
//! as the `tx3g` timed text `mp4 0.14` *can* spell ([`Mp4Muxer::write_subtitles`])
//! and Matroska as `S_TEXT/UTF8` blocks, so neither is a file whose words were.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use mp4::{
    AacConfig, AudioObjectType, AvcConfig, Bytes, ChannelConfig, FourCC, MediaConfig, Mp4Config,
    Mp4Sample, Mp4Writer, SampleFreqIndex, TrackConfig, TrackType, TtxtConfig,
};

use crate::colorspace::ColorDescription;

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
/// The clock a `tx3g` track counts in: one millisecond, the very tick a Matroska
/// block's timestamp is written at ([`TIMESTAMP_SCALE_NS`]), so a cue lands on
/// the same instant whichever of the two containers carries it.
const SUB_TIMESCALE: u32 = 1_000;
/// How much coded picture [`Mp4Muxer`] holds unwritten waiting for the sound
/// to catch up ([`Mp4Muxer::hold`]) before it gives up and flushes video-only.
/// Two seconds is the ceiling `export`'s own progress granularity already
/// tolerates (`AUDIO_BAND`), and it bounds the hold's memory to two seconds
/// of coded pictures -- kilobytes, not the tens of megabytes a whole film's
/// worth would be.
const HOLD_SECONDS: u64 = 2;

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
    /// Set where the packets are **Opus** and not AAC, to the track's pre-skip
    /// in samples at 48 kHz ([`crate::export`] writes those only for a Matroska
    /// file). It is the whole difference between the two tracks a writer here
    /// can declare: `A_OPUS` with an `OpusHead` in `CodecPrivate`
    /// ([`opus_track_entry`]) against `A_AAC` with an `AudioSpecificConfig`
    /// ([`aac_track_entry`]). `None` is AAC, which is every mp4 and every copy.
    pub opus_pre_skip: Option<u16>,
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
    /// The `colr` box that entry gets either way -- what the samples in it mean
    /// ([`colr_nclx`]). `mp4 0.14` writes no `colr` of its own, so this is
    /// patched in at [`finish`](Mp4Muxer::finish) beside the entry rewrite,
    /// which is why an H.264 file is patched at all now.
    colr: [u8; 11],
    /// Where the file is, for that patch: the writer owns the handle it wrote
    /// through and the patch reopens the finished file.
    path: PathBuf,
    /// The video track's clock and how many pictures have been written on it --
    /// which is where the file *ends*, and a timed-text track is written out to
    /// that instant ([`write_subtitles`](Mp4Muxer::write_subtitles)).
    timescale: u32,
    frames: u64,
    /// What the subtitle tracks are *called*, in the order they were written:
    /// an mp4 states a track's title in a `trak/udta/name` box, which `mp4 0.14`
    /// has no field for, so it is patched in at [`finish`](Mp4Muxer::finish)
    /// beside the sample entry. Empty where no track carries a title, which is
    /// every file that has no subtitles at all.
    sub_names: Vec<String>,
    /// Coded pictures not yet handed to the writer, oldest first -- how the
    /// overlapped audio pass gets to land *under* the picture instead of
    /// after all of it: every `write_video_au`/`write_coded_sample` queues
    /// here and [`drain`](Mp4Muxer::drain) interleaves it against
    /// [`audio_queue`](Mp4Muxer::audio_queue) by presentation time, so the
    /// two tracks' bytes end up beside each other in `mdat` rather than one
    /// whole track then the other. `(bytes, is_sync)`.
    ///
    /// NONDETERMINISM: how tightly the two interleave depends on when the
    /// mix thread's join lands relative to the picture loop -- a mix that
    /// finishes before this hold fills stays byte-interleaved from the
    /// start; a mix that lands late only interleaves the tail. A test on
    /// this has to pin that landing (force the join before frame N in a
    /// fixture) rather than race the real mixer thread.
    hold: VecDeque<(Vec<u8>, bool)>,
    /// `hold`'s span, in the video track's own ticks -- flushed video-only
    /// once it passes [`HOLD_SECONDS`], so a mix that never lands (or lands
    /// very late) cannot stall the encode behind an unbounded queue.
    hold_ticks: u64,
    /// AAC packets handed over by [`write_audio_packet`](Mp4Muxer::write_audio_packet)
    /// before `drain` has caught up to them -- normally empty the instant
    /// after that call returns; only grows past one packet where the whole
    /// track lands in one call while the hold is still short (a picture that
    /// finished before its own sound).
    audio_queue: VecDeque<Vec<u8>>,
    /// Ticks, in the audio track's own rate, already written to the file --
    /// `drain`'s clock for comparing against `flushed_video_ticks`.
    audio_ticks: u64,
    /// Ticks, in the video track's own timescale, already written to the
    /// file (as opposed to merely queued in `hold`).
    flushed_video_ticks: u64,
    /// The audio track's sample rate, once [`add_audio_track`](Mp4Muxer::add_audio_track)
    /// has declared one -- `drain`'s clock for `audio_ticks`. 0 until then,
    /// which is also exactly when `audio_queue` can have nothing in it.
    audio_rate: u32,
}

/// A `ColourInformationBox` payload, ISO/IEC 14496-12 §12.1.5: the `nclx` tag
/// and the same three H.273 code points Matroska's `Colour` carries, plus the
/// range as the one-bit flag an mp4 says it with rather than Matroska's code.
fn colr_nclx(colour: ColorDescription) -> [u8; 11] {
    let (primaries, transfer, matrix) = colour.codes();
    let mut out = [0u8; 11];
    out[..4].copy_from_slice(b"nclx");
    out[4..6].copy_from_slice(&primaries.to_be_bytes());
    out[6..8].copy_from_slice(&transfer.to_be_bytes());
    out[8..10].copy_from_slice(&matrix.to_be_bytes());
    out[10] = u8::from(colour.full_range) << 7;
    out
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
    /// corner-cut: the ceiling is that one entry. A second video track, or an AV1
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
        let mut muxer = Self {
            writer,
            frame_duration,
            has_audio: false,
            entry,
            colr: colr_nclx(ColorDescription::output(video.height)),
            path: path.to_path_buf(),
            timescale,
            frames: 0,
            sub_names: Vec::new(),
            hold: VecDeque::new(),
            hold_ticks: 0,
            audio_queue: VecDeque::new(),
            audio_ticks: 0,
            flushed_video_ticks: 0,
            audio_rate: 0,
        };
        if let Some(audio) = audio {
            muxer.add_audio_track(audio)?;
        }
        Ok(muxer)
    }

    /// Declares the AAC track, which an mp4 may do at any point before its
    /// first packet is written: the sample table is built from the samples as
    /// they are handed over, and this file's audio is handed over after the
    /// whole picture ([`write_audio_packet`](Mp4Muxer::write_audio_packet)).
    ///
    /// That is what lets an export mix and encode its sound *while* it encodes
    /// the picture rather than before it: the picture's loop no longer needs to
    /// know the track's shape to start. Declaring it at the open, which is what
    /// a caller that already has the sound does, writes the same file --
    /// `mp4 0.14` builds the movie box at `finish` from tracks in the order
    /// they were added, and the samples themselves are written in the order the
    /// caller writes them either way.
    pub fn add_audio_track(&mut self, audio: &AudioParams) -> crate::Result<()> {
        // The one thing this writer must never do quietly: an `mp4a` sample
        // entry declares AAC, so Opus packets in it would be a file that says
        // one codec and holds another. `export` never asks -- Opus is written
        // for Matroska alone -- and this is the backstop that keeps that true
        // if a format is ever added on the other side.
        if audio.opus_pre_skip.is_some() {
            return Err("an Opus track in an mp4: this writer declares `mp4a` only".into());
        }
        if self.has_audio {
            return Err("an mp4 declares its audio track once".into());
        }
        let freq_index = SampleFreqIndex::try_from(audio.freq_index)?;
        // The index's own rate, not `audio.sample_rate`: this is the clock an
        // `stts` counts in and the `esds` states the index, so the two must not
        // be able to disagree.
        let sample_rate = freq_index.freq();
        self.writer.add_track(&TrackConfig {
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
        self.has_audio = true;
        self.audio_rate = sample_rate;
        Ok(())
    }

    /// Says what this file's samples mean, where that is *not* the rule the
    /// height implies -- a picture written in its source's own space rather
    /// than converted into the output one ([`crate::proxy`] is why: a stand-in
    /// carries the film's colour and lets the screen convert it, once, at
    /// preview size). The box is written when the file is finished, so this may
    /// be said any time before then.
    pub fn set_colour(&mut self, colour: ColorDescription) {
        self.colr = colr_nclx(colour);
    }

    /// One coded picture, Annex-B framed. Parameter sets inside it are dropped
    /// (they are already in `avcC`); an IDR slice marks the sample as a sync point.
    pub fn write_video_au(&mut self, annex_b: &[u8]) -> crate::Result<()> {
        let (bytes, is_sync) = annex_b_to_avcc(annex_b)?;
        self.queue_video(bytes, is_sync)
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
        self.queue_video(obus.to_vec(), key)
    }

    /// Queues one coded picture and tries to advance the interleave --
    /// shared by [`write_video_au`](Mp4Muxer::write_video_au) and
    /// [`write_coded_sample`](Mp4Muxer::write_coded_sample), which differ
    /// only in how the bytes were framed before they got here.
    fn queue_video(&mut self, bytes: Vec<u8>, is_sync: bool) -> crate::Result<()> {
        self.hold.push_back((bytes, is_sync));
        self.hold_ticks += u64::from(self.frame_duration);
        self.drain()
    }

    /// Advances the interleave as far as it currently can, called after
    /// every single sample either track hands over: for as long as both
    /// queues have something in them, writes whichever track's next sample
    /// has the earlier presentation time -- which is what makes the release
    /// of a mix that landed all at once ([`write_audio_packet`](Mp4Muxer::write_audio_packet)
    /// fed in one tight loop) paced by how much *video* keeps arriving to
    /// match it against, rather than dumped in one uninterrupted run the
    /// moment it lands.
    ///
    /// Video queued with nothing to interleave against yet -- no audio
    /// track, or one declared but not fed this far ahead -- is left held,
    /// unless the hold has grown past [`HOLD_SECONDS`], which gives up
    /// waiting and writes video-only to relieve it (a mix that never lands,
    /// or one running well behind the picture). Audio queued with no video
    /// to interleave against is always left held: nothing bounds how far
    /// *behind* the sound the picture may run, but every path that can end
    /// the file ([`finish`](Mp4Muxer::finish), [`write_subtitles`](Mp4Muxer::write_subtitles))
    /// drains it in full first.
    fn drain(&mut self) -> crate::Result<()> {
        loop {
            match (self.hold.front(), self.audio_queue.front()) {
                (Some(_), Some(_)) => {
                    let video_pts = self.flushed_video_ticks as f64 / f64::from(self.timescale);
                    let audio_pts = self.audio_ticks as f64 / f64::from(self.audio_rate);
                    if audio_pts <= video_pts {
                        self.flush_audio_front()?;
                    } else {
                        self.flush_video_front()?;
                    }
                }
                (Some(_), None) if self.hold_ticks > HOLD_SECONDS * u64::from(self.timescale) => {
                    self.flush_video_front()?;
                }
                (Some(_), None) | (None, Some(_)) | (None, None) => break,
            }
        }
        Ok(())
    }

    fn flush_video_front(&mut self) -> crate::Result<()> {
        let Some((bytes, is_sync)) = self.hold.pop_front() else {
            return Ok(());
        };
        self.hold_ticks -= u64::from(self.frame_duration);
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
        self.frames += 1;
        self.flushed_video_ticks += u64::from(self.frame_duration);
        Ok(())
    }

    fn flush_audio_front(&mut self) -> crate::Result<()> {
        let Some(bytes) = self.audio_queue.pop_front() else {
            return Ok(());
        };
        self.writer.write_sample(
            AUDIO_TRACK,
            &Mp4Sample {
                start_time: 0,
                duration: AAC_PACKET_SAMPLES,
                rendering_offset: 0,
                // Every AAC packet is a sync point; saying so per-sample would emit
                // an `stss` listing all of them, while no `stss` means the same thing.
                is_sync: false,
                bytes: Bytes::from(bytes),
            },
        )?;
        self.audio_ticks += u64::from(AAC_PACKET_SAMPLES);
        Ok(())
    }

    /// Everything either queue has left, in presentation order regardless of
    /// the hold cap -- called from [`finish`](Mp4Muxer::finish), where there
    /// is no more picture coming for a short hold to wait on.
    fn drain_all(&mut self) -> crate::Result<()> {
        loop {
            match (self.hold.front(), self.audio_queue.front()) {
                (Some(_), Some(_)) => {
                    let video_pts = self.flushed_video_ticks as f64 / f64::from(self.timescale);
                    let audio_pts = self.audio_ticks as f64 / f64::from(self.audio_rate);
                    if audio_pts <= video_pts {
                        self.flush_audio_front()?;
                    } else {
                        self.flush_video_front()?;
                    }
                }
                (Some(_), None) => self.flush_video_front()?,
                (None, Some(_)) => self.flush_audio_front()?,
                (None, None) => break,
            }
        }
        Ok(())
    }

    /// One raw AAC packet (no ADTS header), copied verbatim from the source --
    /// or one of the hand-written silent ones a gap is filled with. Every AAC-LC
    /// access unit is [`AAC_PACKET_SAMPLES`] frames, gap or not.
    ///
    /// Queues rather than writes straight through: a caller feeding packets
    /// while the picture is still running (the overlapped export path,
    /// [`crate::export::run`]) gets them woven into the file under the
    /// pictures still queued in [`hold`](Mp4Muxer::hold) instead of parked
    /// until the picture ends, which is the whole point of calling this
    /// before the picture loop is done rather than after.
    pub fn write_audio_packet(&mut self, bytes: &[u8]) -> crate::Result<()> {
        if !self.has_audio {
            return Err("audio packet written to a video-only file".into());
        }
        self.audio_queue.push_back(bytes.to_vec());
        self.drain()
    }

    /// The soft subtitle tracks this file carries, written after the picture and
    /// the sound exactly as [`MkvMuxer`] writes its own into the clusters -- one
    /// `tx3g` timed-text track per [`SubParams`], which is the text an mp4 says
    /// a cue with and the one `ffmpeg` calls `mov_text`. Called once, before
    /// [`finish`](Mp4Muxer::finish); an empty list writes nothing at all, which
    /// is why an export with no pick is the file it always was, byte for byte.
    ///
    /// The samples are *continuous*: a timed-text track states one sample per
    /// instant of the film and nothing between them, so the stretches with
    /// nothing to say are empty samples (a 16-bit zero length and no text) and
    /// not holes -- a hole is where a player keeps the last line on screen until
    /// the next one arrives. The run is built out of the cue *boundaries*, so
    /// cues that overlap are one sample of both their lines rather than one of
    /// them dropped: a `tx3g` track shows one sample at a time where a Matroska
    /// one may show two blocks at once, and joining them is what keeps the words
    /// a viewer saw over the picture.
    ///
    /// corner-cut: the `tx3g` sample entry is the one `mp4 0.14` writes and there
    /// is no field to reach into it -- `MediaConfig::TtxtConfig` carries nothing
    /// -- so its `data_reference_index` is the crate's 0 where §8.5.2 says an
    /// index into `dref` counts from 1, and it holds no `ftab` font table.
    /// ffmpeg and mpv both read the track (measured, 2026-08-12); a reader that
    /// checks the index would not. The upgrade path is the trick the `av01`
    /// entry already takes: rewrite the 46 fixed bytes of that entry in
    /// [`patch_entry`] the way the video one is rewritten.
    pub fn write_subtitles(&mut self, subs: &[SubParams]) -> crate::Result<()> {
        if subs.is_empty() {
            return Ok(());
        }
        // `self.frames` below counts only pictures already handed to the
        // writer, and the interleave above can still be holding some back:
        // drained first, or a text track written under a short hold would
        // end short of the picture that has not landed yet.
        self.drain_all()?;
        // Where the picture ends, in the text track's own tick: the last sample
        // runs out to it, so the track covers the film rather than stopping at
        // the last thing anybody says.
        let end = (self.frames * u64::from(self.frame_duration) * u64::from(SUB_TIMESCALE)
            / u64::from(self.timescale)) as i64;
        // The picture is track 1 and the sound, where there is any, track 2:
        // `mp4 0.14` numbers a track by the order it was added in, so the text
        // starts after them and each further track is the next number up.
        for (track_id, subs) in (2 + u32::from(self.has_audio)..).zip(subs) {
            self.writer.add_track(&TrackConfig {
                track_type: TrackType::Subtitle,
                timescale: SUB_TIMESCALE,
                // `und` and not an empty string where the source states no
                // language: an mp4 packs the three letters into a 16-bit field
                // and has no way to leave it out, and `und` is the code that
                // says *undetermined* -- while three zero bits would spell a
                // language nobody speaks.
                language: match subs.language.is_empty() {
                    true => "und".to_string(),
                    false => subs.language.clone(),
                },
                media_conf: MediaConfig::TtxtConfig(TtxtConfig {}),
            })?;
            for (bytes, duration) in timed_text(&subs.cues, end)? {
                self.writer.write_sample(
                    track_id,
                    &Mp4Sample {
                        start_time: 0, // ignored by the writer, as everywhere here
                        duration,
                        rendering_offset: 0,
                        // Every text sample is one a player may start at, and
                        // saying so per-sample would emit an `stss` listing all
                        // of them where no `stss` means the same thing -- the
                        // audio track's reason, and what ffmpeg's own `tx3g`
                        // track does.
                        is_sync: false,
                        bytes: Bytes::from(bytes),
                    },
                )?;
            }
        }
        self.sub_names = subs.iter().map(|subs| subs.name.clone()).collect();
        Ok(())
    }

    pub fn finish(mut self) -> crate::Result<()> {
        // Whatever the hold and the audio queue still carry: nothing further
        // is coming for either to wait on, so this drains both fully rather
        // than leaving a video-only tail queued.
        self.drain_all()?;
        self.writer.write_end()?;
        let Self {
            writer,
            entry,
            colr,
            path,
            sub_names,
            ..
        } = self;
        // Not a `drop`: the buffered tail of `moov` has to be on disk before the
        // patch below reads it back, and a `BufWriter` swallows the error of
        // flushing it on drop.
        writer.into_writer().flush()?;
        patch_entry(&path, entry.as_ref(), &colr, &sub_names)
    }
}

/// The cues of one subtitle track as the run of `tx3g` samples an mp4 carries
/// them in -- `(payload, duration in milliseconds)`, from instant 0 to `end`,
/// with no instant left out.
///
/// A sample's payload is the ISO/IEC 14496-17 one: the text's length in 16 bits
/// and then the UTF-8 itself, which is two zero bytes where there is nothing on
/// screen. The run is walked over the cue *boundaries* rather than over the cues,
/// which is what makes a gap an empty sample and an overlap one sample carrying
/// both lines -- the two things a track of samples has to say that a list of
/// cues does not.
///
/// corner-cut: the active set is looked for from the front of the list at every
/// boundary, so this is quadratic in the cues of one track (a 5 000-cue film
/// costs ~25 M compares once, at the end of an export that took minutes). The
/// upgrade path is a sweep holding the open cues in a heap keyed on their end.
fn timed_text(cues: &[crate::subtitle::Cue], end: i64) -> crate::Result<Vec<(Vec<u8>, u32)>> {
    // Milliseconds, the tick this track is written in -- and a cue that rounds
    // onto its own start stays up for one of them rather than vanishing, which
    // is what the Matroska writer's `BlockDuration` does with it.
    let ms = |us: i64| (us + 500) / 1_000;
    let spans: Vec<(i64, i64)> = cues
        .iter()
        .map(|cue| {
            let start = ms(cue.start_us);
            (start, ms(cue.end_us).max(start + 1))
        })
        .collect();
    let mut times: Vec<i64> = vec![0, end.max(0)];
    for &(start, stop) in &spans {
        times.push(start);
        times.push(stop);
    }
    times.sort_unstable();
    times.dedup();
    let mut out = Vec::with_capacity(times.len());
    for pair in times.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let mut text = String::new();
        for (span, cue) in spans.iter().zip(cues) {
            // The cues arrive in start order (`export::timeline_cues` sorts
            // them), so nothing past this one has opened yet.
            if span.0 > from {
                break;
            }
            if span.1 > from {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&cue.text);
            }
        }
        let len = u16::try_from(text.len()).map_err(|_| {
            format!(
                "a subtitle cue of {} bytes: a tx3g sample states its text length in 16 bits, so \
                 one cue carries at most {} -- export a Matroska, whose blocks state no length at \
                 all",
                text.len(),
                u16::MAX
            )
        })?;
        let mut bytes = Vec::with_capacity(text.len() + 2);
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(text.as_bytes());
        out.push((bytes, u32::try_from(to - from)?));
    }
    Ok(out)
}

/// Rewrites the finished file's one video sample entry: `colr` appended to it
/// always (the crate writes none), and where `entry` says so the `avc1` header
/// itself replaced by the `av01` + `av1C` an AV1 track declares -- or the `hvc1`
/// + `hvcC` an HEVC one does. Called once, on a complete file.
///
/// `moov` sits after `mdat` (the crate writes it at `write_end`), so this only
/// ever rewrites the tail of the file: no sample moves and no chunk offset in
/// `co64` changes. The box tree is rebuilt rather than patched in place, which
/// is what keeps every ancestor's size right by construction.
fn patch_entry(
    path: &Path,
    entry: Option<&([u8; 4], [u8; 4], Vec<u8>)>,
    colr: &[u8],
    sub_names: &[String],
) -> crate::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let end = file.metadata()?.len();
    let (at, payload) = top_level(&mut file, end, b"moov")?.ok_or("no moov box to patch")?;
    let patched = swap_entry(&payload, 0, entry, colr).ok_or("no avc1 sample entry to rewrite")?;
    let patched = name_subtitles(patched, sub_names);
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

/// The box at `depth` of [`STSD_PATH`], rebuilt with its `avc1` entry given a
/// `colr` box and, where `entry` says so, another four-character name and
/// configuration box. `None` where this branch holds no such entry -- an audio
/// `trak` is walked into and comes back untouched, which is how the *video*
/// track is found without being told which id it has.
fn swap_entry(
    payload: &[u8],
    depth: usize,
    entry: Option<&([u8; 4], [u8; 4], Vec<u8>)>,
    colr: &[u8],
) -> Option<Vec<u8>> {
    if depth == STSD_PATH.len() {
        // stsd is a FullBox (4) plus entry_count (4), then the sample entries.
        let mut out = payload.get(..8)?.to_vec();
        let (kind, sample_entry) = crate::demux::boxes(payload.get(8..)?).next()?;
        if kind != b"avc1" {
            return None;
        }
        let (name, mut swapped) = match entry {
            // A `VisualSampleEntry` is 78 bytes of fixed fields (dimensions and
            // all) before its codec box, and `av01` and `hvc1` carry exactly the
            // same ones -- so the header is kept and only the configuration box
            // is swapped.
            Some((want, config_kind, config)) => {
                let mut swapped = sample_entry.get(..78)?.to_vec();
                push_box(&mut swapped, config_kind, config);
                (want, swapped)
            }
            // H.264: the crate spelled the whole entry, `avcC` and all, and only
            // the colour tag is missing from it.
            None => (b"avc1", sample_entry.to_vec()),
        };
        push_box(&mut swapped, b"colr", colr);
        push_box(&mut out, name, &swapped);
        return Some(out);
    }
    let mut out = Vec::with_capacity(payload.len());
    let mut done = false;
    for (kind, child) in crate::demux::boxes(payload) {
        let patched = match !done && kind == STSD_PATH[depth] {
            true => swap_entry(child, depth + 1, entry, colr),
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

/// The title of each subtitle track into the `trak` that carries it, in the
/// order they were written: a `udta` holding a `name` box, which is where an mp4
/// says what a track is *called* (`Signs`) beside the `mdhd` language it is in
/// -- the pair Matroska spells as `Name` and `Language`, and the very boxes
/// ffmpeg writes a `-metadata:s:s:0 title=` into and reads back out.
/// `mp4 0.14`'s `TrakBox` has no `udta` field at all, so it is appended here
/// beside the sample entry rewrite, on the same rebuilt `moov`.
///
/// Untouched where no track has a title -- and returned as it came for a file
/// with no subtitles at all, which is every export that picked none.
fn name_subtitles(moov: Vec<u8>, names: &[String]) -> Vec<u8> {
    if names.iter().all(|name| name.is_empty()) {
        return moov;
    }
    let mut names = names.iter();
    let mut out = Vec::with_capacity(moov.len() + 32 * names.len());
    for (kind, child) in crate::demux::boxes(&moov) {
        // A subtitle `trak` is the one whose media handler says `sbtl`, which is
        // what `TrackType::Subtitle` wrote -- asked of the file rather than
        // counted, so the picture's and the sound's `trak`s cannot be miscounted
        // into.
        let name = match kind == b"trak" && is_subtitle_trak(child) {
            true => names.next().filter(|name| !name.is_empty()),
            false => None,
        };
        let Some(name) = name else {
            push_box(&mut out, kind, child);
            continue;
        };
        let mut trak = child.to_vec();
        let mut udta = Vec::new();
        push_box(&mut udta, b"name", name.as_bytes());
        push_box(&mut trak, b"udta", &udta);
        push_box(&mut out, kind, &trak);
    }
    out
}

/// Whether a `trak` is a subtitle one: its `mdia`'s `hdlr` names the `sbtl`
/// handler, which sits after the box's version, flags and the four `pre_defined`
/// bytes.
fn is_subtitle_trak(trak: &[u8]) -> bool {
    crate::demux::boxes(trak)
        .filter(|(kind, _)| *kind == b"mdia")
        .flat_map(|(_, mdia)| crate::demux::boxes(mdia))
        .any(|(kind, hdlr)| kind == b"hdlr" && hdlr.get(8..12) == Some(&b"sbtl"[..]))
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
// `Colour` and the four children this writes, all inside `Video`. Byte-verified
// against what `demux` reads back and what ffprobe reports: the spec tables that
// list Range as 0x55B3 are describing ChromaSubsamplingHorz.
const COLOUR: u32 = 0x55B0;
const MATRIX_COEFFICIENTS: u32 = 0x55B1;
const RANGE: u32 = 0x55B9;
const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
const PRIMARIES: u32 = 0x55BB;
const AUDIO: u32 = 0xE1;
const SAMPLING_FREQUENCY: u32 = 0xB5;
const CHANNELS: u32 = 0x9F;
const CODEC_DELAY: u32 = 0x56AA;
const SEEK_PRE_ROLL: u32 = 0x56BB;
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

/// What a *copied* track declares: the source's own codec id and configuration
/// record, in the space its samples are really in.
///
/// Nothing here is derived from the timeline -- the copy exists precisely
/// because none of it changed -- so the picture keeps the size, the rate, the
/// curve and the parameter sets it was coded with.
pub struct CopyParams<'a> {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    /// The Matroska codec id, `V_MPEGH/ISO/HEVC` or `V_AV1`.
    pub codec_id: &'a [u8],
    /// `CodecPrivate` as the source holds it ([`crate::demux::MkvDemuxer::codec_private`]).
    pub codec_private: &'a [u8],
    /// The source's own `Colour`: a copied HDR film leaves as the HDR film it
    /// is, because not one of its samples was touched.
    ///
    /// corner-cut: the `MasteringMetadata` and MaxCLL beside it are not carried --
    /// this engine parses them ([`crate::demux::ContentLight`]) but has nowhere
    /// to write them yet, so a copied HDR file states its curve and not its
    /// grading display. Upgrade path is a `Colour` element written from that
    /// same struct.
    pub colour: ColorDescription,
}

/// The soft subtitle track a file carries beside the picture: text, timed, still
/// text in the file -- a player draws it and a user can turn it off, which is
/// what "soft" means and what burning it into the pixels is not.
///
/// Both containers carry one: Matroska as an `S_TEXT/UTF8` track whose blocks
/// are the cues themselves ([`MkvMuxer`]), an mp4 as a `tx3g` timed-text track
/// whose samples run end to end over the film
/// ([`Mp4Muxer::write_subtitles`]).
///
/// The cues are the *exported timeline's* (`export::timeline_cues` puts them
/// there), because the file's clock is the timeline's; nothing here shifts
/// anything.
pub struct SubParams {
    /// What language the track is in, as the three-letter code Matroska says it
    /// with (`fra`, `tur`): the field a player's subtitle menu reads. Empty
    /// where the source states none -- and *only* then, because a `TrackEntry`
    /// without a `Language` means `eng` by spec, so a French track that leaves
    /// this empty leaves as an English one.
    pub language: String,
    /// What the track is *called*, where it has a title of its own beside its
    /// language (`Signs`, `Forced`). Empty where it has none.
    pub name: String,
    /// In start order, which is the order they are written in.
    pub cues: Vec<crate::subtitle::Cue>,
}

/// The cues of one subtitle track waiting to be interleaved into the clusters,
/// [`MkvAudio`]'s twin. One of these per track the file carries.
struct MkvSubs {
    cues: Vec<crate::subtitle::Cue>,
    /// Cues already written.
    next: usize,
    /// Which track its blocks name -- 3 for the first, one more for each after
    /// it ([`SUB_TRACK_FIRST`]).
    track_no: u8,
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
    /// The latest timestamp any picture was written at. Equal to the frame
    /// counter's own for a track this project encoded; for a *copied* one the
    /// blocks carry their source's timing and a cut can leave the last picture
    /// on screen, so the duration is measured rather than counted.
    last_ts: i64,
    /// The AAC track, if the timeline has sound, and how far it has been
    /// written.
    audio: Option<MkvAudio>,
    /// The subtitle tracks travelling with the file, in the order they were
    /// declared, and how far each has been written. Empty where none does.
    subs: Vec<MkvSubs>,
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
    /// corner-cut: that is the copy path's own ceiling reappearing (the track sits
    /// in memory, ~500 MB an hour); the upgrade path is the same streaming
    /// `copy_segments` `export::copy_audio` names.
    pub fn create(
        path: &Path,
        video: &Av1Params,
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
        subs: Vec<SubParams>,
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
            ColorDescription::output(video.height),
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
        subs: Vec<SubParams>,
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
            ColorDescription::output(video.height),
            audio,
            subs,
        )
    }

    /// The same file with a track *copied* out of another one: the source's own
    /// codec id and `CodecPrivate`, and its own `Colour` beside them
    /// ([`crate::export`]'s copy path).
    ///
    /// Not [`Self::create_hevc`] with the source's record, because the picture
    /// is not this project's own: a copy writes the file the source's samples
    /// already are -- an HDR film stays on its PQ curve rather than being
    /// declared the SDR the encoders here write -- and the blocks are timed by
    /// [`Self::write_block`] off the source's clock rather than by a frame
    /// counter, which is the other half of the same statement.
    pub fn create_copy(
        path: &Path,
        video: &CopyParams,
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
        subs: Vec<SubParams>,
    ) -> crate::Result<Self> {
        if video.codec_private.is_empty() {
            return Err("the source track carries no CodecPrivate to copy".into());
        }
        Self::open(
            path,
            video.width,
            video.height,
            video.frame_rate,
            video.codec_id,
            video.codec_private,
            video.colour,
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
        colour: ColorDescription,
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
        subs: Vec<SubParams>,
    ) -> crate::Result<Self> {
        if !frame_rate.is_finite() || frame_rate <= 0.0 {
            return Err(format!("bad frame rate {frame_rate}").into());
        }
        // Said before a byte is written, and by name: past this the block header
        // could not hold the track number ([`MAX_SUB_TRACKS`]) and the file would
        // be quietly wrong rather than absent.
        if subs.len() > MAX_SUB_TRACKS {
            return Err(format!(
                "{} subtitle tracks: a Matroska block writes its track number in \
                 one byte, so one file carries at most {MAX_SUB_TRACKS}",
                subs.len()
            )
            .into());
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
        // What the samples in those pixels mean. Written rather than left to a
        // reader's own 720-line guess -- the guess is right for an encoded file
        // (the export remaps every clip into exactly that space) but a remuxer
        // or a scaler downstream has no reason to make it, and an untagged file
        // is how a 601 source ends up displayed as 709. A *copied* track states
        // its source's space instead, which is the one its samples are in.
        let (primaries, transfer, matrix) = colour.codes();
        let mut tags = Vec::new();
        uint(&mut tags, MATRIX_COEFFICIENTS, u64::from(matrix));
        // Matroska says the range as a code, not a flag: 1 limited, 2 full.
        uint(&mut tags, RANGE, 1 + u64::from(colour.full_range));
        uint(&mut tags, TRANSFER_CHARACTERISTICS, u64::from(transfer));
        uint(&mut tags, PRIMARIES, u64::from(primaries));
        elem(&mut dims, COLOUR, &tags);
        elem(&mut entry, VIDEO, &dims);
        let mut tracks = Vec::new();
        elem(&mut tracks, TRACK_ENTRY, &entry);
        if let Some((audio, _)) = &audio {
            let entry = match audio.opus_pre_skip {
                Some(pre_skip) => opus_track_entry(audio, pre_skip)?,
                None => aac_track_entry(audio)?,
            };
            elem(&mut tracks, TRACK_ENTRY, &entry);
        }
        // Declared even where the audio track is not: the picture is always 1
        // and the sound always 2, so the text starts at 3 whether or not there
        // is any sound -- and each further track is the next number up, which is
        // the only counting in the file.
        for (i, subs) in subs.iter().enumerate() {
            let track_no = SUB_TRACK_FIRST + i as u8;
            elem(
                &mut tracks,
                TRACK_ENTRY,
                &subtitle_track_entry(&subs.language, &subs.name, track_no),
            );
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
            last_ts: 0,
            audio: audio.map(|(params, packets)| MkvAudio {
                packets,
                next: 0,
                samples: 0,
                sample_rate: params.sample_rate.max(1),
            }),
            subs: subs
                .into_iter()
                .enumerate()
                .map(|(i, subs)| MkvSubs {
                    cues: subs.cues,
                    next: 0,
                    track_no: SUB_TRACK_FIRST + i as u8,
                })
                .collect(),
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
        self.put(obus, key, ts)
    }

    /// One coded picture *copied* out of another file, shown at `ts_ns` on this
    /// file's clock rather than at the next frame of a counter.
    ///
    /// A copy has to be timed by the source's own timestamps and not by the
    /// order the blocks arrive in: a stream with B-frames is stored in decode
    /// order, so its timestamps step backwards over a group of pictures, and
    /// re-timing those blocks one frame apart would play the group in the order
    /// it was coded in. Matroska times every block in presentation and the
    /// relative timestamp in a block header is signed, so what a copy writes
    /// here is exactly what its source said.
    pub fn write_block(&mut self, payload: &[u8], key: bool, ts_ns: i64) -> crate::Result<()> {
        if payload.is_empty() {
            return Err("an empty coded block".into());
        }
        let ts = (ts_ns + TIMESTAMP_SCALE_NS as i64 / 2) / TIMESTAMP_SCALE_NS as i64;
        self.put(payload, key, ts)
    }

    /// One block into the cluster it belongs in, whatever timed it.
    fn put(&mut self, payload: &[u8], key: bool, ts: i64) -> crate::Result<()> {
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
        self.block(1, ts, key, payload);
        self.frames += 1;
        self.last_ts = self.last_ts.max(ts);
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
    /// never shows. Every track in turn, in the order they were declared.
    fn drain_subs(&mut self, until: i64) -> crate::Result<()> {
        for track in 0..self.subs.len() {
            loop {
                let subs = &mut self.subs[track];
                let Some(cue) = subs.cues.get_mut(subs.next) else {
                    break;
                };
                // Milliseconds, the tick this file is written in: a cue is read
                // for a second or more and no eye finds the half a tick it
                // rounds by.
                let ts = (cue.start_us + 500) / 1_000;
                if ts > until {
                    break;
                }
                // A cue that says it ends before it starts stays up for one tick
                // rather than for a duration a reader would take as unsigned.
                let ms = ((cue.end_us - cue.start_us + 500) / 1_000).max(1) as u64;
                let text = std::mem::take(&mut cue.text);
                subs.next += 1;
                let track_no = subs.track_no;
                // A cluster of its own where the text has run past what a 16-bit
                // relative timestamp reaches, exactly as the sound's drain does.
                if self.cluster.is_empty() || ts - self.cluster_ts >= CLUSTER_MS {
                    self.flush()?;
                    self.cluster_ts = ts;
                    uint(&mut self.cluster, CLUSTER_TIMESTAMP, ts as u64);
                }
                self.block_group(track_no, ts, ms, text.as_bytes());
            }
        }
        Ok(())
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
        // One frame past the last picture shown, which is where the file really
        // ends: counting the frames instead would cut a copied track short by
        // however many pictures a cut left the one before it on screen for.
        let ms = (self.frames as f64 * self.frame_ns as f64 / TIMESTAMP_SCALE_NS as f64)
            .max(self.last_ts as f64 + self.frame_ns as f64 / TIMESTAMP_SCALE_NS as f64);
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

/// The same track when the sound is Opus: `A_OPUS`, whose `CodecPrivate` is the
/// 19-byte `OpusHead` identification header of RFC 7845 §5.1 -- the very bytes
/// an Ogg Opus file opens with, which is why one writer serves both containers
/// and why this project's own reader configures itself from it unchanged
/// (`audio::opus_pre_skip` and `audio::opus_layout` parse exactly this).
///
/// Mapping family 0: mono or stereo, no channel-mapping table, which is the only
/// shape [`crate::export::encode_opus`] writes. The rate field is what the
/// *input* was, not what the stream is coded at -- every Opus decoder runs at
/// 48 kHz whatever it says -- and the output gain is 0 because the mix is
/// already at the level the timeline asked for.
///
/// Two elements no AAC track needs, and both are the difference between a file
/// that plays in sync and one that does not:
/// * `CodecDelay`, the pre-skip in nanoseconds. Matroska times an Opus block for
///   the *audible* stream while the decoder hands back the pre-skip in front of
///   it, so a reader that ignores this starts 2.5 ms early -- measured the other
///   way round on a real film remux (`audio::Track::samples_at`).
/// * `SeekPreRoll`, fixed at 80 ms for Opus by the Matroska spec: how much has
///   to be decoded and thrown away before a seek target for the decoder to be
///   warm. It is a constant of the codec, not of the file.
fn opus_track_entry(audio: &AudioParams, pre_skip: u16) -> crate::Result<Vec<u8>> {
    let rate = audio.sample_rate;
    if rate == 0 || !(1..=2).contains(&audio.chan_conf) {
        return Err(format!(
            "an Opus track at {rate} Hz with {} channels",
            audio.chan_conf
        )
        .into());
    }
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(audio.chan_conf);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // mapping family

    let mut entry = Vec::new();
    uint(&mut entry, TRACK_NUMBER, 2);
    uint(&mut entry, TRACK_UID, 2);
    uint(&mut entry, TRACK_TYPE, 2); // audio
    uint(&mut entry, FLAG_LACING, 0);
    elem(&mut entry, CODEC_ID, b"A_OPUS");
    elem(&mut entry, CODEC_PRIVATE, &head);
    uint(
        &mut entry,
        CODEC_DELAY,
        u64::from(pre_skip) * 1_000_000_000 / u64::from(rate),
    );
    uint(&mut entry, SEEK_PRE_ROLL, 80_000_000);
    let mut audio_elem = Vec::new();
    put_id(&mut audio_elem, SAMPLING_FREQUENCY);
    put_size(&mut audio_elem, 8);
    audio_elem.extend_from_slice(&f64::from(rate).to_be_bytes());
    uint(&mut audio_elem, CHANNELS, u64::from(audio.chan_conf));
    elem(&mut entry, AUDIO, &audio_elem);
    Ok(entry)
}

/// Which track the *first* subtitle block names, the picture's 1 and the sound's
/// 2 being fixed: a file with no audio track still writes its first text track
/// on 3, so nothing has to count the tracks before it to read a block. Each
/// further subtitle track is the next number up.
const SUB_TRACK_FIRST: u8 = 3;

/// How many subtitle tracks one file may carry. A block writes its track number
/// as a one-byte EBML integer (`0x80 | track`, [`MkvMuxer::block`]), and 127 is
/// one number too far: its byte is `0xFF`, the all-ones variable-length integer
/// EBML spells *unknown* with, which this project's own reader hands back as
/// `u64::MAX` (`demux`'s `vint`) -- so track 127's blocks would match no track,
/// its cues would vanish on the way back in, and the file would be one edith
/// wrote and cannot read. The numbers therefore run `3..=126` and the count is
/// what is left of them -- derived from the encoding, so it cannot drift from
/// it. A file asking for more is refused by name in [`MkvMuxer::open`] rather
/// than written with a byte that means another track, or none.
pub const MAX_SUB_TRACKS: usize = 0x7F - SUB_TRACK_FIRST as usize;

/// The `TrackEntry` of the subtitle track: type 0x11 (subtitles) and
/// `S_TEXT/UTF8`, whose blocks are the cue's own UTF-8 text and whose timing is
/// the block's -- the codec every player draws and the one this project's own
/// reader parses back (`subtitle::cues_of`).
///
/// The two names a track has are written as the two fields Matroska has for
/// them: `Language` is what a player's subtitle menu offers and what a "play
/// French" setting matches on, `Name` is the title beside it (`Signs`). Both
/// where the source has both -- a track carrying only a name is an *English*
/// track to every reader, that being the spec's default, which is how a
/// multi-language film used to export as several English ones. Either is left
/// out where it is empty, and [`crate::subtitle::of_matroska`] reads exactly
/// this back, so a track exported and re-imported keeps what it had.
fn subtitle_track_entry(language: &str, name: &str, track_no: u8) -> Vec<u8> {
    let mut entry = Vec::new();
    uint(&mut entry, TRACK_NUMBER, u64::from(track_no));
    uint(&mut entry, TRACK_UID, u64::from(track_no));
    uint(&mut entry, TRACK_TYPE, 0x11);
    uint(&mut entry, FLAG_LACING, 0);
    elem(&mut entry, CODEC_ID, b"S_TEXT/UTF8");
    if !language.is_empty() {
        elem(&mut entry, TRACK_LANGUAGE, language.as_bytes());
    }
    if !name.is_empty() {
        elem(&mut entry, TRACK_NAME, name.as_bytes());
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

    use crate::scratch::Scratch;

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

    fn cue(start_ms: i64, end_ms: i64, text: &str) -> crate::subtitle::Cue {
        crate::subtitle::Cue {
            start_us: start_ms * 1_000,
            end_us: end_ms * 1_000,
            text: text.into(),
            image: None,
        }
    }

    /// What a `tx3g` track is made of: one sample per instant of the film and no
    /// hole anywhere. The gaps -- before the first cue, between two, after the
    /// last -- are empty samples (a 16-bit zero and nothing else), and the whole
    /// run adds up to the length it was asked for, which is what stops a player
    /// leaving the last line on screen over the rest of the film.
    #[test]
    fn a_timed_text_track_covers_every_instant() {
        let cues = [cue(500, 1_500, "first"), cue(4_000, 4_750, "third")];
        let samples = timed_text(&cues, 5_000).unwrap();
        let times: Vec<u32> = samples.iter().map(|(_, ms)| *ms).collect();
        assert_eq!(times, [500, 1_000, 2_500, 750, 250], "{samples:?}");
        assert_eq!(times.iter().sum::<u32>(), 5_000, "the film, end to end");
        let texts: Vec<&[u8]> = samples.iter().map(|(bytes, _)| &bytes[..]).collect();
        assert_eq!(texts[0], b"\0\0", "nothing is said until 0.5 s");
        assert_eq!(texts[1], b"\0\x05first");
        assert_eq!(texts[2], b"\0\0", "...nor between the two cues");
        assert_eq!(texts[3], b"\0\x05third");
        assert_eq!(texts[4], b"\0\0", "...nor after the last one");
    }

    /// Two cues over one another: a Matroska file shows both blocks at once and
    /// a `tx3g` track has one sample per instant, so the overlap is *one* sample
    /// carrying both lines rather than one of them dropped -- the words a viewer
    /// saw over the picture, which is what this file is a copy of.
    #[test]
    fn overlapping_cues_are_one_sample_of_both_lines() {
        let cues = [cue(0, 2_000, "sign"), cue(1_000, 3_000, "dialogue")];
        let samples = timed_text(&cues, 3_000).unwrap();
        let got: Vec<(String, u32)> = samples
            .iter()
            .map(|(bytes, ms)| (String::from_utf8_lossy(&bytes[2..]).into_owned(), *ms))
            .collect();
        assert_eq!(
            got,
            [
                ("sign".into(), 1_000),
                ("sign\ndialogue".into(), 1_000),
                ("dialogue".into(), 1_000),
            ],
            "{samples:?}"
        );
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
        let out = Scratch::file("ve_mux_never_written", "mp4");
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
        let out = Scratch::file("ve_mux", "mp4");

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
                opus_pre_skip: None,
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

        let out = Scratch::file("ve_mkv_sound", "mkv");
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
                    opus_pre_skip: None,
                },
                packets,
            )),
            Vec::new(),
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

        let out = Scratch::file("ve_mp4_av1", "mp4");
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
                opus_pre_skip: None,
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

        let out = Scratch::file("ve_mkv", "mkv");
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
            Vec::new(),
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
                Vec::new(),
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
        let out = Scratch::file("ve_mux_ntsc", "mp4");
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
