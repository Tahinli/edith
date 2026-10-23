//! MPEG transport streams -- `.ts`, `.mts`, `.m2ts`, `.m2t` -- over the family's own
//! demuxer, `ec-mpegts`. The file the AVCHD camcorders, the broadcast recorders and the
//! Blu-ray rippers write, and the one container edith could not open at all before.
//!
//! Three readers live here, all of them over one [`ec_mpegts::TsDemuxer`]:
//!
//! - [`TsDemuxer`], the picture: H.264 and HEVC access units in Annex-B (MPEG-1/2 video
//!   as the pictures with their sequence header), the parameter
//!   sets re-injected ahead of a keyframe that lacks them, the same four calls the
//!   mp4 and Matroska doors answer ([`Demuxer`]).
//! - [`TsAudio`], one sound track, with its own demuxer so a seek on the sound alone
//!   lands where the sound is and not at the picture's keyframe, and [`TsDecoder`], the
//!   codec behind it (AAC in ADTS, AC-3, E-AC-3, TrueHD, MP3, HDMV LPCM, DTS).
//! - [`subtitles`], the PGS tracks, as the same display sets Matroska carries.
//!
//! **Time.** A transport stream's first timestamp is not 0 (a camcorder starts at
//! seconds, a broadcast anywhere, a long recording near the 33-bit wrap), and its
//! picture and its sound do not start together. Source time 0 is the first *picture*
//! (its earliest presentation timestamp); a file with no picture counts from its
//! first sound. Everything else is placed against that origin, so the sound that
//! starts 80 ms after the picture starts 80 ms into the clip -- the offset a player
//! honours -- and sound that starts before the picture is trimmed to the clip.
//!
//! **Refusals.** DTS and TrueHD decode ([`TsDecoder`]); LATM and the other kinds
//! `ec-mpegts` has no demux path for are named, with the ffmpeg command that makes
//! the file usable ([`refusal_text_for`]).

use std::collections::VecDeque;

use ec_core::registry::{Decoder as _, SeekMode};
use ec_core::timebase::{TimeBase, Timestamp};
use ec_core::{CodecId, Frame, MediaParameters, MediaType};
use ec_mpegts::{StreamKind as TsKind, VideoFraming};

use super::*;

/// The 90 kHz clock every packet of the demuxer is stamped in.
const TICKS: i64 = 90_000;

fn tb() -> TimeBase {
    TimeBase::new(1, TICKS)
}

/// Whether `path` is a transport stream: its extension, or -- for a name that says
/// nothing -- its bytes. A container whose magic is Matroska's or an mp4's is never
/// one, whatever the file is called, and a standalone sound file or a still is not
/// sniffed at all.
pub fn is_mpegts(path: &Path) -> bool {
    if sniff_container(path).is_some() {
        return false;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(ext.as_deref(), Some("ts" | "mts" | "m2ts" | "m2t" | "tp" | "trp")) {
        return true;
    }
    if crate::is_audio(path) || crate::is_image(path) {
        return false;
    }
    let mut head = vec![0u8; 8 * 1024];
    let read = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    ec_mpegts::is_mpegts(&head[..read])
}

pub(crate) fn ts_error(e: ec_core::Error) -> crate::Error {
    format!("MPEG transport stream: {e}").into()
}

/// The words for a track this build does not read: what `ec-mpegts` names it, and the
/// ffmpeg line that turns the file into one edith opens.
/// The refusal wording for a transport stream (`ps` false) or a program stream.
pub(crate) fn refusal_text_for(why: &str, video: bool, flavor: Flavor) -> String {
    let input = flavor.input_name();
    let fix = match (video, flavor) {
        // An AVI's picture is rarely one an mp4 takes as it is.
        (true, _) | (false, Flavor::Avi | Flavor::Dv) => format!("ffmpeg -i {input} -c:v libx264 -c:a aac out.mp4"),
        (false, _) => format!("ffmpeg -i {input} -c:v copy -c:a aac out.mp4"),
    };
    format!("{why} — convert it with {fix}")
}

/// The ffmpeg line that turns a file of this container into one edith opens.
pub(crate) fn convert_line(flavor: Flavor) -> String {
    format!("ffmpeg -i {} -c:v libx264 -c:a aac out.mp4", flavor.input_name())
}

/// The kind name of a picture stream this build refuses, in an AVI.
pub(crate) const REFUSED_VIDEO: &str = "refused_video";

/// Which container a [`Src`] reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Flavor {
    Ts,
    Ps,
    Avi,
    Dv,
}

impl Flavor {
    pub(crate) fn of(path: &Path) -> Self {
        if super::avi::is_avi(path) {
            Self::Avi
        } else if super::dv::is_dv(path) {
            Self::Dv
        } else if super::ps::is_mpegps(path) {
            Self::Ps
        } else {
            Self::Ts
        }
    }

    fn input_name(self) -> &'static str {
        match self {
            Self::Ts => "in.ts",
            Self::Ps => "in.mpg",
            Self::Avi => "in.avi",
            Self::Dv => "in.dv",
        }
    }
}

/// Whether a track kind is a picture this reads (or, in an AVI, a refused one).
fn is_picture_kind(kind: &TsKind) -> bool {
    kind.is_video()
        || matches!(kind, TsKind::Other(n) if matches!(n.as_str(), "mjpeg" | "prores" | "dnxhd" | "dvvideo") || n.starts_with("mpeg4") || n.starts_with("msmpeg4") || n == "rawvideo" || n.starts_with(REFUSED_VIDEO))
}

type Reader = BufReader<File>;

/// The one demuxer behind every reader here: a transport stream, or -- for `.mpg`, `.vob`,
/// `.m2v` and the rest ([`super::ps`]) -- a program stream. Both speak the same packets and
/// the same track list, so the pictures, the sound and the probe are written once.
pub(crate) enum Src {
    Ts(ec_mpegts::TsDemuxer<Reader>),
    Ps(ec_mpegts::PsDemuxer<Reader>),
    Avi(Box<super::avi::AviSrc>),
    Dv(Box<super::dv::DvSrc>),
}

impl Src {
    pub(crate) fn open(path: &Path, flavor: Flavor) -> crate::Result<Self> {
        if flavor == Flavor::Avi {
            return super::avi::AviSrc::open(path).map(|d| Self::Avi(Box::new(d)));
        }
        if flavor == Flavor::Dv {
            return super::dv::DvSrc::open(path).map(|d| Self::Dv(Box::new(d)));
        }
        let file = File::open(path)?;
        let reader = BufReader::with_capacity(256 << 10, file);
        match flavor {
            Flavor::Ps => ec_mpegts::PsDemuxer::new(reader).map(Self::Ps).map_err(super::ps::ps_error),
            _ => ec_mpegts::TsDemuxer::new(reader).map(Self::Ts).map_err(ts_error),
        }
    }

    pub(crate) fn streams(&self) -> &[ec_core::StreamInfo] {
        match self {
            Self::Ts(d) => d.streams(),
            Self::Ps(d) => d.streams(),
            Self::Avi(d) => d.streams(),
            Self::Dv(d) => d.streams(),
        }
    }

    pub(crate) fn tracks(&self) -> Vec<ec_mpegts::TrackInfo> {
        match self {
            Self::Ts(d) => d.tracks(),
            Self::Ps(d) => d.tracks(),
            Self::Avi(d) => d.tracks(),
            Self::Dv(d) => d.tracks(),
        }
    }

    pub(crate) fn next_packet(&mut self) -> ec_core::Result<ec_core::Packet> {
        match self {
            Self::Ts(d) => d.next_packet(),
            Self::Ps(d) => d.next_packet(),
            Self::Avi(d) => d.next_packet(),
            Self::Dv(d) => d.next_packet(),
        }
    }

    pub(crate) fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> ec_core::Result<()> {
        match self {
            Self::Ts(d) => d.seek(stream, to, mode),
            Self::Ps(d) => d.seek(stream, to, mode),
            Self::Avi(d) => d.seek(stream, to, mode),
            Self::Dv(d) => d.seek(stream, to, mode),
        }
    }

    pub(crate) fn set_video_framing(&mut self, framing: VideoFraming) {
        match self {
            Self::Ts(d) => d.set_video_framing(framing),
            Self::Ps(d) => d.set_video_framing(framing),
            // An AVI's H.264 is Annex B already.
            Self::Avi(_) | Self::Dv(_) => {}
        }
    }

    pub(crate) fn duration_secs(&self) -> Option<f64> {
        match self {
            Self::Ts(d) => d.duration_secs(),
            Self::Ps(d) => d.duration_secs(),
            Self::Avi(d) => d.duration_secs(),
            Self::Dv(d) => d.duration_secs(),
        }
    }
}

fn open_demuxer(path: &Path, flavor: Flavor) -> crate::Result<Src> {
    Src::open(path, flavor)
}

/// What the header of a transport stream says, from one open.
pub(crate) struct TsFile {
    /// The picture stream's index, if the file has a picture this reads.
    pub video: Option<u32>,
    /// Source time 0, in ticks: the first picture's first presentation time, or the
    /// earliest sound/subtitle start where there is no picture.
    pub origin: i64,
    /// Every audio track of the program, in PMT order, the refused ones too.
    pub audio: Vec<TsAudioInfo>,
    /// Every PGS track, in PMT order.
    pub subtitles: Vec<TsSubInfo>,
    /// The refusal of the first picture track this build does not read, if any.
    pub refused_video: Option<String>,
    /// ffprobe's name of that refused picture (`rawvideo`, `dvvideo`, `mpeg4`, ...).
    pub refused_codec: Option<String>,
}

pub(crate) struct TsAudioInfo {
    /// The demuxer's stream index; `None` for a refused track.
    pub stream: Option<u32>,
    /// ffprobe's name: `aac`, `ac3`, `eac3`, `mp3`, `pcm_bluray`, `mp2`, `dts`, ...
    pub codec: String,
    pub codec_id: Option<CodecId>,
    pub language: Option<String>,
    pub channels: u16,
    pub sample_rate: u32,
    pub start: Option<i64>,
    pub duration: Option<i64>,
    pub refusal: Option<String>,
}

pub(crate) struct TsSubInfo {
    pub stream: u32,
    /// The Matroska codec id the track's packets are read as: PGS, VobSub or DVB.
    pub codec: &'static str,
    /// ffprobe's name of it.
    pub probe_name: &'static str,
    pub language: Option<String>,
}

fn ffprobe_audio_name(kind: &TsKind, params: Option<CodecId>) -> String {
    match (kind, params) {
        (TsKind::MpegAudio, Some(CodecId::Mp3)) => "mp3".into(),
        (TsKind::MpegAudio, Some(CodecId::Mp1)) => "mp1".into(),
        (TsKind::MpegAudio, _) => "mp2".into(),
        (TsKind::Lpcm, _) => "pcm_bluray".into(),
        (TsKind::DvdLpcm, _) => "pcm_dvd".into(),
        (k, _) => k.name(),
    }
}

/// Opens `path` and describes it. Cheap -- the demuxer stops reading as soon as every
/// stream has its parameters.
pub(crate) fn describe(path: &Path) -> crate::Result<TsFile> {
    let d = open_demuxer(path, Flavor::of(path))?;
    Ok(describe_open(&d))
}

fn describe_open(d: &Src) -> TsFile {
    let streams = d.streams();
    let info = |i: u32| streams.iter().find(|s| s.index == i);
    let mut video = None;
    let mut refused_video = None;
    let mut refused_codec = None;
    let mut audio = Vec::new();
    let mut subtitles = Vec::new();
    for t in d.tracks() {
        match (&t.kind, t.stream) {
            (k, Some(i)) if is_picture_kind(k) => {
                video.get_or_insert(i);
            }
            (k, None) if is_picture_kind(k) => {
                refused_video.get_or_insert_with(|| t.refusal.clone().unwrap_or_else(|| k.name()));
                refused_codec.get_or_insert_with(|| {
                    k.name().strip_prefix(REFUSED_VIDEO).unwrap_or_default().trim_start_matches(':').to_string()
                });
            }
            (TsKind::Pgs | TsKind::DvdSub | TsKind::DvbSub, Some(stream)) => {
                let (codec, probe_name) = match t.kind {
                    TsKind::DvdSub => (VOBSUB, "dvd_subtitle"),
                    TsKind::DvbSub => (DVBSUB, "dvb_subtitle"),
                    _ => (PGS, "hdmv_pgs_subtitle"),
                };
                subtitles.push(TsSubInfo {
                    stream,
                    codec,
                    probe_name,
                    language: t.language.clone(),
                })
            }
            (TsKind::Pgs | TsKind::DvdSub | TsKind::DvbSub, None) => {}
            (kind, _) if is_audio_kind(kind, t.stream_type) => {
                let s = t.stream.and_then(info);
                let (channels, sample_rate) = match s.map(|s| &s.params.media) {
                    Some(MediaParameters::Audio(a)) => (a.layout.channel_count() as u16, a.sample_rate),
                    _ => (0, 0),
                };
                audio.push(TsAudioInfo {
                    stream: t.stream,
                    codec: ffprobe_audio_name(&t.kind, s.map(|s| s.params.codec)),
                    codec_id: s.map(|s| s.params.codec),
                    language: t.language.clone(),
                    channels,
                    sample_rate,
                    start: s.and_then(|s| s.start_time),
                    duration: s.and_then(|s| s.duration),
                    refusal: t.refusal.clone(),
                });
            }
            _ => {}
        }
    }
    let starts = |pred: &dyn Fn(&ec_core::StreamInfo) -> bool| {
        streams
            .iter()
            .filter(|s| pred(s))
            .filter_map(|s| s.start_time)
            .min()
    };
    let origin = video
        .and_then(info)
        .and_then(|s| s.start_time)
        .or_else(|| starts(&|s| s.params.codec.media_type() == MediaType::Audio))
        .or_else(|| starts(&|_| true))
        .unwrap_or(0);
    TsFile {
        video,
        origin,
        audio,
        subtitles,
        refused_video,
        refused_codec,
    }
}

/// Whether a PMT entry is sound: the known audio kinds (TrueHD included), and the `Other`
/// types that are DTS-HD or an AVI's refused audio, listed by name.
fn is_audio_kind(kind: &TsKind, stream_type: u8) -> bool {
    match kind {
        TsKind::AacAdts
        | TsKind::AacLatm
        | TsKind::Ac3
        | TsKind::Eac3
        | TsKind::TrueHd
        | TsKind::MpegAudio
        | TsKind::Lpcm
        | TsKind::Dts
        | TsKind::DvdLpcm => true,
        TsKind::Other(name) => {
            // The last two rows are an AVI's: its PCM by ffprobe's name, and the tags it refuses.
            matches!(name.as_str(), "dts" | "dts_hd" | "aac_raw")
                || name.starts_with("pcm_")
                || name.starts_with("wav_")
                || matches!(name.as_str(), "wmav1" | "wmav2" | "wmapro" | "adpcm_ms" | "adpcm_ima_wav" | "mp3" | "aac")
                || matches!(stream_type, 0x82 | 0x83 | 0x85 | 0x86 | 0xA2)
        }
        _ => false,
    }
}

// ----------------------------------------------------------------------------------
// The picture
// ----------------------------------------------------------------------------------

/// H.264 or HEVC out of a transport stream, one access unit at a time.
pub struct TsDemuxer {
    inner: Src,
    flavor: Flavor,
    stream: u32,
    pub(crate) codec: Codec,
    /// The first picture's presentation time, in ticks: frame 0.
    start: i64,
    /// Ticks per frame.
    tpf: f64,
    /// Annex-B VPS/SPS/PPS (SPS/PPS for H.264), re-injected ahead of a keyframe that
    /// does not carry its own.
    pub(crate) config: Vec<u8>,
    pub(crate) profile: Option<&'static str>,
    pub(crate) bit_depth: u8,
    pub(crate) chroma: Option<Chroma>,
    pub(crate) hevc_profile: Option<u8>,
    pub(crate) light: ContentLight,
    /// The access unit [`Self::seek_to_sync_at_or_before`] read to learn where it
    /// landed, handed out by the next [`Self::next_access_unit`].
    pending: VecDeque<Vec<u8>>,
    /// An AVI's field order for its MJPEG, from the demuxer ([`Self::field_order`]).
    field_order: Option<ec_core::frame::Interlace>,
    /// An AVI's FourCC for its MPEG-4 Part 2 picture (`XVID`, `DIVX`...); see [`Demuxer::fourcc`].
    pub(crate) fourcc: Option<[u8; 4]>,
    /// The picture stream's codec id (MS-MPEG4 version 2 or 3 share one `Codec`) and its stated size.
    pub(crate) codec_id: CodecId,
    pub(crate) size: (u32, u32),
    /// An AVI's raw video `BITMAPINFOHEADER`; empty for every other codec.
    pub(crate) header: Vec<u8>,
    /// What a DV picture states (interlace and field order), read off its first frame at open
    /// ([`Demuxer::fill_intra`]); the default (progressive) for every other codec here.
    pub(crate) intra: crate::decode::intra::IntraFacts,
    /// Bytes of picture and of sound over the file, for the bitrate card.
    path: PathBuf,
    /// The stream asks for 3:2 field repeats: its pictures are 23.976p film frames
    /// and [`VideoMeta::frame_rate`] says so ([`crate::ivtc::Repeats`]).
    pub(crate) soft_pulldown: bool,
}

impl TsDemuxer {
    pub(crate) fn colour_tags(&self) -> Tags {
        bitstream_tags(self.codec, &self.config)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
        let flavor = Flavor::of(path);
        let mut inner = open_demuxer(path, flavor)?;
        let file = describe_open(&inner);
        let Some(stream) = file.video else {
            return Err(match (file.refused_video, flavor) {
                (Some(why), _) => refusal_text_for(&why, true, flavor).into(),
                (None, Flavor::Avi) => {
                    crate::Error::from(NoVideoTrack("this AVI file has no video track".into()))
                }
                (None, Flavor::Dv) => {
                    crate::Error::from(NoVideoTrack("this DV file has no video track".into()))
                }
                (None, flavor) => crate::Error::from(NoVideoTrack(format!(
                    "this MPEG {} stream has no video track",
                    if flavor == Flavor::Ps { "program" } else { "transport" }
                ))),
            });
        };
        let info = inner
            .streams()
            .iter()
            .find(|s| s.index == stream)
            .ok_or("MPEG transport stream: the picture stream vanished")?
            .clone();
        let MediaParameters::Video(video) = info.params.media.clone() else {
            return Err("MPEG transport stream: the picture stream has no picture parameters".into());
        };
        let codec = match info.params.codec {
            CodecId::H264 => Codec::H264,
            CodecId::H265 => Codec::Hevc,
            CodecId::Mpeg2Video => Codec::Mpeg2,
            CodecId::Mjpeg => Codec::Mjpeg,
            CodecId::ProRes => Codec::ProRes,
            CodecId::Dnxhd => Codec::Dnxhd,
            CodecId::DvVideo => Codec::Dv,
            CodecId::Mpeg4Part2 => Codec::Mpeg4,
            CodecId::MsMpeg4V2 | CodecId::MsMpeg4V3 => Codec::MsMpeg4,
            CodecId::RawVideo => Codec::Raw,
            other => return Err(format!("MPEG transport stream: {other:?} video is not supported").into()),
        };
        for stated in [video.width, video.height] {
            if stated > crate::MAX_RESOLUTION {
                return Err(unsupported_display_size(u64::from(stated)));
            }
        }
        let extradata = info
            .params
            .extradata
            .as_ref()
            .map(|b| b.as_ref().to_vec())
            .unwrap_or_default();
        let (mut config, mut bit_depth, mut hevc_profile, mut profile) = (Vec::new(), 8, None, None);
        let mut header = Vec::new();
        match codec {
            Codec::H264 => {
                config = parse_avcc(&extradata)?.1;
                bit_depth = avc_bit_depth(&config)?;
                profile = video_profile::h264(&config);
            }
            Codec::Hevc => {
                let (_, sets, depth) = parse_hvcc(&extradata)?;
                config = sets;
                bit_depth = depth;
                hevc_profile = extradata.get(1).map(|b| b & 0x1f);
                profile = video_profile::hevc_record(&extradata);
            }
            // The sequence header (and its extension) the decoder starts from, in front of
            // every keyframe that lacks one.
            Codec::Mpeg2 => {
                config = extradata.clone();
                profile = mpeg2_profile(&config);
            }
            // The raw layout is stated by the BITMAPINFOHEADER alone.
            // The BITMAPINFOHEADER states the layout; it is not a parameter set to put in front
            // of the pictures.
            Codec::Raw => {
                header = extradata.clone();
                profile = Some(video_profile::raw(&header));
            }
            Codec::MsMpeg4 => profile = Some(if info.params.codec == CodecId::MsMpeg4V2 { "v2" } else { "v3" }),
            _ => {}
        }
        let chroma = bitstream_chroma(codec, &config);
        // A stream states its rate by its timestamps; an estimate that is missing (a
        // single picture) falls back to the broadcast rate nothing else could be.
        let fps = video
            .frame_rate
            .map(|r| r.num() as f64 / r.den() as f64)
            .filter(|f| *f > 0.0)
            .unwrap_or(25.0);
        let tpf = TICKS as f64 / fps;
        let start = info.start_time.unwrap_or(file.origin);
        // Frames: the picture's span over its rate. The demuxer's duration runs from the
        // first presentation time to the end of the last picture, found by a tail scan.
        let frames = match info.duration {
            Some(d) if d > 0 => ((d as f64 / tpf).round() as u32).max(1),
            _ => 1,
        };
        let pixel_aspect = video
            .sample_aspect_ratio
            .and_then(|r| PixelAspect::stated(r.num() as u32, r.den() as u32))
            .unwrap_or(PixelAspect::SQUARE);
        let (width, height) = (pixel_aspect.width_factor(video.width), video.height);
        let tags = bitstream_tags(codec, &config);
        let mut meta = VideoMeta {
            width,
            height,
            coded_width: video.width,
            coded_height: video.height,
            pixel_aspect,
            frame_rate: fps,
            frame_count: frames,
            codec,
            color: ColorDescription::resolve_sized(Tags::default(), tags, width, height),
            matrix_assumed: ColorDescription::matrix_assumed(Tags::default(), tags),
            rotation: Rotation::None,
        };
        inner.set_video_framing(VideoFraming::AnnexB);
        // An AVI's MPEG-4 picture carries its FourCC in the track's kind (`mpeg4:XVID`).
        let fourcc = inner.tracks().iter().find(|t| t.stream == Some(stream)).and_then(|t| match &t.kind {
            TsKind::Other(n) => n.strip_prefix("mpeg4:").and_then(|f| f.as_bytes().try_into().ok()),
            _ => None,
        });
        let field_order = match &inner {
            Src::Avi(a) => a.field_order(stream),
            _ => None,
        };
        let mut demuxer = Self {
            inner,
            flavor,
            stream,
            codec,
            start,
            tpf,
            config,
            profile,
            bit_depth,
            chroma,
            hevc_profile,
            light: ContentLight::default(),
            pending: VecDeque::new(),
            field_order,
            fourcc,
            codec_id: info.params.codec,
            size: (video.width, video.height),
            header,
            intra: Default::default(),
            path: path.to_path_buf(),
            soft_pulldown: false,
        };
        demuxer.seek_to_sync_at_or_before(0);
        // Soft pulldown: the coded pictures are the 23.976p film frames and the stream
        // asks the player to repeat fields, so its pictures arrive 2 and 3 fields
        // apart and the shortest gap, which the rate was estimated from, is the
        // 29.97 of the repeated display. The pictures' own rate is four fifths of it
        // ([`crate::ivtc::Repeats`]).
        if matches!(codec, Codec::Mpeg2 | Codec::H264) && flavor != Flavor::Avi && fps > 25.0 {
            let mut repeats = crate::ivtc::Repeats::default();
            for _ in 0..crate::ivtc::SAMPLE {
                match demuxer.next_access_unit() {
                    Ok(Some(au)) if codec == Codec::Mpeg2 => repeats.mpeg2(&au),
                    Ok(Some(au)) => repeats.h264(&demuxer.config, &au),
                    _ => break,
                }
            }
            if repeats.soft() {
                let film = fps * 0.8;
                demuxer.soft_pulldown = true;
                demuxer.tpf = TICKS as f64 / film;
                meta.frame_rate = film;
                meta.frame_count = match info.duration {
                    Some(d) if d > 0 => ((d as f64 / demuxer.tpf).round() as u32).max(1),
                    _ => 1,
                };
            }
            demuxer.seek_to_sync_at_or_before(0);
        }
        Ok((meta, demuxer))
    }

    /// The next picture's bytes in decode order, `None` at the end of the file.
    pub(crate) fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
        if let Some(au) = self.pending.pop_front() {
            return Ok(Some(au));
        }
        Ok(self.read_picture()?.map(|(_, au)| au))
    }

    /// One picture and its time, with the parameter sets in front of a keyframe that
    /// carries none (a recovery-point frame in a stream that repeats them only now
    /// and then).
    fn read_picture(&mut self) -> crate::Result<Option<(i64, Vec<u8>)>> {
        loop {
            let packet = match self.inner.next_packet() {
                Ok(p) => p,
                Err(ec_core::Error::Eof) => return Ok(None),
                Err(e) => return Err(ts_error(e)),
            };
            if packet.stream != self.stream {
                continue;
            }
            let pts = packet.pts.or(packet.dts).unwrap_or(self.start);
            let bytes: &[u8] = &packet.data;
            let present = match self.codec {
                Codec::Mpeg2 => has_sequence_header(bytes),
                codec => has_parameter_sets(codec, bytes),
            };
            if !packet.flags.keyframe || self.config.is_empty() || present {
                return Ok(Some((pts, bytes.to_vec())));
            }
            let mut au = Vec::with_capacity(self.config.len() + bytes.len());
            au.extend_from_slice(&self.config);
            au.extend_from_slice(bytes);
            return Ok(Some((pts, au)));
        }
    }

    /// Puts the cursor on the latest keyframe whose presentation time is at or before
    /// display frame `frame`, and returns the display index of that keyframe.
    pub(crate) fn seek_to_sync_at_or_before(&mut self, frame: u32) -> i64 {
        let mut slot_target = frame;
        loop {
            let (slot, lead) = self.seek_slot(slot_target);
            let shown = slot + lead;
            // An AVI's keyframe is placed by its slot in the file; its leading B pictures put
            // its display index later, possibly past the frame asked for. Go back a keyframe.
            if shown > i64::from(frame) && slot > 0 && self.flavor == Flavor::Avi {
                slot_target = (slot - 1) as u32;
                continue;
            }
            return shown;
        }
    }

    /// Puts the cursor on the keyframe at or before decode slot / display frame `frame` and
    /// returns its slot and the number of pictures displayed before it that follow it in the file.
    fn seek_slot(&mut self, frame: u32) -> (i64, i64) {
        self.pending.clear();
        let target = self.start + (f64::from(frame) * self.tpf).round() as i64;
        let target = target.max(self.start);
        let landed = self
            .inner
            .seek(self.stream, Timestamp::new(target, tb()), SeekMode::SyncBefore)
            .is_ok();
        if !landed {
            // A seek the file refuses (damage ahead of the target) starts over from the
            // top rather than leaving the cursor where it was.
            if let Ok(fresh) = open_demuxer(&self.path, self.flavor) {
                self.inner = fresh;
                self.inner.set_video_framing(VideoFraming::AnnexB);
            }
        }
        match self.read_picture() {
            Ok(Some((pts, au))) => {
                self.pending.push_back(au);
                let slot = (((pts - self.start) as f64) / self.tpf).round() as i64;
                (slot, self.leading_b_pictures())
            }
            _ => (0, 0),
        }
    }

    /// In an AVI, which has no timestamps, the keyframe just read ([`Self::pending`]) sits at its
    /// decode slot; the B pictures that follow it at once are displayed *before* it, so its
    /// display index is that many later. They are read ahead into `pending`.
    fn leading_b_pictures(&mut self) -> i64 {
        if self.flavor != Flavor::Avi || !matches!(self.codec, Codec::Mpeg2 | Codec::H264 | Codec::Mpeg4) {
            return 0;
        }
        let (mut ahead, mut leading) = (Vec::new(), 0);
        while ahead.len() < 16 {
            let Ok(Some((_, au))) = self.read_picture() else { break };
            let b = super::avi::is_b_picture(self.codec, &au);
            ahead.push(au);
            if !b {
                break;
            }
            leading += 1;
        }
        self.pending.extend(ahead);
        leading
    }

    /// The scan order of the fields of an AVI's interlaced MJPEG, where the file states one.
    pub(crate) fn field_order(&self) -> Option<ec_core::frame::Interlace> {
        self.field_order
    }

    /// Bytes of the picture stream, for the bitrate card: a walk of the file.
    pub(crate) fn stream_bytes(&self) -> (Option<u64>, Option<u64>) {
        let Ok(mut d) = open_demuxer(&self.path, self.flavor) else {
            return (None, None);
        };
        let (mut video, mut audio) = (0u64, 0u64);
        let first_audio = d
            .tracks()
            .into_iter()
            .find(|t| matches!(
                    t.kind,
                    TsKind::AacAdts | TsKind::Ac3 | TsKind::Eac3 | TsKind::TrueHd | TsKind::MpegAudio | TsKind::Lpcm | TsKind::DvdLpcm
                ) && t.stream.is_some())
            .and_then(|t| t.stream);
        while let Ok(p) = d.next_packet() {
            if p.stream == self.stream {
                video += p.data.len() as u64;
            } else if Some(p.stream) == first_audio {
                audio += p.data.len() as u64;
            }
        }
        (Some(video), Some(audio))
    }

    /// The length the container's streams span, in seconds: the longest of them.
    pub(crate) fn container_secs(&self) -> Option<f64> {
        self.inner.duration_secs()
    }
}

/// Whether an Annex-B access unit already carries the sets a decoder starts from.
fn has_parameter_sets(codec: Codec, au: &[u8]) -> bool {
    ec_mpegts::nal_units(au).any(|nal| match codec {
        Codec::H264 => nal.first().is_some_and(|b| b & 0x1f == 7),
        Codec::Hevc => nal.first().is_some_and(|b| (b >> 1) & 0x3f == 33),
        _ => true,
    })
}

/// ffprobe's name for the profile of an MPEG-2 sequence extension.
fn mpeg2_profile(config: &[u8]) -> Option<&'static str> {
    let b = crate::colorspace::mpeg2_extension(config, 1)?;
    let code = (b.first()? & 0x0f) << 4 | b.get(1)? >> 4;
    Some(match (code >> 4) & 7 {
        5 => "Simple",
        4 => "Main",
        3 => "SNR Scalable",
        2 => "Spatial Scalable",
        1 => "High",
        0 if code & 0x80 != 0 => "4:2:2",
        _ => return None,
    })
}

/// Whether an MPEG-1/2 picture packet leads with (or holds) a sequence header.
fn has_sequence_header(au: &[u8]) -> bool {
    au.windows(4).any(|w| w == [0, 0, 1, 0xB3])
}

// ----------------------------------------------------------------------------------
// The sound
// ----------------------------------------------------------------------------------

/// What a decoded run of sound is: interleaved f32, the width it was decoded at, and the
/// frames per channel it holds.
pub(crate) struct Decoded {
    pub samples: Vec<f32>,
    pub channels: usize,
    pub frames: u64,
}

/// A codec of a transport stream's sound, fed one packet at a time.
pub(crate) enum TsDecoder {
    Aac(Box<ec_aac::AacDecoder>),
    Ac3(Box<ec_ac3::Ac3Decoder>),
    /// Dolby TrueHD, one access unit per packet, as the Matroska path feeds it.
    TrueHd(Box<ec_truehd::TrueHdDecoder>),
    Mp3(Box<ec_mp3::Mp3Decoder>),
    /// HDMV LPCM: the width of a sample in bytes, big-endian 16 bit or the little-endian
    /// 24 bit the demuxer hands over, and the channel count.
    Pcm { bytes: usize, big_endian: bool, channels: usize },
    /// An AVI's little-endian PCM (8, 16, 24, 32 bit integer or 32 bit float).
    PcmLe { codec: CodecId, channels: usize },
    /// DTS, and an AVI's IMA / MS ADPCM, A-law and mu-law ([`crate::extaudio`]).
    Ext(Box<crate::extaudio::ExtDecoder>),
}

/// Bits per sample of a PCM track, `None` for anything coded.
pub(crate) fn pcm_depth<T: From<u8>>(codec: Option<CodecId>) -> Option<T> {
    Some(T::from(match codec? {
        CodecId::PcmU8 => 8,
        CodecId::PcmS16Be | CodecId::PcmS16Le => 16,
        CodecId::PcmS24Le => 24,
        CodecId::PcmS32Le | CodecId::PcmF32Le => 32,
        _ => return None,
    }))
}

/// The little-endian PCM an AVI carries.
fn is_pcm_le(codec: CodecId) -> bool {
    matches!(
        codec,
        CodecId::PcmU8 | CodecId::PcmS16Le | CodecId::PcmS24Le | CodecId::PcmS32Le | CodecId::PcmF32Le
    )
}

impl TsDecoder {
    fn new(
        codec: CodecId,
        channels: usize,
        sample_rate: u32,
        stereo_downmix: bool,
        asc: &[u8],
        params: Option<&ec_core::CodecParameters>,
    ) -> crate::Result<Self> {
        Ok(match codec {
            c if crate::extaudio::is_ext_codec(c) => {
                let params = params.ok_or("the sound track states no codec parameters")?;
                Self::Ext(Box::new(crate::extaudio::ExtDecoder::new(params)?))
            }
            CodecId::Aac => Self::Aac(Box::new(match asc.is_empty() {
                true => ec_aac::AacDecoder::new(),
                false => ec_aac::AacDecoder::with_config_bytes(asc)
                    .map_err(|e| format!("AAC configuration: {e}"))?,
            })),
            CodecId::Ac3 | CodecId::EAc3 => Self::Ac3(Box::new(ec_ac3::Ac3Decoder::with_options(ec_ac3::Options {
                downmix: match stereo_downmix {
                    true => ec_ac3::Downmix::Stereo,
                    false => ec_ac3::Downmix::Native,
                },
                ..ec_ac3::Options::default()
            }))),
            CodecId::TrueHd => Self::TrueHd(Box::new(ec_truehd::TrueHdDecoder::new())),
            CodecId::Mp3 | CodecId::Mp2 | CodecId::Mp1 => Self::Mp3(Box::new(
                ec_mp3::Mp3Decoder::new(ec_mp3::layer_codec_parameters(
                    match codec {
                        CodecId::Mp1 => 1,
                        CodecId::Mp2 => 2,
                        _ => 3,
                    },
                    sample_rate,
                    channels.max(1),
                ))
                    .map_err(ts_error)?,
            )),
            CodecId::PcmS16Be => Self::Pcm { bytes: 2, big_endian: true, channels: channels.max(1) },
            CodecId::PcmS24Le => Self::Pcm { bytes: 3, big_endian: false, channels: channels.max(1) },
            c if is_pcm_le(c) => Self::PcmLe { codec: c, channels: channels.max(1) },
            other => return Err(format!("MPEG transport stream: {other:?} sound is not supported").into()),
        })
    }

    /// One packet in. `Ok(None)` when the codec wants more before it answers.
    pub(crate) fn decode(&mut self, data: &[u8]) -> crate::Result<Option<Decoded>> {
        match self {
            Self::Aac(d) => {
                let pcm = d
                    .decode(data, None)
                    .map_err(|e| format!("AAC decode failed: {e}"))?;
                let channels = usize::from(pcm.channels).max(1);
                let frames = (pcm.samples.len() / channels) as u64;
                Ok((frames > 0).then_some(Decoded { samples: pcm.samples, channels, frames }))
            }
            Self::Ac3(d) => match d.decode_frame(data) {
                Ok(frame) => {
                    let samples: Vec<f32> = frame.data[0]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|s| f32::from_le_bytes(*s))
                        .collect();
                    let frames = frame.samples as u64;
                    let channels = (samples.len() as u64 / frames.max(1)).max(1) as usize;
                    Ok(Some(Decoded { samples, channels, frames }))
                }
                Err(ec_ac3::Error::NeedMore) => Ok(None),
                Err(e) => Err(format!("AC-3 decode failed: {e}").into()),
            },
            Self::TrueHd(d) => match d.decode_access_unit(data) {
                Ok(Some(frame)) => {
                    let channels = frame.layout.channel_count().max(1);
                    let frames = frame.samples as u64;
                    let samples = crate::audio::truehd_f32(&frame);
                    Ok(Some(Decoded { samples, channels, frames }))
                }
                // No restart header seen yet: nothing to emit for this unit.
                Ok(None) | Err(ec_core::Error::NeedMore) => Ok(None),
                Err(e) => Err(format!("TrueHD decode failed: {e}").into()),
            },
            Self::Mp3(d) => {
                let packet = ec_core::Packet::new(0, tb(), data.to_vec());
                d.send_packet(&packet).map_err(ts_error)?;
                let mut samples = Vec::new();
                let mut channels = 0usize;
                loop {
                    match d.receive_frame() {
                        Ok(Frame::Audio(a)) => {
                            channels = a.layout.channel_count().max(1);
                            samples.extend(
                                a.data[0]
                                    .as_ref()
                                    .as_chunks::<4>()
                                    .0
                                    .iter()
                                    .map(|s| f32::from_ne_bytes(*s)),
                            );
                        }
                        Ok(_) => {}
                        Err(ec_core::Error::NeedMore | ec_core::Error::Eof) => break,
                        Err(e) => return Err(ts_error(e)),
                    }
                }
                let frames = (samples.len() / channels.max(1)) as u64;
                Ok((frames > 0).then_some(Decoded { samples, channels, frames }))
            }
            Self::Ext(d) => Ok(d.decode(data)?.map(|(samples, channels)| {
                let frames = (samples.len() / channels.max(1)) as u64;
                Decoded { samples, channels, frames }
            })),
            Self::PcmLe { codec, channels } => {
                let bytes = match codec {
                    CodecId::PcmU8 => 1,
                    CodecId::PcmS16Le => 2,
                    CodecId::PcmS24Le => 3,
                    _ => 4,
                };
                let frames = data.len() / (bytes * *channels);
                let samples = data[..frames * bytes * *channels]
                    .chunks_exact(bytes)
                    .map(|s| match codec {
                        CodecId::PcmU8 => (f32::from(s[0]) - 128.0) / 128.0,
                        CodecId::PcmS16Le => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
                        CodecId::PcmS24Le => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
                        CodecId::PcmS32Le => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
                        _ => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    })
                    .collect();
                Ok(Some(Decoded { samples, channels: *channels, frames: frames as u64 }))
            }
            Self::Pcm { bytes, big_endian, channels } => {
                let step = *bytes * *channels;
                let frames = data.len() / step.max(1);
                let mut samples = Vec::with_capacity(frames * *channels);
                for s in data[..frames * step].chunks_exact(*bytes) {
                    let v = match (*bytes, *big_endian) {
                        (2, true) => f32::from(i16::from_be_bytes([s[0], s[1]])) / 32768.0,
                        _ => {
                            let raw = i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8;
                            raw as f32 / 8_388_608.0
                        }
                    };
                    samples.push(v);
                }
                Ok(Some(Decoded { samples, channels: *channels, frames: frames as u64 }))
            }
        }
    }
}

/// One sound track of a transport stream, with a demuxer of its own.
pub(crate) struct TsAudio {
    path: PathBuf,
    d: Src,
    flavor: Flavor,
    stream: u32,
    pub(crate) codec: CodecId,
    /// The `AudioSpecificConfig` of raw AAC (an AVI's), empty for ADTS.
    asc: Vec<u8>,
    /// The stream's codec parameters, for the codecs built from them
    /// ([`crate::extaudio`]).
    params: Option<ec_core::CodecParameters>,
    /// Source time 0 in ticks, and this track's first timestamp.
    origin: i64,
    start: i64,
    /// The earliest timestamp of any stream: a seek this close to it starts over.
    file_start: i64,
    /// The packet a seek read to learn where it landed.
    pending: VecDeque<(i64, Vec<u8>)>,
    /// What the decoder is built from: source channels, rate, and whether the AC-3
    /// decoder folds to a pair.
    pub(crate) src_channels: usize,
    pub(crate) sample_rate: u32,
    pub(crate) stereo_downmix: bool,
    pub(crate) total_samples: Option<u64>,
}

impl TsAudio {
    /// Opens audio track `index` (PMT order, refused tracks counted). `Ok(None)` for a
    /// track the build cannot decode, `Err` for an index the file does not have.
    pub(crate) fn open(path: &Path, index: usize) -> crate::Result<Option<Self>> {
        let flavor = Flavor::of(path);
        let d = open_demuxer(path, flavor)?;
        let file = describe_open(&d);
        if file.audio.is_empty() {
            return Ok(None);
        }
        let Some(info) = file.audio.get(index) else {
            return Err(format!(
                "{}: audio stream {index} of {} stream{}",
                path.display(),
                file.audio.len(),
                if file.audio.len() == 1 { "" } else { "s" }
            )
            .into());
        };
        let (Some(stream), Some(codec)) = (info.stream, info.codec_id) else {
            return Ok(None);
        };
        let asc = d
            .streams()
            .iter()
            .find(|s| s.index == stream)
            .and_then(|s| s.params.extradata.as_ref())
            .filter(|_| codec == CodecId::Aac)
            .map(|b| b.as_ref().to_vec())
            .unwrap_or_default();
        let d_params = d
            .streams()
            .iter()
            .find(|s| s.index == stream)
            .map(|s| s.params.clone());
        let start = info.start.unwrap_or(file.origin);
        let file_start = d
            .streams()
            .iter()
            .filter_map(|s| s.start_time)
            .min()
            .unwrap_or(start);
        let mut track = Self {
            path: path.to_path_buf(),
            d,
            flavor,
            stream,
            codec,
            asc,
            params: d_params,
            origin: file.origin,
            start,
            file_start,
            pending: VecDeque::new(),
            src_channels: usize::from(info.channels).max(1),
            sample_rate: info.sample_rate,
            stereo_downmix: false,
            total_samples: None,
        };
        track.measure()?;
        let end = start + info.duration.unwrap_or(0);
        // The track plays from `start - origin` to its end on the clip's clock; a track
        // that begins before the picture is trimmed to the picture.
        let rel_end = (end - file.origin).max(0);
        track.total_samples = Some((rel_end as i128 * i128::from(track.sample_rate) / i128::from(TICKS)) as u64);
        Ok(Some(track))
    }

    /// Decodes the first packets to learn what the headers do not say: the rate and
    /// width a decoder really emits (HE-AAC doubles its core rate), and whether an
    /// AC-3 track has more than one front channel to fold.
    fn measure(&mut self) -> crate::Result<()> {
        if self.codec == CodecId::Ac3 || self.codec == CodecId::EAc3 {
            if let Some((_, first)) = self.read_audio()? {
                let nfchans = match self.codec {
                    CodecId::EAc3 => ec_ac3::eac3::parse(first.get(2..).unwrap_or_default())
                        .map(|b| b.nfchans)
                        .map_err(|e| format!("not a readable E-AC-3 bit stream information: {e:?}"))?,
                    _ => ec_ac3::bsi::parse(first.get(5..).unwrap_or_default())
                        .map(|b| b.nfchans)
                        .map_err(|e| format!("not a readable AC-3 bit stream information: {e:?}"))?,
                };
                self.stereo_downmix = nfchans > 1;
            }
            self.reopen()?;
        }
        let mut decoder = self.decoder()?;
        for _ in 0..16 {
            let Some((_, bytes)) = self.read_audio()? else { break };
            if let Some(out) = decoder.decode(&bytes)? {
                self.src_channels = out.channels;
                // The rate of an AAC stream is what the decoder emits, not what the ADTS
                // header of its core says.
                if self.codec == CodecId::Aac
                    && let Ok(aac) = self.aac_decoder().and_then(|mut d| {
                        d.decode(&bytes, None).map_err(|e| format!("AAC decode failed: {e}").into())
                    })
                    && aac.sample_rate > 0
                {
                    self.sample_rate = aac.sample_rate;
                }
                break;
            }
        }
        self.reopen()?;
        if self.sample_rate == 0 {
            return Err("MPEG transport stream: the sound track states no sample rate".into());
        }
        Ok(())
    }

    /// The width the timeline gets: mono stays mono, anything wider is folded to a pair.
    pub(crate) fn channels(&self) -> u16 {
        match (self.stereo_downmix, self.src_channels) {
            (_, 1) => 1,
            _ => 2,
        }
    }

    pub(crate) fn decoder(&self) -> crate::Result<TsDecoder> {
        TsDecoder::new(
            self.codec,
            self.src_channels,
            self.sample_rate,
            self.stereo_downmix,
            &self.asc,
            self.params.as_ref(),
        )
    }

    fn aac_decoder(&self) -> crate::Result<ec_aac::AacDecoder> {
        match self.asc.is_empty() {
            true => Ok(ec_aac::AacDecoder::new()),
            false => ec_aac::AacDecoder::with_config_bytes(&self.asc)
                .map_err(|e| format!("AAC configuration: {e}").into()),
        }
    }

    fn reopen(&mut self) -> crate::Result<()> {
        self.d = open_demuxer(&self.path, self.flavor)?;
        self.pending.clear();
        Ok(())
    }

    /// Seconds a decoder is run ahead of the first sample it is asked for.
    pub(crate) fn pre_roll_secs(&self) -> f64 {
        match self.codec {
            CodecId::PcmS16Be | CodecId::PcmS24Le => 0.0,
            c if is_pcm_le(c) => 0.0,
            CodecId::ImaAdpcmWav | CodecId::MsAdpcm | CodecId::PcmAlaw | CodecId::PcmMulaw => 0.0,
            _ => 0.35,
        }
    }

    fn read_audio(&mut self) -> crate::Result<Option<(i64, Vec<u8>)>> {
        if let Some(p) = self.pending.pop_front() {
            return Ok(Some(p));
        }
        loop {
            match self.d.next_packet() {
                Ok(p) if p.stream == self.stream => {
                    let pts = p.pts.or(p.dts).unwrap_or(self.start);
                    return Ok(Some((pts, p.data.as_ref().to_vec())));
                }
                Ok(_) => {}
                Err(ec_core::Error::Eof) => return Ok(None),
                Err(e) => return Err(ts_error(e)),
            }
        }
    }

    /// The next packet of this track: its time in samples on the clip's clock (negative
    /// before the picture) and its bytes.
    pub(crate) fn next_packet(&mut self) -> crate::Result<Option<(i128, Vec<u8>)>> {
        Ok(self.read_audio()?.map(|(pts, bytes)| {
            let rel = i128::from(pts - self.origin) * i128::from(self.sample_rate);
            // Rounded to the nearest sample.
            let samples = (rel * 2 + i128::from(TICKS)).div_euclid(2 * i128::from(TICKS));
            (samples, bytes)
        }))
    }

    /// Positions the track so the next packet starts at or before `secs` of the clip
    /// (or at the top of the file, if nothing earlier exists).
    pub(crate) fn seek(&mut self, secs: f64) -> crate::Result<()> {
        let want = self.origin + (secs.max(0.0) * TICKS as f64) as i64;
        if want <= self.file_start + TICKS {
            return self.reopen();
        }
        // DVD LPCM at 20 and 24 bits packs two frames to a group that can straddle two packets;
        // a read that begins in the middle of a packet cannot tell where a group begins and
        // decodes noise from there on. It is read from the top instead (the sound is only
        // copied, so this is a walk of the file, not a decode).
        if self.flavor == Flavor::Ps && self.codec == CodecId::PcmS24Le {
            return self.reopen();
        }
        let mut slack = TICKS;
        loop {
            let at = want - slack;
            if at <= self.file_start {
                return self.reopen();
            }
            self.pending.clear();
            match self.d.seek(self.stream, Timestamp::new(at, tb()), SeekMode::Exact) {
                Ok(()) => {}
                Err(_) => return self.reopen(),
            }
            match self.read_audio()? {
                Some((pts, bytes)) if pts <= want => {
                    self.pending.push_front((pts, bytes));
                    return Ok(());
                }
                Some((pts, bytes)) => {
                    // Landed after what was wanted: look further back.
                    self.pending.push_front((pts, bytes));
                    slack *= 4;
                    if slack > 600 * TICKS {
                        return self.reopen();
                    }
                }
                None => return self.reopen(),
            }
        }
    }
}

// ----------------------------------------------------------------------------------
// The subtitles
// ----------------------------------------------------------------------------------

/// The PGS tracks of a transport stream, one [`MkvSubtitle`] each so the Matroska reader
/// of display sets ([`crate::subtitle`]) takes them as they are. `number` is the demuxer's
/// stream index, which is what a project saves.
pub(crate) fn subtitles(path: &Path) -> crate::Result<Vec<MkvSubtitle>> {
    let mut d = open_demuxer(path, Flavor::of(path))?;
    let file = describe_open(&d);
    let mut tracks: Vec<MkvSubtitle> = file
        .subtitles
        .iter()
        .map(|s| MkvSubtitle {
            number: u64::from(s.stream),
            codec: s.codec.to_string(),
            language: s.language.clone().unwrap_or_else(|| "und".into()),
            name: String::new(),
            private: Vec::new(),
            cues: Vec::new(),
            unsupported: None,
            unpack: Unpack::None,
        })
        .collect();
    if tracks.is_empty() {
        return Ok(tracks);
    }
    loop {
        match d.next_packet() {
            Ok(p) => {
                if let Some(t) = tracks.iter_mut().find(|t| t.number == u64::from(p.stream)) {
                    let pts = p.pts.or(p.dts).unwrap_or(file.origin);
                    t.cues.push(MkvCue {
                        start_us: (pts - file.origin) * 1_000_000 / TICKS,
                        duration_us: None,
                        payload: p.data.as_ref().to_vec(),
                    });
                }
            }
            Err(ec_core::Error::Eof) => break,
            Err(e) => return Err(ts_error(e)),
        }
    }
    Ok(tracks)
}

// ----------------------------------------------------------------------------------
// The probe
// ----------------------------------------------------------------------------------

/// Every stream of a transport stream as [`StreamFacts`] lists it: the picture, each sound
/// track in PMT order (the refused ones too, by their ffprobe name), each PGS track.
pub(crate) fn header_streams(path: &Path) -> crate::Result<Vec<StreamFacts>> {
    let d = open_demuxer(path, Flavor::of(path))?;
    let file = describe_open(&d);
    let secs = |ticks: Option<i64>| ticks.filter(|t| *t > 0).map(|t| t as f64 / TICKS as f64);
    let mut out = Vec::new();
    if let Some(info) = file.video.and_then(|v| d.streams().iter().find(|s| s.index == v)) {
        out.push(StreamFacts {
            kind: StreamKind::Video,
            index: 0,
            codec_id: Some(
                match info.params.codec {
                    CodecId::H265 => "hevc",
                    CodecId::Mpeg2Video => "mpeg2video",
                    CodecId::Mjpeg => "mjpeg",
                    CodecId::ProRes => "prores",
                    CodecId::Dnxhd => "dnxhd",
                    CodecId::DvVideo => "dvvideo",
                    CodecId::Mpeg4Part2 => "mpeg4",
                    CodecId::MsMpeg4V2 => "msmpeg4v2",
                    CodecId::MsMpeg4V3 => "msmpeg4v3",
                    CodecId::RawVideo => "rawvideo",
                    _ => "h264",
                }
                .to_string(),
            ),
            language: None,
            channels: None,
            sample_rate: None,
            duration_secs: secs(info.duration),
            profile: None,
            bit_depth: None,
        });
    }
    if let (None, Some(name)) = (file.video, &file.refused_codec) {
        out.push(StreamFacts {
            kind: StreamKind::Video,
            index: 0,
            codec_id: Some(name.clone()).filter(|n| !n.is_empty()),
            language: None,
            channels: None,
            sample_rate: None,
            duration_secs: None,
            profile: None,
            bit_depth: None,
        });
    }
    for (index, a) in file.audio.iter().enumerate() {
        out.push(StreamFacts {
            kind: StreamKind::Audio,
            index,
            codec_id: Some(a.codec.clone()),
            language: a.language.clone(),
            channels: Some(a.channels).filter(|c| *c > 0),
            sample_rate: Some(a.sample_rate).filter(|r| *r > 0),
            duration_secs: secs(a.duration),
            profile: None,
            bit_depth: pcm_depth(a.codec_id),
        });
    }
    for (index, s) in file.subtitles.iter().enumerate() {
        out.push(StreamFacts {
            kind: StreamKind::Subtitle,
            index,
            codec_id: Some(s.probe_name.to_string()),
            language: s.language.clone(),
            channels: None,
            sample_rate: None,
            duration_secs: None,
            profile: None,
            bit_depth: None,
        });
    }
    Ok(out)
}

/// Every picture of a transport or program stream through ec-cc's extractor for its codec, for
/// the embedded closed captions ([`super::captions`]). Times are microseconds from source time 0
/// (the first picture), the clock every other cue of this file is on. `limit_us` stops the walk
/// once a picture is past it (the cheap presence probe). Empty for a stream whose picture is
/// neither H.264, HEVC nor MPEG-1/2.
pub(crate) fn caption_triplets(path: &Path, limit_us: Option<i64>) -> crate::Result<Vec<ec_cc::TimedCc>> {
    let mut d = open_demuxer(path, Flavor::of(path))?;
    let file = describe_open(&d);
    let Some(video) = file.video else {
        return Ok(Vec::new());
    };
    let kind = d.tracks().into_iter().find(|t| t.stream == Some(video)).map(|t| t.kind);
    enum Ex {
        H264(ec_cc::H264Extractor),
        Hevc(ec_cc::HevcExtractor),
        Mpeg2(ec_cc::Mpeg2Extractor),
    }
    let mut ex = match kind {
        Some(TsKind::H264) => Ex::H264(ec_cc::H264Extractor::new(ec_cc::extract::NalFormat::AnnexB)),
        Some(TsKind::Hevc) => Ex::Hevc(ec_cc::HevcExtractor::new(ec_cc::extract::NalFormat::AnnexB)),
        Some(TsKind::Mpeg1Video | TsKind::Mpeg2Video) => Ex::Mpeg2(ec_cc::Mpeg2Extractor::new()),
        _ => return Ok(Vec::new()),
    };
    d.set_video_framing(VideoFraming::AnnexB);
    loop {
        let p = match d.next_packet() {
            Ok(p) => p,
            Err(ec_core::Error::Eof) => break,
            Err(e) => return Err(ts_error(e)),
        };
        if p.stream != video {
            continue;
        }
        let us = p.pts.or(p.dts).map(|t| (t - file.origin) * 1_000_000 / TICKS);
        if limit_us.is_some_and(|l| us.is_some_and(|u| u > l)) {
            break;
        }
        match &mut ex {
            Ex::H264(e) => e.push(p.data.as_ref(), us),
            Ex::Hevc(e) => e.push(p.data.as_ref(), us),
            Ex::Mpeg2(e) => e.push(p.data.as_ref(), us),
        }
    }
    let rate = (25, 1);
    Ok(match ex {
        Ex::H264(e) => e.finish(rate),
        Ex::Hevc(e) => e.finish(rate),
        Ex::Mpeg2(e) => e.finish(rate),
    })
}
