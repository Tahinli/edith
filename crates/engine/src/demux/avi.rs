//! AVI -- `.avi`, `.divx` -- over the family's own [`ec_riff::AviDemuxer`], read by the very
//! readers a transport stream is ([`super::ts`]): [`ts::TsDemuxer`](super::ts::TsDemuxer) for
//! the picture, [`ts::TsAudio`](super::ts::TsAudio) for each sound track.
//!
//! [`AviSrc`] is the adapter: it presents the AVI as the 90 kHz packet stream the readers
//! expect (every stream's `dwScale/dwRate` time base rescaled to ticks, a seek rescaled
//! back), and its tracks as the [`ec_mpegts::TrackInfo`] list `ts::describe_open` walks.
//! Source time 0 is frame 0 (an AVI stream starts at 0).
//!
//! **Picture.** MJPEG, H.264 (Annex B), MPEG-2, ProRes, DNxHD, DV (type-2, where the sound is its
//! own stream, and type-1 `iavs`, where `ec-riff` splits the sound out of the frames), MPEG-4
//! Part 2 (XviD, DivX 4-6, FMP4...), MS-MPEG4 version 2 and 3 (`MP42`, `DIV3`...) and raw video
//! (`BI_RGB` 24 and 32 bit, YUY2, UYVY, I420, YV12). Refused by name with the ffmpeg line that
//! converts the file: MS-MPEG4 version 1 (`MPG4`, `DIV1`), other raw layouts (8, 15 and 16 bit
//! DIB, palettised, NV12, V210, YVYU...) and any other FourCC. A file whose picture is refused
//! but whose sound is usable still opens as sound (the audio door ignores the refused picture stream and names
//! nothing); asking for its picture fails with the refusal.
//!
//! **Sound.** PCM (8/16/24/32 bit integer, 32 bit float), MPEG layer II/III, AC-3 and AAC
//! (raw, with its `AudioSpecificConfig`). Any other `wFormatTag` is refused by name.
//!
//! **B-frames.** An AVI carries no timestamps: the packets are in decode order and the
//! demuxer's pts is the decode slot. The decoders release pictures in display order by
//! themselves, so a sequential decode is right; what a slot cannot say is the display index
//! of a keyframe that has pictures displayed before it *after* it in the file (an open-GOP
//! `I` followed by its leading `B`s). [`is_b_picture`] recognises those in the bitstream
//! (the run of B pictures right behind the keyframe), and the picture reader adds them to
//! the slot. Limit: a keyframe flagged by the index that is not the start of a GOP in this
//! sense and has no B pictures behind it is taken at its slot; and a dropped frame (a
//! zero-size video chunk) keeps its slot but yields no picture, so a file with those drifts
//! by that many frames after them.
//!
//! **Time.** 90 kHz ticks rounded from the stream's own rate: a sample count of a 96 kHz or
//! faster PCM stream may come back one sample off (a tick is 1.07 samples there).

use std::io::BufReader;

use ec_core::registry::SeekMode;
use ec_core::timebase::{Rounding, TimeBase, Timestamp};
use ec_core::{CodecId, MediaType, Packet, StreamInfo};
use ec_mpegts::{StreamKind as TsKind, TrackInfo};
use ec_riff::{AviDemuxer, AviTrackInfo};

use super::*;

type Reader = BufReader<File>;

/// The ticks a second is cut into, as [`super::ts`] counts.
fn tb() -> TimeBase {
    TimeBase::new(1, 90_000)
}

pub(crate) fn avi_error(e: ec_core::Error) -> crate::Error {
    match &e {
        // A refusal is a named gap with a way out; a damaged file is not.
        ec_core::Error::Unsupported { .. } => format!(
            "AVI: {e} — convert it with {}",
            super::ts::convert_line(super::ts::Flavor::Avi)
        )
        .into(),
        _ => format!("AVI: {e}").into(),
    }
}

/// Whether `path` is an AVI file: its first twelve bytes, or -- for a name that says so and
/// bytes that say nothing else -- its extension (`.avi`, `.divx`). A container whose magic
/// is Matroska's or an mp4's is never one.
pub fn is_avi(path: &Path) -> bool {
    if sniff_container(path).is_some() {
        return false;
    }
    let mut head = [0u8; 12];
    let read = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    if ec_riff::is_avi(&head[..read]) {
        return true;
    }
    // A name that says AVI over a file that is plainly something else (a transport stream, a
    // WAV) is not one; over bytes that say nothing, the open reports what is wrong.
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "avi" | "divx"))
        && !(read >= 4 && (&head[..4] == b"RIFF" || head[0] == 0x47))
}

/// An AVI as the packet stream [`super::ts::Src`] speaks.
pub(crate) struct AviSrc {
    d: AviDemuxer<Reader>,
    /// The streams in 90 kHz ticks.
    streams: Vec<StreamInfo>,
    /// Each stream's own time base, by stream index.
    native: Vec<TimeBase>,
    tracks: Vec<TrackInfo>,
}

fn refused_name(info: &AviTrackInfo) -> String {
    match info.media {
        Some(MediaType::Audio) => {
            let tag = u16::from_le_bytes([info.tag[0], info.tag[1]]);
            match tag {
                0x0160 => "wmav1".into(),
                0x0161 => "wmav2".into(),
                0x0162 => "wmapro".into(),
                0x0002 => "adpcm_ms".into(),
                0x0011 => "adpcm_ima_wav".into(),
                0x0055 => "mp3".into(),
                0x2001 => "dts".into(),
                0x00FF => "aac".into(),
                _ => format!("wav_{tag:04x}"),
            }
        }
        _ => String::from_utf8_lossy(&info.tag).trim().to_ascii_lowercase(),
    }
}

/// What an AVI does not state, read off the first pictures: for H.264 the `avcC` record of its
/// first SPS and PPS, for MPEG-2 the sequence header with its extension. `None` where the
/// stream is neither or shows nothing in its first packets.
fn codec_config(d: &mut AviDemuxer<Reader>, stream: &StreamInfo) -> Option<Vec<u8>> {
    let codec = stream.params.codec;
    if !matches!(codec, CodecId::H264 | CodecId::Mpeg2Video) {
        return None;
    }
    d.rewind();
    let (mut sps, mut pps) = (None, None);
    // The strf tail, where a muxer that keeps the sets out of band (ffmpeg, from an mp4)
    // puts them: Annex B, or an `avcC` record already.
    if let Some(extra) = stream.params.extradata.as_ref().filter(|_| codec == CodecId::H264) {
        let extra: &[u8] = extra;
        if extra.first() == Some(&1) {
            return Some(extra.to_vec());
        }
        for nal in ec_mpegts::nal_units(extra) {
            match nal.first().map(|b| b & 0x1f) {
                Some(7) if sps.is_none() => sps = Some(nal.to_vec()),
                Some(8) if pps.is_none() => pps = Some(nal.to_vec()),
                _ => {}
            }
        }
    }
    for _ in 0..256 {
        if sps.is_some() && pps.is_some() {
            break;
        }
        let Ok(p) = d.next_packet() else { break };
        if p.stream != stream.index {
            continue;
        }
        let data: &[u8] = &p.data;
        if codec == CodecId::Mpeg2Video {
            let at = data.windows(4).position(|w| w == [0, 0, 1, 0xB3])?;
            let end = data[at + 4..].windows(4).position(|w| w == [0, 0, 1, 0x00]).map_or(data.len(), |e| at + 4 + e);
            return Some(data[at..end].to_vec());
        }
        for nal in ec_mpegts::nal_units(data) {
            match nal.first().map(|b| b & 0x1f) {
                Some(7) if sps.is_none() => sps = Some(nal.to_vec()),
                Some(8) if pps.is_none() => pps = Some(nal.to_vec()),
                _ => {}
            }
        }
        if sps.is_some() && pps.is_some() {
            break;
        }
    }
    let (sps, pps) = (sps?, pps?);
    let mut avcc = vec![1, *sps.get(1)?, *sps.get(2)?, *sps.get(3)?, 0xFF, 0xE1];
    avcc.extend((sps.len() as u16).to_be_bytes());
    avcc.extend(&sps);
    avcc.push(1);
    avcc.extend((pps.len() as u16).to_be_bytes());
    avcc.extend(&pps);
    Some(avcc)
}

/// ffprobe's name for a picture stream the demuxer refused, from its reason and FourCC.
fn refused_video_name(why: &str, tag: &[u8; 4]) -> String {
    if why.contains("uncompressed") {
        "rawvideo".into()
    } else if why.starts_with("DV video") {
        "dvvideo".into()
    } else if why.starts_with("MPEG-4 Part 2") || why.starts_with("MS-MPEG4") {
        match &tag.to_ascii_lowercase()[..] {
            b"div3" | b"div4" | b"div5" | b"div6" | b"mp43" | b"mpg4" => "msmpeg4v3".into(),
            b"div1" | b"mp41" => "msmpeg4v1".into(),
            b"div2" | b"mp42" => "msmpeg4v2".into(),
            _ => "mpeg4".into(),
        }
    } else {
        String::from_utf8_lossy(tag).trim().to_ascii_lowercase()
    }
}

impl AviSrc {
    pub(crate) fn open(path: &Path) -> crate::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::with_capacity(256 << 10, file);
        let d = AviDemuxer::new(reader).map_err(avi_error)?;
        let mut native = Vec::new();
        let mut d = d;
        let streams: Vec<StreamInfo> = d
            .streams()
            .iter()
            .map(|s| {
                native.push(s.time_base);
                let mut s = s.clone();
                let to = |t: i64| Timestamp::new(t, s.time_base).rescale(tb(), Rounding::Nearest).ticks;
                s.start_time = s.start_time.map(to);
                s.duration = s.duration.map(to);
                s.time_base = tb();
                s
            })
            .collect();
        let mut streams = streams;
        for stream in &mut streams {
            if let Some(extra) = codec_config(&mut d, stream) {
                stream.params.extradata = Some(extra.into());
            }
        }
        d.rewind();
        let tracks = d
            .tracks()
            .into_iter()
            .map(|t| {
                let params = t.stream.and_then(|i| streams.iter().find(|s| s.index == i));
                // A picture codec this build has no decode seat for (ec-riff reads more than the
                // engine routes) is refused by name here.
                let supported = |c: CodecId| {
                    matches!(
                        c,
                        CodecId::H264
                            | CodecId::Mpeg2Video
                            | CodecId::Mjpeg
                            | CodecId::ProRes
                            | CodecId::Dnxhd
                            | CodecId::DvVideo
                            | CodecId::Mpeg4Part2
                            | CodecId::MsMpeg4V2
                            | CodecId::MsMpeg4V3
                            | CodecId::RawVideo
                    )
                };
                let refused_picture = params
                    .map(|p| p.params.codec)
                    .filter(|c| c.media_type() == MediaType::Video && !supported(*c));
                let mut refusal = t.unsupported.clone();
                let mut stream = t.stream;
                let kind = match (t.media, params.map(|p| p.params.codec)) {
                    _ if refused_picture.is_some() => {
                        let c = refused_picture.unwrap_or(CodecId::H264);
                        refusal = Some(format!("{} video: no decoder in this build", c.name()));
                        stream = None;
                        TsKind::Other(format!("{}:{}", super::ts::REFUSED_VIDEO, c.name()))
                    }
                    (_, Some(CodecId::H264)) => TsKind::H264,
                    (_, Some(CodecId::Mpeg2Video)) => TsKind::Mpeg2Video,
                    (_, Some(CodecId::Mjpeg)) => TsKind::Other("mjpeg".into()),
                    (_, Some(CodecId::ProRes)) => TsKind::Other("prores".into()),
                    (_, Some(CodecId::Dnxhd)) => TsKind::Other("dnxhd".into()),
                    (_, Some(CodecId::DvVideo)) => TsKind::Other("dvvideo".into()),
                    // The FourCC rides in the kind: it picks the decoder's encoder workarounds.
                    (_, Some(CodecId::Mpeg4Part2)) => {
                        TsKind::Other(format!("mpeg4:{}", String::from_utf8_lossy(&t.tag.to_ascii_uppercase())))
                    }
                    (_, Some(CodecId::MsMpeg4V2)) => TsKind::Other("msmpeg4v2".into()),
                    (_, Some(CodecId::MsMpeg4V3)) => TsKind::Other("msmpeg4v3".into()),
                    (_, Some(CodecId::RawVideo)) => TsKind::Other("rawvideo".into()),
                    (_, Some(CodecId::Aac)) => TsKind::AacAdts,
                    (_, Some(CodecId::Ac3)) => TsKind::Ac3,
                    (_, Some(CodecId::Mp2 | CodecId::Mp3)) => TsKind::MpegAudio,
                    (_, Some(CodecId::PcmU8)) => TsKind::Other("pcm_u8".into()),
                    (_, Some(CodecId::PcmS16Le)) => TsKind::Other("pcm_s16le".into()),
                    (_, Some(CodecId::PcmS24Le)) => TsKind::Other("pcm_s24le".into()),
                    (_, Some(CodecId::PcmS32Le)) => TsKind::Other("pcm_s32le".into()),
                    (_, Some(CodecId::PcmF32Le)) => TsKind::Other("pcm_f32le".into()),
                    (_, Some(CodecId::ImaAdpcmWav)) => TsKind::Other("adpcm_ima_wav".into()),
                    (_, Some(CodecId::MsAdpcm)) => TsKind::Other("adpcm_ms".into()),
                    (_, Some(CodecId::PcmAlaw)) => TsKind::Other("pcm_alaw".into()),
                    (_, Some(CodecId::PcmMulaw)) => TsKind::Other("pcm_mulaw".into()),
                    (_, Some(CodecId::Dts)) => TsKind::Dts,
                    (_, Some(_)) => TsKind::Other("data".into()),
                    (Some(MediaType::Video), None) => TsKind::Other(format!(
                        "{}:{}",
                        super::ts::REFUSED_VIDEO,
                        refused_video_name(t.unsupported.as_deref().unwrap_or_default(), &t.tag)
                    )),
                    (Some(MediaType::Audio), None) => TsKind::Other(refused_name(&t)),
                    _ => TsKind::Other("data".into()),
                };
                TrackInfo {
                    pid: t.number as u16,
                    stream_type: 0,
                    kind,
                    stream,
                    refusal,
                    language: None,
                }
            })
            .collect();
        let mut tracks: Vec<TrackInfo> = tracks;
        // The sound a type-1 DV stream carries inside its frames: streams the file never
        // declared as tracks of their own, which `ec-riff` splits out of each frame chunk.
        for s in &streams {
            if s.params.codec == CodecId::PcmS16Le && !tracks.iter().any(|t| t.stream == Some(s.index)) {
                tracks.push(TrackInfo {
                    pid: 0x100 + s.index as u16,
                    stream_type: 0,
                    kind: TsKind::Other("pcm_s16le".into()),
                    stream: Some(s.index),
                    refusal: None,
                    language: None,
                });
            }
        }
        Ok(Self {
            d,
            streams,
            native,
            tracks,
        })
    }

    pub(crate) fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    pub(crate) fn tracks(&self) -> Vec<TrackInfo> {
        self.tracks.clone()
    }

    pub(crate) fn next_packet(&mut self) -> ec_core::Result<Packet> {
        let mut p = self.d.next_packet()?;
        let from = self.native.get(p.stream as usize).copied().unwrap_or(p.time_base);
        let to = |t: i64| Timestamp::new(t, from).rescale(tb(), Rounding::Nearest).ticks;
        p.pts = p.pts.map(to);
        p.dts = p.dts.map(to);
        p.duration = p.duration.map(to);
        p.time_base = tb();
        Ok(p)
    }

    pub(crate) fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> ec_core::Result<()> {
        let at = to.rescale(self.native.get(stream as usize).copied().unwrap_or(to.base), Rounding::Nearest);
        self.d.seek_stream(stream, at, mode).map(|_| ())
    }

    pub(crate) fn field_order(&self, stream: u32) -> Option<ec_core::frame::Interlace> {
        self.d.field_order(stream)
    }

    pub(crate) fn duration_secs(&self) -> Option<f64> {
        self.streams
            .iter()
            .filter_map(|s| s.duration)
            .max()
            .map(|t| t as f64 / 90_000.0)
    }
}

/// Whether a packet of `codec` holds a B picture.
pub(crate) fn is_b_picture(codec: Codec, au: &[u8]) -> bool {
    match codec {
        Codec::Mpeg4 => mpeg4_is_b(au),
        Codec::Mpeg2 => mpeg2_picture_type(au) == Some(3),
        Codec::H264 => h264_is_b(au),
        _ => false,
    }
}

/// Whether the first VOP of an MPEG-4 Part 2 packet is a B-VOP (`vop_coding_type` 2). A packed
/// packet (a P-VOP with its B-VOP behind it) reads as the P it leads with.
fn mpeg4_is_b(au: &[u8]) -> bool {
    au.windows(5).find(|w| w[..4] == [0, 0, 1, 0xB6]).is_some_and(|w| w[4] >> 6 == 2)
}

/// `picture_coding_type` of the first picture header in an MPEG-1/2 packet.
fn mpeg2_picture_type(au: &[u8]) -> Option<u8> {
    let at = au.windows(4).position(|w| w == [0, 0, 1, 0x00])?;
    Some(au.get(at + 5)? >> 3 & 7)
}

/// Whether the first slice of an Annex-B H.264 packet is a B slice (`slice_type` 1 or 6).
fn h264_is_b(au: &[u8]) -> bool {
    for nal in ec_mpegts::nal_units(au) {
        let Some(&head) = nal.first() else { continue };
        if !matches!(head & 0x1f, 1 | 5) {
            continue;
        }
        let mut rbsp = Vec::new();
        unescape_rbsp(&nal[1..nal.len().min(16)], &mut rbsp);
        let mut bits = Bits { data: &rbsp, at: 0 };
        let _first_mb = bits.ue();
        return matches!(bits.ue(), Some(1 | 6));
    }
    false
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.at / 8)?;
        let b = (byte >> (7 - self.at % 8)) & 1;
        self.at += 1;
        Some(u32::from(b))
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let mut v = 0u32;
        for _ in 0..zeros {
            v = v << 1 | self.bit()?;
        }
        Some((1u32 << zeros) - 1 + v)
    }
}
