//! Annex-B -> MP4, the mirror of `demux`: encoders emit start-code framed NALs,
//! an mp4 sample wants 4-byte length prefixes with SPS/PPS living in `avcC`.
//!
//! ...and AV1 -> Matroska, the mirror of the *other* half of `demux`: an AV1
//! export is written as EBML by hand exactly as an AV1 import is read as EBML by
//! hand ([`MkvMuxer`]). The mp4 side is [`Mp4Muxer`], written through `ec-mp4`,
//! which carries H.264, HEVC and AV1 sample entries itself.
//!
//! Both containers carry the timeline's AAC, so no format here writes a picture
//! whose sound was silently left behind -- and both carry its subtitles, an mp4
//! as the `tx3g` timed text ([`Mp4Muxer::write_subtitles`])
//! and Matroska as `S_TEXT/UTF8` blocks, so neither is a file whose words were.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use ec_core::{
    AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, ColorInfo, MediaParameters,
    Muxer as _, Packet, StreamInfo, TimeBase, VideoParameters,
};

use crate::colorspace::{ColorDescription, Tags};
use crate::demux::mp4_error;


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
    /// The track's language as a caller names it (`und` when it names none),
    /// written to the mp4 `mdhd` through [`mp4_language`].
    pub language: Option<String>,
}


/// The AAC sampling frequencies, indexed by the `samplingFrequencyIndex` an
/// `AudioSpecificConfig` states (ISO/IEC 14496-3 table 1.16).
const AAC_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// The `tx3g` sample entry's style, after its data reference index: the 30 bytes
/// `TextSampleEntry` holds from `displayFlags` on, which `ec-mp4` writes as they
/// are. Bottom-centred, an opaque black background, the text box the whole
/// picture, and font 1 at size 16 in opaque white -- the entry this writer has
/// always declared, so a player draws a subtitle exactly as before.
const TX3G_STYLE: [u8; 30] = [
    0, 0, 0, 0, // displayFlags
    1, // horizontal justification: centred
    0xFF, // vertical justification: bottom
    0, 0, 0, 255, // background: opaque black
    0, 0, 0, 0, 0, 0, 0, 0, // text box: left unset, so the whole picture
    0, 0, // startChar
    0, 0, // endChar
    0, 1, // font-ID
    0, // font flags
    16, // font size
    255, 255, 255, 255, // text colour: opaque white
];

/// The video track is stream 0 and the sound, where there is any, stream 1:
/// streams are numbered in the order they are declared, and the picture is
/// declared first.
const VIDEO_STREAM: u32 = 0;
const AUDIO_STREAM: u32 = 1;
/// AAC-LC encoder delay in samples: the first access unit of every AAC track
/// this writer takes, copied or encoded, is 1024 samples of priming that no
/// sound should start with. Declared as an `elst` media_time, so a reader plays
/// the sound from timeline frame 0 and does not run it 21 ms late.
const AAC_PRIMING: u32 = 1024;

/// Writes one H.264, HEVC or AV1 track plus an optional AAC track, through
/// `ec-mp4`'s muxer. Timing is carried per sample: every sample is handed over
/// with its exact decode time and duration, so the tracks cannot drift.
pub struct Mp4Muxer {
    writer: ec_mp4::Mp4Muxer<BufWriter<File>>,
    frame_duration: u32,
    /// Whether the file carries a picture track. A sound-only file
    /// ([`Mp4Muxer::create_audio_only`]) has none, and then the audio is the
    /// stream 0 it is declared as.
    has_video: bool,
    has_audio: bool,
    /// The stream index the audio track was declared at, which is what its
    /// packets are written under: 1 behind a picture, 0 without one.
    audio_stream: u32,
    /// How many streams have been declared: the next one's index.
    streams: u32,
    /// What the picture's samples mean, applied at [`finish`](Mp4Muxer::finish).
    colour: ColorInfo,
    /// HDR static metadata for the `mdcv` / `clli` boxes, written at finish.
    light: ec_core::ContentLight,
    /// The video track's clock and how many pictures have been written on it --
    /// which is where the file *ends*, and a timed-text track is written out to
    /// that instant ([`write_subtitles`](Mp4Muxer::write_subtitles)).
    timescale: u32,
    frames: u64,
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
    /// The sound's exact length in PCM frames, where the caller knows it
    /// ([`Mp4Muxer::set_audio_frames`]). Only an audio-only file reads it.
    audio_frames: Option<u64>,
}

/// The H.273 colour description of a picture, as the `colr` box states it.
fn color_info(colour: Tags) -> ColorInfo {
    // H.273's 2 is "unspecified": what a field the source left out stays in the box.
    ColorInfo {
        primaries: colour.primaries.unwrap_or(2) as u8,
        transfer: colour.transfer_code.unwrap_or(2) as u8,
        matrix: colour.matrix_code.unwrap_or(2) as u8,
        full_range: colour.full_range.unwrap_or(false),
    }
}

/// An H.264 `AVCDecoderConfigurationRecord` (ISO/IEC 14496-15 §5.3.3.1) for one
/// SPS and one PPS, with four-byte NAL length prefixes -- the framing
/// [`annex_b_to_avcc`] writes the samples in.
fn avcc_record(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut out = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    out
}

/// The `AudioSpecificConfig` of an AAC-LC track at `freq_index` and
/// `chan_conf`: object type 2, then the two indices packed into two bytes.
fn audio_specific_config(freq_index: u8, chan_conf: u8) -> [u8; 2] {
    [
        (2 << 3) | (freq_index >> 1),
        ((freq_index & 1) << 7) | (chan_conf << 3),
    ]
}

/// The language an mp4 track states, from whatever a caller said the track is
/// in.
///
/// `mdhd` packs exactly three ISO 639-2 letters into its 16-bit field, and the
/// `mp4` crate's own `language_code` (`vendor/mp4/src/mp4box/mdhd.rs:157`)
/// masks the three UTF-16 units it is handed with `0x1F` and drops the rest --
/// so a two-letter `en` is written as `en` and a backtick, and a `pt-BR` tag as
/// `ptm`, silently, with a player's subtitle menu then offering a language
/// nobody speaks. Matroska's `Language` is raw UTF-8 beside that and may carry
/// a BCP-47 tag, so the tag is cut to its primary subtag here, mapped through
/// [`crate::demux::ISO_639_1_TO_2`] where it is two letters, and left `und`
/// -- the code that says *undetermined* -- where nothing names it.
///
/// Three lowercase letters are taken as they are: that is the code the field
/// holds, and it is what an ISO 639-3 tag like `fil` and every language
/// `crate::demux::mkv_language` hands over already are.
fn mp4_language(language: &str) -> String {
    let primary = language
        .split('-')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let code = match primary.len() {
        2 => crate::demux::ISO_639_1_TO_2
            .iter()
            .find(|(two, _)| *two == primary)
            .map(|(_, three)| *three),
        3 if primary.bytes().all(|b| b.is_ascii_lowercase()) => Some(primary.as_str()),
        _ => None,
    };
    code.unwrap_or("und").to_owned()
}


impl Mp4Muxer {
    /// Declares the sound's exact length in PCM frames per channel, for an
    /// audio-only file: its `elst` plays `frames` samples from the priming and
    /// stops there, so the file says what was exported and not what the last
    /// AAC packet happens to carry. Written at [`finish`](Self::finish).
    pub fn set_audio_frames(&mut self, frames: u64) -> crate::Result<()> {
        self.audio_frames = Some(frames);
        Ok(())
    }

    /// An audio-only file (`.m4a` or a sound-only `.mp4`). The `ec-mp4` writer
    /// has no video-less file and no `M4A ` brand yet, so this is refused by name
    /// rather than written as a file that says the wrong thing.
    pub fn create_audio_only(path: &Path, audio: &AudioParams, m4a: bool) -> crate::Result<Self> {
        // The `.m4a` says so in its major brand, which is the one a player picks
        // an audio parser off; an `.mp4` of sound is the plain `isom` file.
        let (major, compatible): ([u8; 4], &[[u8; 4]]) = match m4a {
            true => (*b"M4A ", &[*b"M4A ", *b"mp42", *b"isom"]),
            false => (*b"isom", &[*b"isom", *b"iso2", *b"mp41"]),
        };
        let writer = ec_mp4::Mp4Muxer::with_brands(
            BufWriter::new(File::create(path)?),
            major,
            compatible,
        )
        .map_err(mp4_error)?;
        let mut muxer = Self {
            writer,
            frame_duration: 1,
            has_video: false,
            has_audio: false,
            audio_stream: 0,
            colour: color_info(Tags::default()),
            light: Default::default(),
            streams: 0,
            timescale: 1,
            frames: 0,
            hold: VecDeque::new(),
            hold_ticks: 0,
            audio_queue: VecDeque::new(),
            audio_ticks: 0,
            flushed_video_ticks: 0,
            audio_rate: 0,
            audio_frames: None,
        };
        muxer.add_audio_track(audio)?;
        Ok(muxer)
    }

    pub fn create(
        path: &Path,
        video: &VideoParams,
        audio: Option<&AudioParams>,
    ) -> crate::Result<Self> {
        // AvcCBox-style indexing of sps[1..=3] for profile and level: a short
        // SPS is refused here, where the message still means something.
        if video.sps.len() < 4 || video.pps.is_empty() {
            return Err("no usable SPS/PPS for the video track".into());
        }
        let config = avcc_record(video.sps, video.pps);
        Self::open(
            path,
            (video.width, video.height, video.frame_rate),
            CodecId::H264,
            config,
            audio,
        )
    }

    /// The same file with an AV1 track in it: an `av01` sample entry with its
    /// `av1C` record -- the very four bytes and sequence header OBU
    /// [`MkvMuxer`] puts in `CodecPrivate`, which is what makes the two
    /// containers' AV1 one stream written twice rather than two.
    pub fn create_av1(
        path: &Path,
        video: &Av1Params,
        audio: Option<&AudioParams>,
    ) -> crate::Result<Self> {
        if video.config.is_empty() {
            return Err("no AV1 sequence header for the track's av1C record".into());
        }
        let mut av1c = av1c_head(video.ten_bit).to_vec();
        av1c.extend_from_slice(video.config);
        Self::open(
            path,
            (video.width, video.height, video.frame_rate),
            CodecId::Av1,
            av1c,
            audio,
        )
    }

    /// An HEVC track, its `hvcC` record the very bytes the Matroska file puts
    /// in `CodecPrivate`, so the two containers carry one stream. The sample
    /// entry is `hvc1`: the parameter sets are in the `hvcC`, which is exactly
    /// what the `hvc1` name promises a reader.
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
            (video.width, video.height, video.frame_rate),
            CodecId::H265,
            video.hvcc.to_vec(),
            audio,
        )
    }

    fn open(
        path: &Path,
        (width, height, frame_rate): (u32, u32, f64),
        codec: CodecId,
        config: Vec<u8>,
        audio: Option<&AudioParams>,
    ) -> crate::Result<Self> {
        if !frame_rate.is_finite() || frame_rate <= 0.0 {
            return Err(format!("bad frame rate {frame_rate}").into());
        }
        if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
            return Err(format!("bad dimensions {width}x{height}").into());
        }

        let (timescale, frame_duration) = frame_timing(frame_rate)?;
        let mut writer = ec_mp4::Mp4Muxer::new(BufWriter::new(File::create(path)?))
            .map_err(mp4_error)?;
        let video_info = StreamInfo::new(
            VIDEO_STREAM,
            TimeBase::new(1, i64::from(timescale)),
            CodecParameters {
                codec,
                extradata: Some(Buf::from(config)),
                media: MediaParameters::Video(VideoParameters {
                    width,
                    height,
                    format: None,
                    frame_rate: Some(TimeBase::new(
                        i64::from(timescale),
                        i64::from(frame_duration),
                    )),
                    sample_aspect_ratio: None,
                    color: ColorInfo::default(),
                    light: Default::default(),
                }),
            },
        );
        writer.add_stream(video_info).map_err(mp4_error)?;
        let mut muxer = Self {
            writer,
            frame_duration,
            has_video: true,
            has_audio: false,
            audio_stream: AUDIO_STREAM,
            streams: 1,
            colour: color_info(Tags::of(ColorDescription::output(height))),
            light: Default::default(),
            timescale,
            frames: 0,
            hold: VecDeque::new(),
            hold_ticks: 0,
            audio_queue: VecDeque::new(),
            audio_ticks: 0,
            flushed_video_ticks: 0,
            audio_rate: 0,
            audio_frames: None,
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
    pub fn add_audio_track(&mut self, audio: &AudioParams) -> crate::Result<()> {
        // An `mp4a` sample entry declares AAC, so Opus packets in it would be a
        // file that says one codec and holds another. `export` never asks.
        if audio.opus_pre_skip.is_some() {
            return Err("an Opus track in an mp4: this writer declares `mp4a` only".into());
        }
        if self.has_audio {
            return Err("an mp4 declares its audio track once".into());
        }
        // The index's own rate, not `audio.sample_rate`: this is the clock an
        // `stts` counts in and the config states the index, so the two must not
        // be able to disagree.
        let sample_rate = *AAC_RATES
            .get(usize::from(audio.freq_index))
            .ok_or_else(|| format!("no AAC sampling frequency index {}", audio.freq_index))?;
        let channels = match audio.chan_conf {
            1..=6 => usize::from(audio.chan_conf),
            7 => 8,
            other => return Err(format!("no AAC channel configuration {other}").into()),
        };
        let mut params = CodecParameters::new(CodecId::Aac);
        params.extradata = Some(Buf::from(
            audio_specific_config(audio.freq_index, audio.chan_conf).to_vec(),
        ));
        params.media = MediaParameters::Audio(AudioParameters {
            sample_rate,
            layout: ChannelLayout::from_count(channels),
            ..Default::default()
        });
        let mut info = StreamInfo::new(self.streams, TimeBase::from_rate(sample_rate), params);
        info.language = Some(mp4_language(audio.language.as_deref().unwrap_or("und")));
        self.audio_stream = self.add_stream(info)?;
        self.has_audio = true;
        self.audio_rate = sample_rate;
        Ok(())
    }

    /// Declares a stream and returns its index, which is the one its packets
    /// name.
    fn add_stream(&mut self, info: StreamInfo) -> crate::Result<u32> {
        let index = self.writer.add_stream(info).map_err(mp4_error)?;
        self.streams += 1;
        Ok(index)
    }

    /// Says what this file's samples mean, where that is *not* the rule the
    /// height implies -- a picture written in its source's own space rather
    /// than converted into the output one ([`crate::proxy`] is why: a stand-in
    /// carries the film's colour and lets the screen convert it, once, at
    /// preview size). Applied when the file is finished, so this may be said
    /// any time before then.
    pub fn set_colour(&mut self, colour: Tags) {
        self.colour = color_info(colour);
    }

    /// HDR static metadata for the `mdcv` / `clli` boxes, written at finish.
    pub fn set_hdr(&mut self, hdr: &crate::export::HdrOutput) {
        self.light = hdr_light(hdr);
    }

    /// The chapter marks, `(start in ns, title)`, written at [`finish`](Mp4Muxer::finish) as a
    /// `chpl` box and a chapter text track ([`ec_mp4::Mp4Muxer::set_chapters`]). Empty writes none.
    pub fn set_chapters(&mut self, chapters: &[(u64, String)]) -> crate::Result<()> {
        self.writer.set_chapters(chapters).map_err(mp4_error)
    }

    /// One coded picture, Annex-B framed. Parameter sets inside it are dropped
    /// (they are already in the sample entry); an IDR slice marks the sample as
    /// a sync point.
    pub fn write_video_au(&mut self, annex_b: &[u8]) -> crate::Result<()> {
        let (bytes, is_sync) = annex_b_to_avcc(annex_b)?;
        self.queue_video(bytes, is_sync)
    }

    /// One coded picture already framed the way its sample carries it: an AV1
    /// temporal unit is the low-overhead OBU stream the encoder handed over, and
    /// an HEVC sample is the length-prefixed NALs [`annex_b_to_hvcc`] made. `key`
    /// is the encoder's own word for a unit a decoder may start on.
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

    /// Hands one sample to the muxer with its exact decode time and duration.
    fn put(
        &mut self,
        stream: u32,
        time_base: TimeBase,
        data: Vec<u8>,
        dts: u64,
        duration: u32,
        keyframe: bool,
    ) -> crate::Result<()> {
        let mut packet = Packet::new(stream, time_base, data);
        packet.dts = Some(dts as i64);
        packet.duration = Some(i64::from(duration));
        packet.flags.keyframe = keyframe;
        self.writer.write_packet(&packet).map_err(mp4_error)
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
                // Sound with no picture to wait for is written as it arrives.
                (None, Some(_)) if !self.has_video => self.flush_audio_front()?,
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
        let time_base = TimeBase::new(1, i64::from(self.timescale));
        let dts = self.flushed_video_ticks;
        // No B-frames on either encode path, so decode and presentation are one.
        self.put(
            VIDEO_STREAM,
            time_base,
            bytes,
            dts,
            self.frame_duration,
            is_sync,
        )?;
        self.frames += 1;
        self.flushed_video_ticks += u64::from(self.frame_duration);
        Ok(())
    }

    fn flush_audio_front(&mut self) -> crate::Result<()> {
        let Some(bytes) = self.audio_queue.pop_front() else {
            return Ok(());
        };
        let time_base = TimeBase::from_rate(self.audio_rate);
        let dts = self.audio_ticks;
        // Every AAC packet is a sync point. Saying so per sample is what keeps
        // the track free of an `stss`, which means the same thing.
        self.put(self.audio_stream, time_base, bytes, dts, AAC_PACKET_SAMPLES, true)?;
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
    /// Queues rather than writes straight through, so a caller feeding packets
    /// while the picture is still running ([`crate::export::run`]) gets them
    /// woven into the file under the pictures still queued in
    /// [`hold`](Mp4Muxer::hold).
    pub fn write_audio_packet(&mut self, bytes: &[u8]) -> crate::Result<()> {
        if !self.has_audio {
            return Err("audio packet written to a video-only file".into());
        }
        self.audio_queue.push_back(bytes.to_vec());
        self.drain()
    }

    /// The soft subtitle tracks this file carries, written after the picture and
    /// the sound -- one `tx3g` timed-text track per [`SubParams`], which is the
    /// text an mp4 says a cue with and the one `ffmpeg` calls `mov_text`. Called
    /// once, before [`finish`](Mp4Muxer::finish); an empty list writes nothing.
    ///
    /// The samples are *continuous*: a timed-text track states one sample per
    /// instant of the film and nothing between them, so the stretches with
    /// nothing to say are empty samples (a 16-bit zero length and no text) and
    /// not holes -- a hole is where a player keeps the last line on screen until
    /// the next one arrives. The run is built out of the cue *boundaries*, so
    /// cues that overlap are one sample of both their lines rather than one of
    /// them dropped.
    ///
    /// corner-cut: the `tx3g` sample entry is written with `ec-mp4`'s default
    /// style and no `ftab` font table.
    pub fn write_subtitles(&mut self, subs: &[SubParams]) -> crate::Result<()> {
        if subs.is_empty() {
            return Ok(());
        }
        // `self.frames` counts only pictures already handed to the writer, and
        // the interleave can still be holding some back: drained first, or a
        // text track written under a short hold would end short of the picture.
        self.drain_all()?;
        // Where the picture ends, in the text track's own tick: the last sample
        // runs out to it, so the track covers the film rather than stopping at
        // the last thing anybody says.
        let end = (self.frames * u64::from(self.frame_duration) * u64::from(SUB_TIMESCALE)
            / u64::from(self.timescale)) as i64;
        for sub in subs {
            let mut params = CodecParameters::new(CodecId::Tx3g);
            params.extradata = Some(Buf::from(TX3G_STYLE.to_vec()));
            params.media = MediaParameters::Subtitle;
            let mut info = StreamInfo::new(
                self.streams,
                TimeBase::from_rate(SUB_TIMESCALE),
                params,
            );
            info.language = Some(mp4_language(&sub.language));
            let stream = self.add_stream(info)?;
            if !sub.name.is_empty() {
                self.writer
                    .set_track_title(stream, sub.name.clone())
                    .map_err(mp4_error)?;
            }
            let mut dts = 0u64;
            for (bytes, duration) in timed_text(&sub.cues, end)? {
                // Every text sample is one a player may start at (no `stss`).
                self.put(
                    stream,
                    TimeBase::from_rate(SUB_TIMESCALE),
                    bytes,
                    dts,
                    duration,
                    true,
                )?;
                dts += u64::from(duration);
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> crate::Result<()> {
        // Whatever the hold and the audio queue still carry: nothing further
        // is coming for either to wait on, so this drains both fully.
        self.drain_all()?;
        if self.has_video {
            // Refused by name rather than written: an mp4 whose picture track holds
            // no sample is a file that opens on a blank window with nothing said.
            if self.frames == 0 {
                return Err("no frames were written to the mp4 file".into());
            }
            self.writer
                .set_color(VIDEO_STREAM, self.colour)
                .map_err(mp4_error)?;
            self.writer
                .set_content_light(VIDEO_STREAM, self.light)
                .map_err(mp4_error)?;
        }
        if self.has_audio {
            if self.has_video {
                // The sound plays for as long as the picture does, not to the end of
                // its own packets: the copied AAC can run a packet past the picture.
                // `picture_seconds * rate`, computed in whole ticks and rounded.
                let picture_ticks = self.frames * u64::from(self.frame_duration);
                let rate = u64::from(self.audio_rate);
                let timescale = u64::from(self.timescale);
                let picture_samples = (picture_ticks * rate * 2 + timescale) / (timescale * 2);
                let picture_samples = u32::try_from(picture_samples)
                    .map_err(|_| "the picture is too long for an mp4 audio edit")?;
                self.writer
                    .set_edit(self.audio_stream, AAC_PRIMING, picture_samples)
                    .map_err(mp4_error)?;
            } else if let Some(frames) = self.audio_frames {
                // The sound plays for exactly the samples the encoder was fed, and
                // no further: the last packet's padding and the priming's own tail
                // past the end are not sound, so the edit states the count rather
                // than leaving the packets' sum to say it.
                let frames = u32::try_from(frames)
                    .map_err(|_| "the sound is too long for an mp4 audio edit")?;
                self.writer
                    .set_edit(self.audio_stream, AAC_PRIMING, frames)
                    .map_err(mp4_error)?;
            } else {
                self.writer
                    .set_audio_delay(self.audio_stream, AAC_PRIMING)
                    .map_err(mp4_error)?;
            }
        }
        let mut writer = self.writer;
        ec_core::Muxer::finish(&mut writer).map_err(mp4_error)?;
        // Not a `drop`: a `BufWriter` swallows the error of flushing on drop.
        writer.into_inner().flush()?;
        Ok(())
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
// How the copied picture is drawn: the same elements [`crate::demux`] reads
// back. `DisplayUnit` 4 spells the pair as the sample aspect ratio itself and
// 3 as a display aspect ratio; this writer says SAR (unit 4) because that is
// the ratio it was handed ([`crate::demux::PixelAspect`]) and the one a
// round-trip through this engine's own reader reproduces exactly. Square --
// the default these elements are optional for -- writes none of them, which is
// what keeps every existing copy's header byte-identical.
const DISPLAY_WIDTH: u32 = 0x54B0;
const DISPLAY_HEIGHT: u32 = 0x54BA;
const DISPLAY_UNIT: u32 = 0x54B2;
const DISPLAY_UNIT_SAR: u64 = 4;
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
const CHAPTERS: u32 = 0x1043_A770;
const EDITION_ENTRY: u32 = 0x45B9;
const EDITION_FLAG_DEFAULT: u32 = 0x45DB;
const CHAPTER_ATOM: u32 = 0xB6;
const CHAPTER_UID: u32 = 0x73C4;
const CHAPTER_TIME_START: u32 = 0x91;
const CHAPTER_TIME_END: u32 = 0x92;
const CHAPTER_DISPLAY: u32 = 0x80;
const CHAP_STRING: u32 = 0x85;
const CHAP_LANGUAGE: u32 = 0x437C;

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
/// ...and on how far a block's timestamp may sit from its cluster's, *either*
/// way: the field is a signed 16-bit millisecond count, and a copy's backwards
/// steps over a group of pictures are the direction nothing else bounds.
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

/// [`AV1C_HEAD`] for a track at 10 bits: the same record with `high_bitdepth` set,
/// which is the bit a reader takes the track's depth from (`parse_av1c`).
fn av1c_head(ten_bit: bool) -> [u8; 4] {
    let mut head = AV1C_HEAD;
    if ten_bit {
        head[2] |= 0x40; // high_bitdepth; the 8-bit record leaves it clear
    }
    head
}

/// What an AV1 Matroska track declares. `config` is the sequence header OBU,
/// which is [`av1_sequence_header`]'s answer for the first coded keyframe.
pub struct Av1Params<'a> {
    /// Whether the sequence header is 10-bit; the `av1C` record says so too.
    pub ten_bit: bool,
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub config: &'a [u8],
    /// The track's language as a caller names it (`und` when it names none),
    /// written as the Matroska `Language` of the track ([`track_language`]).
    pub language: Option<String>,
    /// The colour the track declares, when it is not the output rule's (a 10-bit
    /// export of an SDR source states its source's matrix). `None` is the rule.
    pub colour: Option<Tags>,
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
    /// The track's language as a caller names it (`und` when it names none),
    /// written as the Matroska `Language` of the track ([`track_language`]).
    pub language: Option<String>,
    /// The colour the Matroska `Colour` element declares, where it is not the
    /// output rule's (an HDR export: BT.2020 and PQ or HLG). `None` keeps the rule.
    pub colour: Option<Tags>,
    /// The HDR static metadata the `Colour` element carries beside it
    /// (MaxCLL/MaxFALL and the `MasteringMetadata`), or the mp4's `mdcv`/`clli`.
    /// `None` writes nothing, which is every SDR file's header exactly as it was.
    pub light: Option<ec_core::ContentLight>,
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
    /// The track's language as a caller names it (`und` when it names none),
    /// written as the Matroska `Language` of the track ([`track_language`]).
    pub language: Option<String>,
    /// The source's own `Colour`: a copied HDR film leaves as the HDR film it
    /// is, because not one of its samples was touched.
    ///
    /// corner-cut: the `MasteringMetadata` and MaxCLL beside it are not carried --
    /// this engine parses them ([`crate::demux::ContentLight`]) but has nowhere
    /// to write them yet, so a copied HDR file states its curve and not its
    /// grading display. Upgrade path is a `Colour` element written from that
    /// same struct.
    pub colour: Tags,
    /// The ratio one of the copied samples' pixels is drawn at, written as the
    /// track's `Display*` elements: the aspect is metadata about the samples,
    /// which are the source's own, so it travels instead of being baked in --
    /// ffmpeg's own copy behaviour. [`PixelAspect::SQUARE`] writes nothing,
    /// which is every square-pixel file's header exactly as it was.
    pub pixel_aspect: crate::demux::PixelAspect,
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
    ///
    /// Matroska carries whatever bytes this holds, and a file may state a
    /// BCP-47 tag in them (`pt-BR`); an mp4 has one 16-bit field of exactly
    /// three ISO 639-2 letters, so [`Mp4Muxer::write_subtitles`] turns a tag
    /// into a code -- or into `und`, where nothing names it -- rather than
    /// letting the container mangle it.
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
    /// Where the last chapter's `ChapterTimeEnd` sits in the file, and that chapter's start in ns,
    /// once [`set_chapters`](MkvMuxer::set_chapters) has written them. The film's end is patched
    /// into the first at [`finish`](MkvMuxer::finish).
    chapter_end: Option<(u64, u64)>,
    /// Whether the first cluster has been opened: chapters must be set before it.
    clusters_started: bool,
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
        let mut av1c = av1c_head(video.ten_bit).to_vec();
        av1c.extend_from_slice(video.config);
        Self::open(
            path,
            video.width,
            video.height,
            video.frame_rate,
            b"V_AV1",
            &av1c,
            video.colour.unwrap_or_else(|| Tags::of(ColorDescription::output(video.height))),
            crate::demux::PixelAspect::SQUARE,
            audio,
            subs,
            false,
            video.language.as_deref(),
            None,
        )
    }

    /// The same AV1 track as [`MkvMuxer::create`] with no subtitle track, in a
    /// WebM file: the `webm` DocType, which is the one thing that differs.
    pub fn create_webm(
        path: &Path,
        video: &Av1Params,
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
    ) -> crate::Result<Self> {
        if video.config.is_empty() {
            return Err("no AV1 sequence header for the track's CodecPrivate".into());
        }
        let mut av1c = av1c_head(video.ten_bit).to_vec();
        av1c.extend_from_slice(video.config);
        Self::open(
            path,
            video.width,
            video.height,
            video.frame_rate,
            b"V_AV1",
            &av1c,
            Tags::of(ColorDescription::output_sized(video.width, video.height)),
            crate::demux::PixelAspect::SQUARE,
            audio,
            Vec::new(),
            true,
            video.language.as_deref(),
            None,
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
            video.colour.unwrap_or_else(|| Tags::of(ColorDescription::output(video.height))),
            crate::demux::PixelAspect::SQUARE,
            audio,
            subs,
            false,
            video.language.as_deref(),
            video.light,
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
            video.pixel_aspect,
            audio,
            subs,
            false,
            video.language.as_deref(),
            None,
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
        colour: Tags,
        pixel_aspect: crate::demux::PixelAspect,
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
        subs: Vec<SubParams>,
        webm: bool,
        language: Option<&str>,
        light: Option<ec_core::ContentLight>,
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
        elem(&mut ebml, DOC_TYPE, if webm { b"webm" } else { b"matroska" });
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
        track_language(&mut entry, language);
        elem(&mut entry, CODEC_PRIVATE, codec_private);
        uint(&mut entry, DEFAULT_DURATION, frame_ns);
        let mut dims = Vec::new();
        uint(&mut dims, PIXEL_WIDTH, u64::from(width));
        uint(&mut dims, PIXEL_HEIGHT, u64::from(height));
        // The copied samples' aspect, where it is not square: the same three
        // elements this engine's own reader asks a Matroska track for, as a
        // sample aspect ratio (unit 4). A square copy writes none of them --
        // exactly the header every square-pixel copy wrote before this.
        if !pixel_aspect.is_square() {
            uint(&mut dims, DISPLAY_WIDTH, u64::from(pixel_aspect.h));
            uint(&mut dims, DISPLAY_HEIGHT, u64::from(pixel_aspect.v));
            uint(&mut dims, DISPLAY_UNIT, DISPLAY_UNIT_SAR);
        }
        // What the samples in those pixels mean. Written rather than left to a
        // reader's own 720-line guess -- the guess is right for an encoded file
        // (the export remaps every clip into exactly that space) but a remuxer
        // or a scaler downstream has no reason to make it, and an untagged file
        // is how a 601 source ends up displayed as 709. A *copied* track states
        // its source's space instead, which is the one its samples are in.
        let mut tags = Vec::new();
        uint(&mut tags, MATRIX_COEFFICIENTS, u64::from(colour.matrix_code.unwrap_or(2)));
        // Matroska says the range as a code, not a flag: 1 limited, 2 full.
        uint(&mut tags, RANGE, 1 + u64::from(colour.full_range.unwrap_or(false)));
        uint(&mut tags, TRANSFER_CHARACTERISTICS, u64::from(colour.transfer_code.unwrap_or(2)));
        uint(&mut tags, PRIMARIES, u64::from(colour.primaries.unwrap_or(2)));
        if let Some(light) = light {
            write_light(&mut tags, light);
        }
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
            chapter_end: None,
            clusters_started: false,
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
    /// here is exactly what its source said -- and a step back wider than that
    /// signed field reaches opens a cluster of its own rather than being
    /// truncated into an instant nobody asked for ([`Self::cluster_holds`]).
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
        // timestamp reaches, either way.
        if self.cluster.is_empty()
            || key
            || self.cluster.len() >= CLUSTER_BYTES
            || !self.cluster_holds(ts)
        {
            self.open_cluster(ts)?;
        }
        // The sound this picture plays under goes in first, into the cluster the
        // picture opened: a player reads the two together. The cue over it with
        // them, for the same reason.
        self.drain_audio(ts)?;
        self.drain_subs(ts)?;
        // ...unless a track lagged this picture by more than a cluster reaches
        // and opened one of its own behind it, in which case the picture gets a
        // cluster named after its own instant rather than a distance its block
        // header cannot state.
        if !self.cluster_holds(ts) {
            self.open_cluster(ts)?;
        }
        self.block(1, ts, key, payload)?;
        self.frames += 1;
        self.last_ts = self.last_ts.max(ts);
        Ok(())
    }

    /// Whether the open cluster can hold a block at `ts`. A cluster is buffered
    /// whole and its blocks are timed *relative* to it, in a signed 16-bit
    /// millisecond field ([`CLUSTER_MS`]), so a block further from the cluster's
    /// own timestamp than that -- in **either** direction -- needs a cluster of
    /// its own. The backwards one is the side a copy really takes: a stream with
    /// B-frames is stored in decode order, so its presentation times step back
    /// over a group of pictures.
    ///
    /// This is what keeps every distance written inside the field, which is what
    /// makes [`Self::relative_ms`]'s checked cast a guard of last resort rather
    /// than the place a real block lands.
    fn cluster_holds(&self, ts: i64) -> bool {
        !self.cluster.is_empty() && (ts - self.cluster_ts).unsigned_abs() < CLUSTER_MS as u64
    }

    /// Opens a cluster stating `ts`, closing whatever was open: a cluster's size
    /// is a header field, so it is written whole or not at all.
    fn open_cluster(&mut self, ts: i64) -> crate::Result<()> {
        self.clusters_started = true;
        self.flush()?;
        self.cluster_ts = ts;
        uint(&mut self.cluster, CLUSTER_TIMESTAMP, ts as u64);
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
            // A cluster of its own where the sound has run further from the open
            // one than a 16-bit relative timestamp reaches, either way.
            if !self.cluster_holds(ts) {
                self.open_cluster(ts)?;
            }
            self.block(2, ts, true, &bytes)?;
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
                // A cluster of its own where the text has run further from the
                // open one than a 16-bit relative timestamp reaches, either way,
                // exactly as the sound's drain does.
                if !self.cluster_holds(ts) {
                    self.open_cluster(ts)?;
                }
                self.block_group(track_no, ts, ms, text.as_bytes())?;
            }
        }
        Ok(())
    }

    /// One `SimpleBlock` of `track`, timed against the open cluster. Every AAC
    /// packet is a keyframe; a picture is one when the encoder said so.
    fn block(&mut self, track: u8, ts: i64, key: bool, payload: &[u8]) -> crate::Result<()> {
        let relative = self.relative_ms(ts)?;
        let mut block = Vec::with_capacity(payload.len() + 4);
        block.push(0x80 | track); // the track number as a one-byte EBML integer
        block.extend_from_slice(&relative);
        block.push(if key { 0x80 } else { 0 });
        block.extend_from_slice(payload);
        elem(&mut self.cluster, SIMPLE_BLOCK, &block);
        Ok(())
    }

    /// A block's own timestamp less its cluster's, in the signed 16-bit
    /// milliseconds the block header carries.
    ///
    /// Checked and not cast: a distance that does not fit is a block at an
    /// instant nobody asked for -- `as i16` of a step back wider than the field
    /// lands as a *positive* offset, 65.536 s past its cluster -- and the error
    /// names the distance instead. Both callers keep the open cluster within
    /// [`CLUSTER_MS`] of every block they write ([`Self::cluster_holds`]), so
    /// this is the guard of last resort rather than a case a file reaches.
    fn relative_ms(&self, ts: i64) -> crate::Result<[u8; 2]> {
        let ms = ts - self.cluster_ts;
        i16::try_from(ms).map(i16::to_be_bytes).map_err(|_| {
            format!(
                "a block {ms} ms from its cluster's timestamp does not fit the 16-bit \
                 relative field"
            )
            .into()
        })
    }

    /// One `BlockGroup`: a `Block` and the `BlockDuration` beside it. That pair
    /// is how a cue says when it goes away -- a `SimpleBlock` has nowhere to put
    /// a duration, and a subtitle without one stays up until whatever a player
    /// decides, which is what a subtitle must never do.
    fn block_group(
        &mut self,
        track: u8,
        ts: i64,
        duration_ms: u64,
        payload: &[u8],
    ) -> crate::Result<()> {
        let relative = self.relative_ms(ts)?;
        let mut block = Vec::with_capacity(payload.len() + 4);
        block.push(0x80 | track); // the track number as a one-byte EBML integer
        block.extend_from_slice(&relative);
        // No flags a text block can carry: not lacing, and "keyframe" is a
        // `SimpleBlock` field that a plain `Block` does not have at all.
        block.push(0);
        block.extend_from_slice(payload);
        let mut group = Vec::new();
        elem(&mut group, BLOCK, &block);
        uint(&mut group, BLOCK_DURATION, duration_ms);
        elem(&mut self.cluster, BLOCK_GROUP, &group);
        Ok(())
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
    /// The chapter marks, `(start in ns, title)`, written as a `Chapters` element straight after
    /// the header, where a reader finds it without a SeekHead: before the first cluster. Sorted by
    /// start. Each chapter ends at the next one's start, and the last at the film's end, which is
    /// written at [`finish`](MkvMuxer::finish). Empty writes nothing. Must be called before any
    /// picture or sound is written, and once.
    pub fn set_chapters(&mut self, chapters: &[(u64, String)]) -> crate::Result<()> {
        if chapters.is_empty() {
            return Ok(());
        }
        if self.clusters_started {
            return Err("the chapters must be set before the first picture or sound".into());
        }
        if self.chapter_end.is_some() {
            return Err("the chapters are already set".into());
        }
        let mut sorted = chapters.to_vec();
        sorted.sort_by_key(|(start, _)| *start);
        let at = self.file.stream_position()?;
        let (element, end_offset) = chapters_element(&sorted);
        self.file.write_all(&element)?;
        let last_start = sorted.last().map_or(0, |(start, _)| *start);
        self.chapter_end = Some((at + end_offset as u64, last_start));
        Ok(())
    }

    pub fn finish(mut self) -> crate::Result<()> {
        // Whatever sound outlasts the last picture, before the last cluster is
        // closed: a track cut short here is a file that goes silent at the end.
        self.drain_audio(i64::MAX)?;
        self.drain_subs(i64::MAX)?;
        self.flush()?;
        if self.frames == 0 {
            return Err("no frames were written to the Matroska file".into());
        }
        // One frame past the last picture shown, which is where the file really
        // ends: counting the frames instead would cut a copied track short by
        // however many pictures a cut left the one before it on screen for.
        let ms = (self.frames as f64 * self.frame_ns as f64 / TIMESTAMP_SCALE_NS as f64)
            .max(self.last_ts as f64 + self.frame_ns as f64 / TIMESTAMP_SCALE_NS as f64);
        // The last chapter ends where the film does: the same instant the duration states.
        if let Some((at, last_start)) = self.chapter_end {
            let film_ns = ((ms * TIMESTAMP_SCALE_NS as f64).round() as u64).max(last_start);
            let tail = self.file.stream_position()?;
            self.file.seek(SeekFrom::Start(at))?;
            self.file.write_all(&film_ns.to_be_bytes())?;
            self.file.seek(SeekFrom::Start(tail))?;
        }
        let end = self.file.stream_position()?;
        let body = end - (self.segment_size_at + 8);
        // The reserved 8-byte size: marker bit in the top byte, value under it.
        let size = (1u64 << 56) | body;
        self.file.seek(SeekFrom::Start(self.segment_size_at))?;
        self.file.write_all(&size.to_be_bytes())?;
        self.file.seek(SeekFrom::Start(self.duration_at))?;
        self.file.write_all(&ms.to_be_bytes())?;
        self.file.flush()?;
        Ok(())
    }
}

/// The Matroska `Chapters` element: one edition with a `ChapterAtom` per chapter. Every time is
/// eight bytes, so the element never changes size: each chapter ends at the next one's start, and
/// the last one's end is left at zero to be patched with the film's end. Returns the element and the
/// offset of that last `ChapterTimeEnd` payload within it.
fn chapters_element(chapters: &[(u64, String)]) -> (Vec<u8>, usize) {
    let mut edition = Vec::new();
    uint(&mut edition, EDITION_FLAG_DEFAULT, 1);
    let mut last_end = 0;
    for (i, (start, title)) in chapters.iter().enumerate() {
        let end = match chapters.get(i + 1) {
            Some((next, _)) => (*next).max(*start),
            None => 0,
        };
        let mut display = Vec::new();
        elem(&mut display, CHAP_STRING, title.as_bytes());
        elem(&mut display, CHAP_LANGUAGE, b"und");
        let mut atom = Vec::new();
        elem(&mut atom, CHAPTER_UID, &(i as u64 + 1).to_be_bytes());
        elem(&mut atom, CHAPTER_TIME_START, &start.to_be_bytes());
        let end_in_atom = atom.len() + elem_head_len(CHAPTER_TIME_END, 8);
        elem(&mut atom, CHAPTER_TIME_END, &end.to_be_bytes());
        elem(&mut atom, CHAPTER_DISPLAY, &display);
        last_end = edition.len() + elem_head_len(CHAPTER_ATOM, atom.len()) + end_in_atom;
        elem(&mut edition, CHAPTER_ATOM, &atom);
    }
    let mut entries = Vec::new();
    elem(&mut entries, EDITION_ENTRY, &edition);
    let mut out = Vec::new();
    elem(&mut out, CHAPTERS, &entries);
    let offset = elem_head_len(CHAPTERS, entries.len())
        + elem_head_len(EDITION_ENTRY, edition.len())
        + last_end;
    (out, offset)
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
    track_language(&mut entry, audio.language.as_deref());
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

/// The `Language` a Matroska track states: the one it is given, or `und` where
/// it is given none. A track with no `Language` is `eng` to every reader (the
/// spec's default), so an undetermined source must say `und` rather than leave
/// the element out. The code is written as given, as the subtitle track does.
fn track_language(entry: &mut Vec<u8>, language: Option<&str>) {
    let language = language.filter(|l| !l.is_empty()).unwrap_or("und");
    elem(entry, TRACK_LANGUAGE, language.as_bytes());
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
    track_language(&mut entry, audio.language.as_deref());
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
/// multi-language film used to export as several English ones. `Language` is
/// stated as `und` where the track names none; `Name` is left out where it is
/// empty. [`crate::subtitle::of_matroska`] reads exactly this back, so a track
/// exported and re-imported keeps what it had.
fn subtitle_track_entry(language: &str, name: &str, track_no: u8) -> Vec<u8> {
    let mut entry = Vec::new();
    uint(&mut entry, TRACK_NUMBER, u64::from(track_no));
    uint(&mut entry, TRACK_UID, u64::from(track_no));
    uint(&mut entry, TRACK_TYPE, 0x11);
    uint(&mut entry, FLAG_LACING, 0);
    elem(&mut entry, CODEC_ID, b"S_TEXT/UTF8");
    // Always stated: a track with no `Language` is `eng` to every reader, so a
    // subtitle the source names no language for says `und`.
    track_language(&mut entry, Some(language));
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

/// The `Colour` children that state HDR static metadata, in ffmpeg's order:
/// MaxCLL, MaxFALL, then the `MasteringMetadata` master. Chromaticities are
/// 8-byte floats (the 0.00002 steps over 50000), luminance in nits rounded to
/// the 0.0001 the SEI carries.
fn write_light(tags: &mut Vec<u8>, l: ec_core::ContentLight) {
    if let Some(v) = l.max_cll {
        uint(tags, 0x55BC, v.round() as u64);
    }
    if let Some(v) = l.max_fall {
        uint(tags, 0x55BD, v.round() as u64);
    }
    let mut m = Vec::new();
    let chroma = |o: &mut Vec<u8>, id: u32, raw: u16| elem(o, id, &(f64::from(raw) / 50_000.0).to_be_bytes());
    if let Some(p) = l.mastering_primaries {
        chroma(&mut m, 0x55D1, p.red.0);
        chroma(&mut m, 0x55D2, p.red.1);
        chroma(&mut m, 0x55D3, p.green.0);
        chroma(&mut m, 0x55D4, p.green.1);
        chroma(&mut m, 0x55D5, p.blue.0);
        chroma(&mut m, 0x55D6, p.blue.1);
        chroma(&mut m, 0x55D7, p.white.0);
        chroma(&mut m, 0x55D8, p.white.1);
    }
    let nits = |v: f32| ((f64::from(v) * 1e4).round() / 1e4).to_be_bytes();
    if let Some(v) = l.mastering_max {
        elem(&mut m, 0x55D9, &nits(v));
    }
    if let Some(v) = l.mastering_min {
        elem(&mut m, 0x55DA, &nits(v));
    }
    if !m.is_empty() {
        elem(tags, 0x55D0, &m);
    }
}

/// `HdrOutput` in ec-core's units (nits; chromaticities in 0.00002 steps).
pub(crate) fn hdr_light(hdr: &crate::export::HdrOutput) -> ec_core::ContentLight {
    ec_core::ContentLight {
        max_cll: hdr.max_cll.map(f32::from),
        max_fall: hdr.max_fall.map(f32::from),
        mastering_max: hdr.mastering_display.map(|m| m.max_luminance as f32 / 10_000.0),
        mastering_min: hdr.mastering_display.map(|m| m.min_luminance as f32 / 10_000.0),
        mastering_primaries: hdr.mastering_display.map(|m| ec_core::MasteringPrimaries {
            green: m.primaries[0],
            blue: m.primaries[1],
            red: m.primaries[2],
            white: m.white_point,
        }),
    }
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
    hvcc_record_at(annex_b, false)
}

/// [`hvcc_record`] at either depth: `ten_bit` writes `bit_depth_luma_minus8` and
/// `bit_depth_chroma_minus8` as 2, which is what a Main 10 stream declares. The
/// profile itself is the SPS's own, copied with the rest of the PTL.
pub fn hvcc_record_at(annex_b: &[u8], ten_bit: bool) -> Option<Vec<u8>> {
    let nals = split_annex_b(annex_b);
    let of = |kind: u8| nals.iter().copied().find(|n| hevc_nal_type(n) == kind);
    let (vps, sps) = (of(HEVC_VPS)?, of(HEVC_SPS)?);
    // Every picture parameter set the first unit carries: an I+P stream's P
    // pictures refer to a second one, and samples drop theirs, so this record is
    // all a decoder has of it.
    let ppss: Vec<&[u8]> = nals
        .iter()
        .copied()
        .filter(|n| hevc_nal_type(n) == HEVC_PPS)
        .collect();
    if ppss.is_empty() {
        return None;
    }
    // The SPS as a decoder reads it: the emulation-prevention bytes the encoder
    // escaped it with are not part of the syntax, and a run of zeros in the
    // constraint flags is exactly where one lands.
    let rbsp = unescape(sps.get(2..)?);
    let mut rec = vec![1u8]; // configurationVersion
    rec.extend_from_slice(rbsp.get(1..13)?); // profile_tier_level, verbatim
    rec.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc = 0
    rec.push(0xFC); // parallelismType = 0 (unknown)
    rec.push(0xFC | 1); // chroma_format_idc = 1 (4:2:0)
    let depth = if ten_bit { 0xFA } else { 0xF8 }; // minus8 = 2 at 10 bits, 0 at 8
    rec.push(depth); // bit_depth_luma_minus8
    rec.push(depth); // bit_depth_chroma_minus8
    rec.extend_from_slice(&0u16.to_be_bytes()); // avgFrameRate: unstated
                                                // constantFrameRate 0, numTemporalLayers 1, temporalIdNested 1,
                                                // lengthSizeMinusOne 3 -- the same 4-byte prefix `annex_b_to_hvcc` writes.
    rec.push(0b0000_1111);
    rec.push(3); // numOfArrays
    for (kind, units) in [(HEVC_VPS, vec![vps]), (HEVC_SPS, vec![sps]), (HEVC_PPS, ppss)] {
        rec.push(0x80 | kind); // array_completeness = 1, NAL_unit_type
        rec.extend_from_slice(&(units.len() as u16).to_be_bytes()); // numNalus
        for nal in units {
            rec.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            rec.extend_from_slice(nal);
        }
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

    use crate::scratch::Scratch;

    use super::*;
    use ec_core::Demuxer as _;

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
            words: Vec::new(),
            karaoke: Vec::new(),
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
        assert!(Mp4Muxer::create(
            &out,
            &VideoParams {
                frame_rate: 0.0,
                ..ok
            },
            None
        )
        .is_err());
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
                language: None,
            }),
        )
        .unwrap();
        for unit in &units {
            muxer.write_video_au(unit).unwrap();
        }
        muxer.write_audio_packet(&[0x21, 0x22, 0x23]).unwrap();
        muxer.write_audio_packet(&[0x24, 0x25]).unwrap();
        muxer.finish().unwrap();

        let mut reader = crate::mp4track::open(&out).unwrap();
        let (video_id, audio_id) = (VIDEO_STREAM, AUDIO_STREAM);
        assert_eq!(reader.samples(video_id).len(), 3);
        assert_eq!(reader.samples(audio_id).len(), 2);
        {
            let video = &reader.streams()[0];
            let ec_core::MediaParameters::Video(v) = &video.params.media else {
                panic!("the first stream is the picture");
            };
            assert_eq!((v.width, v.height), (1280, 720));
            let fps = v.frame_rate.unwrap();
            assert_eq!((fps.num() as f64 / fps.den() as f64).round(), 30.0);
            let avcc = video.params.extradata.as_deref().unwrap();
            assert!(avcc.windows(SPS.len()).any(|w| w == SPS), "SPS in avcC");
            assert!(avcc.windows(PPS.len()).any(|w| w == PPS), "PPS in avcC");
        }
        let first = crate::mp4track::sample(&mut reader, video_id, 1).unwrap().unwrap();
        assert!(first.flags.keyframe, "IDR unit is a sync sample");
        assert_eq!(first.duration, Some(3000), "90000 / 30 fps");
        assert_eq!(&first.data[..], [0, 0, 0, 4, 0x65, 0x01, 0x02, 0x03]);
        assert!(!crate::mp4track::sample(&mut reader, video_id, 2).unwrap().unwrap().flags.keyframe);
        assert_eq!(
            &crate::mp4track::sample(&mut reader, audio_id, 1).unwrap().unwrap().data[..],
            [0x21, 0x22, 0x23],
            "audio packets copied verbatim"
        );
        let second = crate::mp4track::sample(&mut reader, audio_id, 2).unwrap().unwrap();
        assert_eq!(&second.data[..], [0x24, 0x25], "verbatim");
        assert_eq!(
            second.duration,
            Some(i64::from(AAC_PACKET_SAMPLES)),
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
        let mut encoder = ec_aac::AacEncoder::new(ec_aac::AacEncoderConfig::default());
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
            colour: None,
                ten_bit: false,
                language: None,
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
                    language: None,
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

    /// The seat an export encodes with, at every width the Matroska muxer can
    /// carry: a track it encoded comes back through the project's own decode
    /// door -- symphonia's reader for mono and stereo, the family's own
    /// `ec-aac` decoder for 5.1, which is where the decode splits -- with its
    /// rate and loudness intact, no delivered channel silent. A width wider
    /// than a pair arrives folded to stereo ([`crate::audio::downmix`] at the
    /// decoder's door), so the 5.1 leg asserts the pair, not the six.
    #[test]
    fn a_seat_encoded_track_round_trips_at_mono_stereo_and_five_one_widths() {
        let sequence = obu(1, &[0x11, 0x22, 0x33]);
        let mut key = sequence.clone();
        key.extend_from_slice(&obu(6, &[0xAA; 8]));
        let inter = obu(6, &[0xBB; 5]);
        for (channels, name) in [(1u16, "mono"), (2, "stereo"), (6, "5.1")] {
            let packets = tone_packets(48_000, channels);
            let heard: usize = packets.iter().map(|p| p.samples as usize).sum();

            let out = Scratch::file("ve_mkv_seat_widths", "mkv");
            let mut muxer = MkvMuxer::create(
                &out,
                &Av1Params {
            colour: None,
                    ten_bit: false,
                    language: None,
                    width: 640,
                    height: 360,
                    frame_rate: 30.0,
                    config: av1_sequence_header(&key).unwrap(),
                },
                Some((
                    &AudioParams {
                        freq_index: 3, // 48000
                        chan_conf: channels as u8,
                        sample_rate: 48_000,
                        opus_pre_skip: None,
                        language: None,
                    },
                    packets,
                )),
                Vec::new(),
            )
            .unwrap();
            muxer.write_frame(&key, true).unwrap();
            for _ in 0..14 {
                muxer.write_frame(&inter, false).unwrap();
            }
            muxer.finish().unwrap();

            // What the door reports: the file's own width, folded to a pair
            // for anything wider -- the door's contract, not a defect.
            let want = channels.min(2);
            let probe = crate::audio::AudioSession::probe(&out, 0)
                .unwrap()
                .expect("the file has an audio track");
            assert_eq!(
                (probe.sample_rate, probe.channels),
                (48_000, want),
                "{name}: probed with the wrong shape"
            );
            let (audio, chunks) = crate::audio::AudioSession::open(&out)
                .unwrap()
                .unwrap_or_else(|| {
                    panic!("{name}: track decodes through the project's own reader")
                });
            assert_eq!(
                (audio.sample_rate, audio.channels),
                (48_000, want),
                "{name}: opened with the wrong shape"
            );
            let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
            let frames = samples.len() / usize::from(want);
            assert!(
                frames + 1024 >= heard && frames <= heard + 2048,
                "{name}: {frames} frames read back of {heard} written"
            );
            // Every delivered channel carries the tone -- a fold that lost a
            // side or a dead decoder would starve one of these.
            for c in 0..usize::from(want) {
                let n = samples.len() / usize::from(want);
                let rms = (samples
                    .iter()
                    .skip(c)
                    .step_by(usize::from(want))
                    .take(n)
                    .map(|s| s * s)
                    .sum::<f32>()
                    / n as f32)
                    .sqrt();
                assert!(
                    rms > 0.05,
                    "{name}: channel {c} came back silent (rms {rms})"
                );
            }
            std::fs::remove_file(&out).unwrap();
        }
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
            colour: None,
                ten_bit: false,
                language: None,
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
                language: None,
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
            colour: None,
                ten_bit: false,
                language: None,
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
        assert!(MkvMuxer::create(
            &out,
            &Av1Params {
            colour: None,
                ten_bit: false,
                language: None,
                width: 640,
                height: 360,
                frame_rate: 30.0,
                config: &[],
            },
            None,
            Vec::new(),
        )
        .is_err());
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

        let mut reader = crate::mp4track::open(&out).unwrap();
        assert_eq!(crate::mp4track::timescale(&reader, VIDEO_STREAM), 24_000);
        let first = crate::mp4track::sample(&mut reader, VIDEO_STREAM, 1).unwrap().unwrap();
        assert_eq!(first.duration, Some(1_001), "one frame is a whole 1001 ticks");

        let (meta, _) = crate::demux::Demuxer::open(&out).unwrap();
        assert!(
            (meta.frame_rate - NTSC).abs() < 1e-9,
            "read back as {} fps, not {NTSC}",
            meta.frame_rate
        );
        std::fs::remove_file(&out).unwrap();
    }

    /// A block further from its cluster's own timestamp than the 16-bit
    /// *signed* relative field reaches gets a cluster of its own, backwards as
    /// well as forwards: a copied stream is stored in decode order, so its
    /// presentation times step back over a group of pictures
    /// ([`MkvMuxer::write_block`]), and `as i16` of a distance that does not
    /// fit lands as a *positive* offset -- the third block here used to be
    /// written 25.536 s past the cluster that holds it, a picture at 65.536 s
    /// the source put at 0. What is asserted is what a reader sees: every block
    /// at the instant its own source said.
    #[test]
    fn a_block_far_behind_its_cluster_gets_one_rather_than_wrapping() {
        let sequence = obu(1, &[0x11, 0x22, 0x33]);
        let mut key = sequence.clone();
        key.extend_from_slice(&obu(6, &[0xAA; 8]));
        let inter = obu(6, &[0xBB; 5]);
        let out = Scratch::file("ve_mkv_backward", "mkv");
        let mut muxer = MkvMuxer::create(
            &out,
            &Av1Params {
            colour: None,
                ten_bit: false,
                language: None,
                width: 640,
                height: 360,
                frame_rate: 30.0,
                config: av1_sequence_header(&key).unwrap(),
            },
            None,
            Vec::new(),
        )
        .unwrap();
        // A keyframe opens the file at 0, the next picture is 40 s later --
        // past the forward bound, so a cluster of its own -- and the third is a
        // non-key block back at the start, which is the second one's reorder
        // depth and the direction nothing checked.
        muxer.write_block(&key, true, 0).unwrap();
        muxer.write_block(&inter, false, 40_000_000_000).unwrap();
        muxer.write_block(&inter, false, 0).unwrap();
        muxer.finish().unwrap();

        let (meta, demuxer) = crate::demux::Demuxer::open(&out).expect("reopen");
        assert_eq!(meta.frame_count, 3);
        let crate::demux::Demuxer::Mkv(mut demuxer) = demuxer else {
            panic!("an mkv file");
        };
        let times: Vec<i64> = (0..3)
            .map(|i| {
                demuxer
                    .coded_block(i)
                    .unwrap()
                    .unwrap_or_else(|| panic!("block {i}"))
                    .ts_ns
            })
            .collect();
        assert_eq!(
            times,
            [0, 40_000_000_000, 0],
            "each block at the instant its source said, not wrapped"
        );
        std::fs::remove_file(&out).unwrap();
    }

    /// A tag Matroska permits where an mp4 has one 16-bit field of three ISO
    /// 639-2 letters: `mp4`'s own `language_code` masks the three UTF-16 units
    /// it is handed with `0x1F` and drops the rest without a word
    /// (`vendor/mp4/src/mp4box/mdhd.rs:157`), so a `pt-BR` track used to be
    /// written as `ptm` -- a language nobody speaks, offered by a player's
    /// subtitle menu as the language the track is not in.
    #[test]
    fn a_tagged_subtitle_track_states_a_language_the_field_can_hold() {
        let out = Scratch::file("ve_mp4_language", "mp4");
        let mut muxer = Mp4Muxer::create(
            &out,
            &VideoParams {
                width: 640,
                height: 360,
                frame_rate: 30.0,
                sps: SPS,
                pps: PPS,
            },
            None,
        )
        .unwrap();
        // A second of picture, so the text track has a film to run over.
        for _ in 0..30 {
            muxer
                .write_video_au(&au(&[SPS, PPS, &[0x65, 0x01]]))
                .unwrap();
        }
        muxer
            .write_subtitles(&[SubParams {
                language: "pt-BR".into(),
                name: "Signs".into(),
                cues: vec![cue(0, 1_000, "hello")],
            }])
            .unwrap();
        muxer.finish().unwrap();

        let reader = crate::mp4track::open(&out).unwrap();
        let text = 2; // no audio track: the text is the track after the picture
        assert_eq!(
            crate::mp4track::language(&reader, text).as_deref(),
            Some("por"),
            "the tag's own language, not a mangling of it"
        );
        std::fs::remove_file(&out).unwrap();
    }

    /// The four shapes a language can arrive in, and what the container is
    /// handed for each: the code the field already holds, a tag cut to its
    /// primary subtag and mapped, and `und` -- never a string whose first three
    /// UTF-16 units the `mp4` crate would mask into a language nobody speaks.
    #[test]
    fn a_language_is_a_code_before_it_reaches_the_mp4_field() {
        assert_eq!(mp4_language("eng"), "eng", "three letters are the code");
        assert_eq!(mp4_language("tur"), "tur");
        assert_eq!(mp4_language("pt-BR"), "por", "the tag's primary subtag");
        assert_eq!(mp4_language("zh-Hans"), "zho");
        assert_eq!(mp4_language("en"), "eng", "two letters are mapped");
        assert_eq!(
            mp4_language("JA"),
            "jpn",
            "a tag's case is not its language"
        );
        assert_eq!(mp4_language("fil"), "fil", "639-3 is three letters too");
        assert_eq!(mp4_language(""), "und", "nothing stated, undetermined");
        assert_eq!(mp4_language("x-pig-latin"), "und", "a tag nothing maps");
    }

    /// A file nobody put a picture in is refused by name rather than written:
    /// [`MkvMuxer::finish`] has always refused the same way, and an mp4 whose
    /// picture track holds no sample comes out as a player's blank window with
    /// nothing said. One picture is all it takes to make the same file finish.
    #[test]
    fn an_mp4_with_no_picture_is_refused_by_name() {
        let params = VideoParams {
            width: 320,
            height: 240,
            frame_rate: 30.0,
            sps: SPS,
            pps: PPS,
        };
        let out = Scratch::file("ve_mp4_no_picture", "mp4");
        let err = Mp4Muxer::create(&out, &params, None)
            .unwrap()
            .finish()
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "no frames were written to the mp4 file",
            "the Matroska muxer's refusal, in this container's own name"
        );
        std::fs::remove_file(&out).unwrap();

        let out = Scratch::file("ve_mp4_one_picture", "mp4");
        let mut muxer = Mp4Muxer::create(&out, &params, None).unwrap();
        muxer
            .write_video_au(&au(&[SPS, PPS, &[0x65, 0x01]]))
            .unwrap();
        muxer.finish().unwrap();
        std::fs::remove_file(&out).unwrap();
    }
}
