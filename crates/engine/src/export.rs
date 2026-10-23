//! Export: the edit list rendered back out as one mp4 (H.264, AV1 or HEVC) or
//! one Matroska file (AV1 or HEVC) — or, picture left behind, as one WAV, FLAC,
//! MP3 or Ogg Vorbis of the timeline's audio alone.
//!
//! The HEVC pair codes **I+P GOPs** by default, from `ec-h265`, the project's
//! own pure-Rust HEVC encoder: an IDR a second and predicted pictures between
//! them, rate-controlled when the settings carry a bitrate and at constant QP
//! when they do not. [`ExportSettings::intra_only`] makes every picture an IDR
//! instead -- an intraframe master in the family of ProRes and DNxHD, large
//! files and every frame a cut point. The hardware HEVC seat codes intra only.
//!
//! Picture and sound are both **copied wherever a copy says exactly what the
//! timeline says** — a copy is exact, free and never a generation of loss. For
//! the sound that is packet for packet ([`copy_audio`]); for the picture it is
//! block for block out of a Matroska source into a Matroska file ([`CopyPlan`]),
//! which is what makes a cut in a two-hour film cost the reading of it rather
//! than the re-encoding of it. The picture's copy is all-or-nothing per export,
//! because a copied span and an encoded one cannot share one track: what it
//! needs of a cut is that it lands on a sync point, and everything else — a
//! grade, a speed, a gap, a source of another size or codec, an mp4 — goes
//! through the encoder exactly as it always did. Where the *sound* cannot be
//! copied — a second audio lane to mix in, a speeded clip, an
//! equalized one, a source that is not AAC inside an mp4's own sample table —
//! the sound is decoded, mixed and encoded again with `ec-aac`
//! ([`encode_audio`]). The audio-only formats have no such split: `ec-riff` writes
//! PCM, `ec-flac` encodes FLAC, `ec-mp3` encodes MP3 and `ec-vorbis`
//! encodes Vorbis, so those are always *decoded* out of the timeline. The worker owns everything: the caller gets an
//! [`ExportHandle`] and polls it from its render loop.
//!
//! **Every video format here carries the timeline's sound**, in whichever
//! container: the mp4 muxer writes an AAC track and so does the Matroska one.
//! Nothing is written picture-only and nothing is left silent without saying so
//! — the only sound that does not come out is sound this engine cannot decode at
//! all (Opus and AC-3 inside a Matroska file), and that is an error by name.
//!
//! A **speeded** clip splits along the same line, and for the same reason. The
//! picture is re-encoded, so the frame walk honours a rate: it takes the source
//! frame each timeline frame shows ([`crate::project::Speed::source_at`], the
//! very frame preview holds there) and encodes a held one again rather than
//! decoding it twice. A clip whose *file* was shot at another rate than the
//! timeline goes through the same walk with one more conversion on the end
//! ([`crate::project::Rate`]), so it is encoded at the speed it was shot at.
//! The sound is *copied* unless something forces a decode, and a packet carries
//! no rate — so a speeded lane is one of the things that forces one
//! ([`copy_audio`]) rather than being written out at 1.00x under a re-timed
//! picture. The audio-only formats are decoded and resampled, so they honour a
//! rate like the picture does.
//!
//! Nothing partial survives a failure: the worker writes to `<out>.part` and
//! renames it onto `out` only once the file is closed and complete, so the
//! output either does not exist or is finished — there is no window where a
//! half-written `.export.mp4` is sitting there looking playable. Cancel and
//! every error path delete the `.part`. A killed *process* leaves the `.part`
//! behind (only in-process cleanup is promised), which is an orphan a user can
//! delete rather than a file that plays for two seconds and stops.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use ec_av1::encode::Picture as Av1Picture;
use ec_av1::stream::decode_stream_with;
use ec_core::frame::VideoFrame;
use ec_core::registry::Decoder as _;
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, MediaParameters, StreamInfo};
use ec_core::{Buf, ChannelLayout, Error as Av1Error, Muxer as _, Packet, TimeBase};
use ec_h264::{Encoder, EncoderConfig, H264Decoder, PictureView, Preset};
use ec_h265::encoder::{Encoder as HevcEncoder, EncoderConfig as HevcEncoderConfig, RateControl};
use ec_h265_syntax::vui::{
    ColourDescription as HevcColourDescription, VideoSignalType as HevcVideoSignalType,
};

use crate::audio::{AudioMeta, AudioSession};
use crate::colorspace::{Chroma, ColorDescription, Matrix, Tags, Transfer};
use crate::decode::{I420, Pulled, Wide};
use crate::demux::{Codec, Demuxer, MkvDemuxer, Rotation, VideoMeta};
use crate::hw::{DmaFrame, DmaWant, HwEncoder, HwPicture, HwSession};
use crate::mux::{
    AudioParams, Av1Params, CopyParams, HevcParams, MkvMuxer, Mp4Muxer, SubParams, VideoParams,
};
use crate::project::{Deinterlace, Lane, LaneKind, Project, Rate, Speed, Title};
use crate::scale::{Composer, FitPolicy, Sample};
use crate::subtitle::{Cue, SubtitleTrack};
use crate::text::{Overlay, Place, TextStyle, overlay, overlay_blocks};
use crate::titleanim::Motion;
use crate::tonemap::{self, ToneMapper};
use crate::transform::TransformParams;

/// Progress is reported in permille: an atomic integer the render loop can read
/// without a lock, fine enough for any progress bar.
pub(crate) const PROGRESS_SCALE: u32 = 1_000;

/// The head of the bar the *sound* owns, before a picture is written at all.
/// The mix is decoded, encoded and measured before the muxer exists ([`run`]
/// says why), and on a feature film that is a minute or two in which a bar
/// pinned at zero is the only thing on screen -- which reads as an export that
/// has hung. Small, because the picture is still nearly all of the work.
pub(crate) const AUDIO_BAND: u32 = PROGRESS_SCALE / 20;

/// Where the bar stands with `done` of `total` pictures written: the sound's
/// band is already behind it ([`AUDIO_BAND`]), so the picture fills what is
/// left rather than starting again from zero.
fn picture_progress(done: u32, total: u32) -> u32 {
    // In `u64`: this is a picture count times a permille, and a film past
    // ~4.5 M pictures (42 hours at 30 fps) overflowed the `u32` this used to be
    // computed in -- a panic in debug inside the export thread, which a caller
    // reads as `export worker panicked` on a file that was never written, and a
    // bar that thrashed in release. The result is under the scale by
    // construction, so the narrowing back is exact.
    let band = u64::from(PROGRESS_SCALE - AUDIO_BAND);
    let done = u64::from(done.min(total));
    AUDIO_BAND + (done * band / u64::from(total.max(1))) as u32
}

/// D2 rate control: bits per pixel per second, then a sane range. 720p30 lands
/// at 2.76 Mbps, which S1 measured the software encoder hitting within 1%.
const BITS_PER_PIXEL: f64 = 0.1;
/// The floor under a rate somebody *asked* for, and under the derived one: 100
/// kbps, so a sub-Mbps budget -- a lo-fi delivery, a tiny picture, a
/// phone-rate master -- is a rate this export is written at. A megabit was the
/// old floor, and it silently rewrote every such ask. Nothing below this is
/// worth a container: the sound's own row starts at 128 kbps.
const MIN_BITRATE: u64 = 100_000;
const MAX_BITRATE: u64 = 20_000_000;

/// The ceiling on a bitrate somebody *asked* for, which is a different question
/// from where the derived one stops: [`MAX_BITRATE`] is the top of the automatic
/// figure's sane range, and a rate a person typed is not that program's guess to
/// bound. Nothing in either encoder objects to it -- the VA-API seat takes a
/// `u64` and the software ones a `u32`/`i32`, all of which hold this with room
/// -- so the old shared ceiling was only ever the auto range wearing two hats.
const MAX_EXPLICIT_BITRATE: u64 = 50_000_000;

/// The rates the sound may be coded at, smallest first -- what a front-end
/// offers and what [`ExportSettings::audio_kbps`] is clamped into. Every one of
/// them is a legal MPEG-1 Layer III value, so the MP3 path writes the figure it
/// was given rather than the nearest one `ec-mp3` snaps to.
pub const AUDIO_KBPS: [u32; 4] = [128, 192, 256, 320];

/// What the sound is coded at when nobody picked: the rate this project wrote
/// before the choice existed. 256 and not the encoders' own 128, because an
/// export is a master a person edits from again and this is a second lossy
/// generation over sources that may already have been one -- measured on the
/// suite's fixtures, an untouched clip re-encoded moves 0.15 dB here against
/// 0.46 dB at 128, and 32 KB/s beside a 2.76 Mbps picture is not a size anyone
/// is counting.
pub const DEFAULT_AUDIO_KBPS: u32 = 256;

/// `rav1e`'s fastest preset. Anything slower is minutes per second of timeline
/// on a build with no assembly, which is what this one is.
const AV1_SPEED: u8 = 10;

/// Tiles per software AV1 frame, the knob that spreads one picture over the
/// cores (see `Enc::open_av1`). A power of two; `rav1e` rounds it to a legal
/// layout for the frame size.
const AV1_TILES: usize = 32;

/// How many lane encoders the intra HEVC seat spreads frames across. Twelve
/// is where the 2026-08-11 bench stopped scaling on this box; more lanes
/// only holds more decoded frames in memory. The cores the lanes leave over
/// go to the wavefront inside each lane's encoder.
const HEVC_LANES: usize = 12;
/// Bits per pixel an intra HEVC encoder spends at QP 27: 0.607 at 720p and
/// 0.586 at 1080p on the old seat's bench, and `ec-h265` lands on the same
/// 0.6 anchor. What [`hevc_qp`] maps a bitrate row through.
const HEVC_BPP_AT_27: f64 = 0.6;
/// The band [`hevc_qp`] is clamped into, for the reason stated there.
const HEVC_QP_MIN: i32 = 22;
const HEVC_QP_MAX: i32 = 40;
/// The quantiser the I+P GOP seat codes at when no bitrate is given: its
/// automatic quality, a constant-quality default in its own right. It is not
/// [`hevc_qp`]: that maps the automatic *bitrate* through the intra anchor
/// ([`HEVC_BPP_AT_27`]), which is measured on intra pictures. P pictures cost
/// far fewer bits, so the same mapping pushed a 1080p30 GOP export to the clamp
/// at QP 40 (652 kb/s at 31.8 dB mean PSNR on a 33 s timeline).
///
/// 28 is chosen so that a default is never visibly worse than the H.264 default
/// on any source, and it matches x265's CRF 28 convention. Measured on 1080p30
/// 10 s sources (kbps, PSNR dB against the source):
///
/// | source   | H.264 default | QP 28          | QP 31          |
/// |----------|---------------|----------------|----------------|
/// | testsrc2 | 6298, 44.08   | 6390, 41.84    | 4080, 39.50    |
/// | mandel   | 6246, 35.98   | 11365, 38.37   | 6742, 36.21    |
///
/// At QP 28 the GOP default is 2.2 dB under H.264 on testsrc2 at equal size
/// (+1.5%), and 2.4 dB above it on mandel at +82% bytes. QP 31 is 4.6 dB under
/// on testsrc2, which is visibly worse on synthetic content, so it is not the
/// default. File size is traded for never being far below H.264 quality: on
/// natural content the GOP default is larger than the H.264 file.
const HEVC_GOP_QP: i32 = 28;

/// What an export writes. H.264-in-mp4, AV1 and HEVC -- each of the last two in
/// Matroska or in mp4, the user's pick of container -- are the *video* pairs
/// with both an encoder and a decoder under this project's no-install rule, and
/// pure-Rust encoder: `ec-riff`, `ec-flac`, `ec-mp3` and `ec-vorbis`. Opus
/// still has none as a *file* -- the Matroska seat is `ec-opus`, but a standalone
/// `.opus` row this project does not offer.
/// (AAC has one too -- `ec-aac`, which is what a re-encoded video track's
/// sound leaves through -- but AAC is a container's own audio, never a file of
/// its own here.) VP9 and VP8 are the
/// codecs that come in through the plugin and stop there, HEVC having
/// gained an encoder (an intra-only one, which the rows say). A front-end says
/// so rather than hiding the row.
///
/// **Every video format carries sound.** Which way it carries it is the only
/// difference: copied packet for packet where a copy can say what the timeline
/// says, decoded and encoded again ([`encode_audio`]) where it cannot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// Picture and sound: video re-encoded, AAC copied -- or re-encoded too,
    /// where a copy could not carry the edit (a second audio lane to mix in, a
    /// speeded clip, an equalized one, a source no mp4 sample table holds).
    #[default]
    Mp4,
    /// AV1 in Matroska, with the timeline's AAC beside it: this engine's
    /// Matroska writer carries an audio track ([`crate::mux::MkvMuxer`]) and its
    /// reader reads one back, symphonia's `mkv` reader being what
    /// `audio::Track::open` opens it with.
    Av1,
    /// The same AV1 stream in an mp4, for everything that plays mp4 and not
    /// Matroska. The sample entry `mp4 0.14` cannot write is written by hand
    /// ([`crate::mux::Mp4Muxer::create_av1`]); the sound is the mp4 path's own,
    /// unchanged.
    Av1Mp4,
    /// HEVC in Matroska: I+P GOPs from `ec-h265` ([`Enc::open_hevc`]), or with
    /// [`ExportSettings::intra_only`] an intraframe master, every frame an IDR.
    Hevc,
    /// The same HEVC stream in an mp4 (`hvc1`), for everything that plays mp4
    /// and not Matroska -- the AV1 pair's split, for the same reason.
    HevcMp4,
    /// AV1 in WebM, with Opus beside it: the open web's container. The same
    /// AV1 encoder as [`Format::Av1`]; the sound is always Opus, mixed at 48 kHz
    /// ([`start`]) because WebM carries no other audio, and the file carries no
    /// subtitles ([`planned_subtitles`] says so). No copy path: a source's own
    /// packets are not AV1 or Opus in general.
    Webm,
    /// The audio lanes alone, 16-bit PCM.
    Wav,
    /// The audio lanes alone, losslessly compressed.
    Flac,
    /// The audio lanes alone, MPEG-1 Layer III CBR (`ec-mp3`) at the rate
    /// [`ExportSettings::audio_kbps`] asks for.
    Mp3,
    /// The audio lanes alone, Vorbis I in an Ogg container -- `ec-vorbis`
    /// encoding, `ec-ogg` paging. Quality-coded and not rate-coded, which
    /// is why [`audio_rate_refusal`] takes the Sound row away here: see
    /// [`VORBIS_QUALITY`] for the operating point and how it was measured.
    Ogg,
    /// The audio lanes alone as AAC in an mp4 box with the `M4A ` brand -- the
    /// file a player picks an audio parser for. The same track an mp4 of a
    /// timeline with no picture carries ([`run_aac`]), which is an [`Format::Mp4`]
    /// and not this one: the difference is the brand alone.
    M4a,
    /// The audio lanes alone as Opus in Ogg (RFC 7845), mixed at 48 kHz and
    /// coded by `ec-opus` at [`ExportSettings::audio_kbps`]
    /// ([`crate::opus_write`]).
    Opus,
    /// Apple ProRes in a QuickTime `.mov`, the profile in
    /// [`ExportSettings::prores_profile`]: always 10-bit, software (`ec-prores`),
    /// every frame a key frame, progressive, no alpha. The sound is 16-bit
    /// little-endian PCM (`sowt`) at the timeline's rate, not AAC. See
    /// [`crate::prores_out`] for the chroma upsampling the 4:2:0 compositor needs.
    ProRes,
    /// A numbered run of PNG pictures, one per timeline frame of the range (or the
    /// whole timeline): `<folder name>_00001.png` onward, written into the folder the
    /// export names. No sound. Each picture is the project's own size, with titles and
    /// burnt captions as the export draws them ([`compose_at_with`]).
    PngSequence,
    /// One PNG picture: the frame at the start of the range (frame 0 without one), at
    /// the project's own size. The poster frame. No sound.
    Still,
    /// An animated GIF of the range, at [`ExportSettings::gif`]. Not written yet: the
    /// GIF encoder is still to come, so this format refuses by name ([`GifWriter`]).
    Gif,
}

impl Format {
    /// The extension a file of this format is named with -- a front-end builds
    /// the destination path from it, so the name never disagrees with the bytes.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 | Self::Av1Mp4 | Self::HevcMp4 => "mp4",
            Self::Av1 | Self::Hevc => "mkv",
            Self::Webm => "webm",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
            Self::Ogg => "ogg",
            Self::M4a => "m4a",
            Self::Opus => "opus",
            Self::ProRes => "mov",
            Self::PngSequence | Self::Still => "png",
            Self::Gif => "gif",
        }
    }

    /// Whether this format carries the picture. The bitrate settings are video
    /// settings and mean nothing to the audio-only ones.
    pub fn has_video(self) -> bool {
        matches!(
            self,
            Self::Mp4 | Self::Av1 | Self::Av1Mp4 | Self::Hevc | Self::HevcMp4 | Self::Webm | Self::ProRes
        )
    }

    /// Whether the picture in it is AV1 rather than H.264 -- which encoder runs
    /// and which of the two AV1 containers is being written are separate
    /// questions, and this is the first.
    pub fn is_av1(self) -> bool {
        matches!(self, Self::Av1 | Self::Av1Mp4 | Self::Webm)
    }

    /// The same question for HEVC, and the same answer for both its containers:
    /// one encoder, two boxes to put its stream in.
    pub fn is_hevc(self) -> bool {
        matches!(self, Self::Hevc | Self::HevcMp4)
    }

    /// Whether the picture is ProRes in a QuickTime file ([`Format::ProRes`]).
    pub fn is_prores(self) -> bool {
        self == Self::ProRes
    }

    /// Whether the file is a Matroska one. Only that container carries this
    /// project's Opus track ([`encode_opus`]): an mp4 may hold Opus by spec, but
    /// the crate that writes the mp4s here has no sample entry for it, so the
    /// mp4 formats stay on AAC and say so.
    fn is_mkv(self) -> bool {
        matches!(self, Self::Av1 | Self::Hevc | Self::Webm)
    }

    /// What the format is called where a refusal names it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Mp4 => "an mp4",
            Self::Webm => "a WebM",
            Self::Av1 => "an AV1 Matroska",
            Self::Av1Mp4 => "an AV1 mp4",
            Self::Hevc => "an HEVC Matroska",
            Self::HevcMp4 => "an HEVC mp4",
            Self::Wav => "a WAV",
            Self::Flac => "a FLAC",
            Self::Mp3 => "an MP3",
            Self::Ogg => "an Ogg Vorbis",
            Self::M4a => "an M4A",
            Self::Opus => "an Ogg Opus",
            Self::ProRes => "a ProRes QuickTime",
            Self::PngSequence => "a PNG sequence",
            Self::Still => "a PNG still",
            Self::Gif => "an animated GIF",
        }
    }
}

/// Which encoder writes the picture, as a person picks it on a card. Three
/// discrete choices and not a switch, because "whatever is best" and "the GPU,
/// and say so if there is none" are different answers: a driver that encodes
/// badly wants [`Software`](Self::Software), and a person who *needs* the GPU
/// (a long delivery, a machine whose CPU is doing something else) wants to be
/// told when it is not there rather than to wait out a silent fallback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EncoderSeat {
    /// Hardware where this machine has a seat for the picked codec, software
    /// everywhere else -- what every export did before there was a choice, and
    /// still what an unpicked project does.
    #[default]
    Auto,
    /// The GPU or nothing: no silent fallback, and [`Enc::open`] refuses in
    /// words before a byte is written. The only way to the plugin's AV1 seat,
    /// which [`Auto`](Self::Auto) will not enter (see [`Enc::open_av1`]).
    Hardware,
    /// The software encoder even where a seat exists -- for a driver that
    /// encodes badly, and for the AV1 pair, where it is what `Auto` picks too.
    Software,
}

impl EncoderSeat {
    pub const ALL: [EncoderSeat; 3] = [Self::Auto, Self::Hardware, Self::Software];

    /// What a card row and a project file call it. The file's keyword too, so
    /// the two can never drift ([`crate::edith`]).
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Hardware => "hardware",
            Self::Software => "software",
        }
    }

    /// Reads one back. `None` for anything else, which a project file refuses
    /// by name rather than silently exporting on another seat.
    pub fn from_name(name: &[u8]) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name().as_bytes() == name)
    }
}

/// The six ProRes profiles ([`Format::ProRes`]). The first four code 4:2:2, the
/// last two 4:4:4; the encoder's rate is Apple's nominal one for the profile and
/// the picture size, so a profile *is* the quality row ([`ExportSettings::bitrate`]
/// means nothing to ProRes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProResProfile {
    Proxy,
    Lt,
    Standard,
    #[default]
    Hq,
    P4444,
    P4444Xq,
}

impl ProResProfile {
    pub const ALL: [ProResProfile; 6] =
        [Self::Proxy, Self::Lt, Self::Standard, Self::Hq, Self::P4444, Self::P4444Xq];

    /// What a card row calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Proxy => "ProRes 422 Proxy",
            Self::Lt => "ProRes 422 LT",
            Self::Standard => "ProRes 422",
            Self::Hq => "ProRes 422 HQ",
            Self::P4444 => "ProRes 4444",
            Self::P4444Xq => "ProRes 4444 XQ",
        }
    }

    /// Whether the profile codes full-resolution chroma (4:4:4).
    pub fn is_444(self) -> bool {
        matches!(self, Self::P4444 | Self::P4444Xq)
    }

    fn codec(self) -> ec_prores::Profile {
        match self {
            Self::Proxy => ec_prores::Profile::Proxy,
            Self::Lt => ec_prores::Profile::Lt,
            Self::Standard => ec_prores::Profile::Standard,
            Self::Hq => ec_prores::Profile::Hq,
            Self::P4444 => ec_prores::Profile::P4444,
            Self::P4444Xq => ec_prores::Profile::P4444Xq,
        }
    }
}

/// The HDR transfer an HEVC export is written on ([`HdrOutput::transfer`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HdrTransfer {
    /// No HDR: the export is SDR as it always was (tone-mapped from HDR sources).
    #[default]
    Sdr,
    /// SMPTE ST 2084 (HDR10): BT.2020 primaries and matrix, limited range, 10-bit.
    Pq,
    /// ARIB STD-B67 (HLG): BT.2020 primaries and matrix, limited range, 10-bit.
    Hlg,
}

impl HdrTransfer {
    pub const ALL: [HdrTransfer; 3] = [Self::Sdr, Self::Pq, Self::Hlg];

    pub fn name(self) -> &'static str {
        match self {
            Self::Sdr => "SDR",
            Self::Pq => "PQ (HDR10)",
            Self::Hlg => "HLG",
        }
    }

    fn colour(self) -> Option<ColorDescription> {
        let transfer = match self {
            Self::Sdr => return None,
            Self::Pq => Transfer::Pq,
            Self::Hlg => Transfer::Hlg,
        };
        Some(ColorDescription { matrix: Matrix::Bt2020Ncl, transfer, full_range: false })
    }
}

/// A SMPTE ST 2086 mastering display, in the units the SEI states: chromaticities
/// in 0.00002 steps, luminance in 0.0001 cd/m2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasteringDisplay {
    /// Green, blue, red `(x, y)`, in that order (the SEI's).
    pub primaries: [(u16, u16); 3],
    pub white_point: (u16, u16),
    /// Peak luminance, 0.0001 cd/m2 (1000 nits = 10_000_000).
    pub max_luminance: u32,
    /// Black level, 0.0001 cd/m2 (0.005 nits = 50).
    pub min_luminance: u32,
}

impl MasteringDisplay {
    /// A BT.2020-primaries, D65 display with the given peak and black, in nits.
    pub fn bt2020(max_nits: f32, min_nits: f32) -> Self {
        Self {
            primaries: [(8500, 39850), (6550, 2300), (35400, 14600)],
            white_point: (15635, 16450),
            max_luminance: (max_nits * 10_000.0).round() as u32,
            min_luminance: (min_nits * 10_000.0).round() as u32,
        }
    }

    /// A DCI-P3 (D65) display, the usual HDR10 grading monitor, with the given
    /// peak and black in nits.
    pub fn p3_d65(max_nits: f32, min_nits: f32) -> Self {
        Self {
            primaries: [(13250, 34500), (7500, 3000), (34000, 16000)],
            white_point: (15635, 16450),
            max_luminance: (max_nits * 10_000.0).round() as u32,
            min_luminance: (min_nits * 10_000.0).round() as u32,
        }
    }
}

/// HDR output of an HEVC export ([`ExportSettings::hdr`]). The default is none:
/// every export that does not set it is the SDR file it always was.
///
/// With a transfer set the file is 10-bit (an explicit
/// [`ExportSettings::video_bit_depth`] of 8 is refused), tagged BT.2020 primaries
/// and matrix with that transfer in the bitstream VUI, the mp4 `colr` box or the
/// Matroska `Colour` element, and the HDR sources are *not* tone-mapped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HdrOutput {
    pub transfer: HdrTransfer,
    /// Written as a mastering display colour volume SEI (in the bitstream).
    pub mastering_display: Option<MasteringDisplay>,
    /// MaxCLL in cd/m2, with `max_fall`, written as a content light level SEI.
    pub max_cll: Option<u16>,
    /// MaxFALL in cd/m2.
    pub max_fall: Option<u16>,
}

/// What the caller gets to decide about the output. The codec is not among it:
/// [`Format`] names a container *and* what goes in it, because every pair we
/// can write is a pair we can also read back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportSettings {
    /// Bits per second, clamped to the same sane range the automatic value uses.
    /// `None` picks it from the picture size and frame rate. Video only.
    pub bitrate: Option<u64>,
    /// Which encoder writes the picture ([`EncoderSeat`]). Was a `force_sw`
    /// flag, which could say "not the GPU" and never "the GPU or say so".
    /// [`EncoderSeat::Software`] is what the `VE_SW_ENC` env pin still forces
    /// on every export of a run.
    pub seat: EncoderSeat,
    /// The file to write. Defaults to [`Format::Mp4`], which is what every
    /// caller wrote before there was a choice.
    pub format: Format,
    /// The sample depth an AV1 software export is written at: `Some(8)` or
    /// `Some(10)`, and `None` follows the source -- 10 where any video source of
    /// the timeline is 10-bit. AV1 and software HEVC (H.264 stays 8-bit, and so do
    /// the hardware AV1 and HEVC seats, which the 10-bit seats do not take). An HDR
    /// transfer ([`ExportSettings::hdr`]) and ProRes are always 10-bit.
    ///
    /// A 10-bit export is decoded at 10 bits and encoded from its 16-bit planes
    /// ([`Enc::encode_wide`]) wherever the span passes its picture through
    /// untouched. A picture composed on the way (a grade, a transform, a fade, a
    /// dissolve, a caption, a tone map, a layer) is composed at 8 bits as it always
    /// was and shifted up to the file's depth, so the file is 10-bit either way.
    pub video_bit_depth: Option<u8>,
    /// What the *sound* is coded at, in kbps, wherever an encoder writes it --
    /// the AAC of a video export as much as an MP3. `None` is
    /// [`DEFAULT_AUDIO_KBPS`], which is what every caller wrote before there
    /// was a choice, so a settings value nobody touched writes the same bytes
    /// it used to. Means nothing to WAV and FLAC, which code no rate, and
    /// nothing to a *copied* AAC track, which carries its source's.
    pub audio_kbps: Option<u32>,
    /// Bits per sample of the PCM sound in a ProRes `.mov`: 16 or 24. `None` (and
    /// anything else) is 24. No other format reads it.
    pub audio_bit_depth: Option<u8>,
    /// Which of the project's subtitle tracks travel with the file: indices into
    /// [`Project::subtitles`], which is the same list a front-end picks rows of.
    /// As many as the timeline holds, in the order given -- the file declares
    /// them in that order and a player's subtitle menu lists them in it. Empty
    /// writes none -- what every caller wrote before there was a choice, so an
    /// export of a timeline without subtitles is the file it always was, byte
    /// for byte.
    ///
    /// Every container that carries picture carries them: Matroska as
    /// `S_TEXT/UTF8` blocks, an mp4 as a `tx3g` timed-text track
    /// ([`crate::mux::Mp4Muxer::write_subtitles`]). A Matroska block names its
    /// track in one byte, so a Matroska file carries at most
    /// [`crate::mux::MAX_SUB_TRACKS`] of them and asking for more is refused by
    /// name rather than truncated; an mp4 numbers its tracks in 32 bits and has
    /// no such ceiling.
    pub subtitles: Vec<usize>,
    /// Writes no soft subtitle track at all -- not the lanes, not the picks. The
    /// captions a file carries are then only the ones [`burn_subtitles`] drew
    /// into its picture. `false` is what every caller wrote before there was a
    /// choice, so the lanes and the picks decide as they always did.
    ///
    /// [`burn_subtitles`]: ExportSettings::burn_subtitles
    pub no_soft_subtitles: bool,
    /// Code every picture as a key frame a decoder may be started from: no
    /// frame references another, the file is an intraframe master in the shape
    /// of ProRes, and a seek to any frame at all is one decode.
    ///
    /// Bigger files for that, so it is off for delivery and on for the
    /// stand-ins [`crate::proxy`] makes, which exist precisely to be seeked
    /// through. This is the switch of the H.264 and HEVC pairs; on HEVC the
    /// software seat then codes every picture an IDR ([`Enc::open_hevc`]) and
    /// the hardware seat is already intra. AV1 has no such switch.
    pub intra_only: bool,
    /// Captions drawn into the picture: the track at this index in
    /// [`Project::subtitles`] (the index [`ExportSettings::subtitles`] and
    /// [`crate::PlaybackSession::timeline_cues`] take), its cue on screen at
    /// each timeline frame, drawn in this style ([`crate::text`]). `None` writes
    /// the picture as it is -- what every caller wrote before there was a
    /// choice.
    ///
    /// A burned-in picture is never copied out of its source: it is decoded,
    /// drawn and encoded, so [`exact_refusal`] names the copy it gives up. The
    /// cues are the ones the timeline plays, so a rippled edit captions what it
    /// shows, the way the soft track does.
    pub burn_subtitles: Option<(usize, TextStyle)>,
    /// Write the picture in the space its **source** declares -- no tone map to
    /// SDR, no matrix conversion -- and tag the file with it, instead of
    /// converting everything into the space the output height implies.
    ///
    /// For a preview file and nothing else ([`crate::proxy`]): it makes the
    /// stand-in a smaller copy of the film rather than a rendition of it, so
    /// the screen tone-maps it live with whatever preset the project is set to
    /// -- and it takes the per-frame conversion off the *source's* full
    /// resolution, which on a 4K HDR film was 48 ms a frame and the whole of a
    /// proxy's cost (measured 2026-08-13).
    ///
    /// A delivery export must leave it off: an mp4 in BT.2020 PQ is a file for
    /// this editor to read back, not one to hand anybody.
    pub keep_source_colour: bool,
    /// Half-open `[start, end)` in timeline frames, or `None` for the whole
    /// timeline -- what every caller wrote before there was a choice, so an
    /// export nobody has marked in or out on writes the same bytes it always
    /// did. Session state, like the mark itself: never saved to a `.edith`.
    pub range: Option<(u32, u32)>,
    /// The Exact quality row: this file must be the source's own coded
    /// picture, copied wherever [`CopyPlan`] already says a copy is exactly
    /// what the timeline says. `bitrate` and `seat` mean nothing to a copied
    /// track, but a fallback re-encode (a cut off a sync point, a grade, a
    /// speed, a transform) still honours them, so a copy-minded caller sends
    /// `bitrate: None` beside this and gets the automatic figure rather than
    /// a preset's.
    ///
    /// [`exact_refusal`] is the gate a front-end asks before the button is
    /// pressed; [`run`] asks it again, which is what makes this a setting a
    /// worker can trust rather than one only the card checked.
    pub exact: bool,
    /// The picture's size in pixels for this export alone, `(width, height)`:
    /// the canvas the file is drawn and encoded at, in place of the project's
    /// resolution. The project itself is not touched -- [`start`] applies it to
    /// the export's own copy of the settings, and every clip refits on that
    /// canvas by its fit mode, as it does when the project's resolution changes.
    /// `None` is the project's own size, as every caller wrote before there was
    /// a choice. Each side must be a picture size ([`crate::is_resolution`]) and
    /// even, which the caller checks: a size here is trusted as given.
    pub size: Option<(u32, u32)>,
    /// Chapter marks the file carries, `(start in ns, title)` each, sorted by start. An mp4 gets
    /// them as a Nero `chpl` and a QuickTime chapter track, a Matroska file as a `Chapters`
    /// element. Empty writes none, which is what every caller wrote before there was a choice.
    pub chapters: Vec<(u64, String)>,
    /// Level the mixed sound to this integrated loudness under this true-peak ceiling before it
    /// is coded ([`LoudnessTarget`]). `None` writes the mix at the level it was heard at, which
    /// is what every caller wrote before there was a choice. A track is never *copied* when this
    /// is set: a packet copy carries no gain.
    pub loudness: Option<LoudnessTarget>,
    /// The ProRes profile of a [`Format::ProRes`] export; means nothing to any
    /// other format. Defaults to [`ProResProfile::Hq`].
    pub prores_profile: ProResProfile,
    /// HDR output of an HEVC export ([`Format::Hevc`], [`Format::HevcMp4`]); see
    /// [`HdrOutput`]. Refused for every other format. The default is none.
    pub hdr: HdrOutput,
    /// Speed preset of the software HEVC encoder ([`Format::Hevc`], [`Format::HevcMp4`]); means
    /// nothing to any other format. `None` is [`HevcSpeed::Default`], the encoder's own search,
    /// bit-identical to every export written before there was a choice.
    pub hevc_speed: Option<HevcSpeed>,
    /// What a GIF asks for beyond the picture ([`Format::Gif`]); means nothing to any other format.
    pub gif: GifSettings,
}

/// The settings of an animated GIF ([`Format::Gif`]): its width in pixels (the height follows
/// the project's aspect), its frame rate, and whether it loops. The default is the size and rate
/// a GIF is usually shared at: 480 pixels wide, 15 frames a second, looping forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GifSettings {
    pub width: u32,
    pub fps: u32,
    pub loop_forever: bool,
}

impl Default for GifSettings {
    fn default() -> Self {
        Self { width: 480, fps: 15, loop_forever: true }
    }
}

/// Speed preset of the software HEVC encoder (`ec-h265`'s `Preset`). `Fast` costs about 0-3%
/// BD-rate for 1.2-1.6x speed, `Faster` up to 27% for about 1.5-2x.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HevcSpeed {
    #[default]
    Default,
    Fast,
    Faster,
}

impl HevcSpeed {
    fn preset(self) -> ec_h265::encoder::Preset {
        match self {
            Self::Default => ec_h265::encoder::Preset::Default,
            Self::Fast => ec_h265::encoder::Preset::Fast,
            Self::Faster => ec_h265::encoder::Preset::Faster,
        }
    }
}

/// An export-time loudness normalisation: one gain over the whole mix, from
/// [`crate::loudness::gain_to_target`], so the integrated loudness lands on `lufs` and the true
/// peak is held at or under `true_peak_dbtp` (the gain is cut back, never a limiter).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoudnessTarget {
    pub lufs: f64,
    pub true_peak_dbtp: f64,
}

// Plain finite figures, never NaN: the settings that hold one are `Eq`.
impl Eq for LoudnessTarget {}

/// Measures `samples` (interleaved) and scales them by the gain that reaches `target`.
fn level_to_target(samples: &mut [f32], rate: u32, channels: u16, target: LoudnessTarget) {
    let mut meter = crate::loudness::Meter::new(rate, channels);
    meter.push(samples);
    let gain = crate::loudness::gain_to_target(&meter.finish(), target.lufs, target.true_peak_dbtp);
    if gain != 0.0 {
        let g = 10f32.powf(gain as f32 / 20.0);
        for s in samples.iter_mut() {
            *s *= g;
        }
    }
}

/// The export's picture size: the project's canvas, or the [`ExportSettings::size`]
/// this export is asked to draw at. Everything that decides what the file's picture
/// is (the encoder's seat, the copy's gate, the colour matrix) asks it through here,
/// so the answer each gives is the one the file has.
pub fn output_meta(meta: &VideoMeta, settings: &ExportSettings) -> VideoMeta {
    match settings.size {
        // The coded size goes with the drawn one: a copy's gate compares the coded pair with its
        // source's, and a picture drawn at another size is not a copy of anything.
        Some((width, height)) => VideoMeta {
            width,
            height,
            coded_width: width,
            coded_height: height,
            ..*meta
        },
        None => *meta,
    }
}

/// What [`fit_picture_clips`] did: the full-frame clips it put on the policy, per lane (a lane
/// with none is left out), and how many transformed overlays it left on their own fit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FitOutcome {
    pub lanes: Vec<(Lane, usize)>,
    pub overlays: usize,
}

/// Puts every full-frame picture clip of `project` on `fit`, in place. Meant for the export's own
/// copy of the timeline ([`start`] takes it by value), so the project a user edits keeps its
/// clips' fit modes. A clip refits on the export's canvas by its fit mode, so this is how an
/// export asks for `fit` on the pictures that fill the frame.
///
/// A clip placed by a transform (moved, scaled, cropped, faded or animated) is an overlay and
/// keeps its own fit: `fill` would crop it to the canvas's shape, so a logo would lose its
/// aspect on a vertical canvas. Its size stays relative to the canvas width, as in the project.
/// The whole change is one undo step on the copy, which nothing reads.
pub fn fit_picture_clips(project: &mut Project, fit: FitPolicy) -> FitOutcome {
    let videos: Vec<Lane> = project.lanes().into_iter().filter(|l| l.kind == LaneKind::Video).collect();
    project.begin_batch();
    let mut outcome = FitOutcome::default();
    for lane in videos {
        let mut count = 0;
        for idx in 0..project.lane(lane).len() {
            if is_placed(project, lane, idx) {
                outcome.overlays += 1;
            } else {
                project.set_fit(lane, idx, fit);
                count += 1;
            }
        }
        if count > 0 {
            outcome.lanes.push((lane, count));
        }
    }
    project.end_batch();
    outcome
}

/// Whether the clip at `idx` of `lane` is placed by a transform that is not the identity, or has
/// a placement animation: the overlays [`fit_picture_clips`] leaves alone.
fn is_placed(project: &Project, lane: Lane, idx: usize) -> bool {
    project.transform_of(lane, idx).is_some_and(|t| !t.is_identity())
        || !project.transform_keys_of(lane, idx).is_empty()
}

/// The first moment a burned caption and a title show together in the same place. `cues` are the
/// burn's cues ([`burn_cues`]) and `place` the burn's own place; a title sits where its
/// `style.place` says, so a clash is a caption and a title with the same place on screen at once.
/// Returns the moment in seconds and the index into `titles` of the title, the earliest such
/// moment; `None` when no caption and title clash.
pub fn caption_title_clash(cues: &[Cue], titles: &[Title], place: Place, fps: f64) -> Option<(f64, usize)> {
    let us = |frame: u32| (f64::from(frame) / fps * 1e6).round() as i64;
    let mut first: Option<(i64, usize)> = None;
    for (i, t) in titles.iter().enumerate().filter(|(_, t)| t.style.place == place) {
        let (start, end) = (us(t.start), us(t.end()));
        for c in cues.iter().filter(|c| c.image.is_none() && !c.text.trim().is_empty()) {
            let lo = c.start_us.max(start);
            if lo < c.end_us.min(end) && first.is_none_or(|(f, _)| lo < f) {
                first = Some((lo, i));
            }
        }
    }
    first.map(|(lo, i)| (lo as f64 / 1e6, i))
}

/// The lowest H.264 level (Table A-1) whose frame size and macroblock rate
/// hold `width`x`height` at `fps_num/fps_den`, as the `level_idc` byte; 62
/// past the table. One table for both seats: the software encoder writes it
/// into its SPS (its default is 3.0, which is out of spec above 720p30) and
/// the plugin hands it to the driver.
pub fn h264_level_idc(width: u32, height: u32, fps_num: u32, fps_den: u32) -> u8 {
    const LEVELS: [(u8, u64, u64); 17] = [
        (10, 1485, 99),
        (11, 3000, 396),
        (12, 6000, 396),
        (13, 11880, 396),
        (20, 11880, 396),
        (21, 19800, 792),
        (22, 20250, 1620),
        (30, 40500, 1620),
        (31, 108000, 3600),
        (32, 216000, 5120),
        (40, 245760, 8192),
        (41, 245760, 8192),
        (42, 522240, 8704),
        (50, 589824, 22080),
        (51, 983040, 36864),
        (52, 2073600, 36864),
        (60, 4177920, 139264),
    ];
    let frame_mbs = u64::from(width.div_ceil(16)) * u64::from(height.div_ceil(16));
    let fps = f64::from(fps_num.max(1)) / f64::from(fps_den.max(1));
    let mbps = (frame_mbs as f64 * fps).ceil() as u64;
    LEVELS
        .iter()
        .find(|&&(_, max_mbps, max_fs)| mbps <= max_mbps && frame_mbs <= max_fs)
        .map_or(62, |&(idc, ..)| idc)
}

struct Shared {
    /// Fraction of the job done, in [`PROGRESS_SCALE`]ths.
    ///
    /// **Every stage that can run beside another writes this with `fetch_max`,
    /// never `store`.** The sound and the picture are two threads publishing
    /// two bands of one bar ([`AUDIO_BAND`]): a `store` from the lower band
    /// pulls the bar backwards every time the other thread has already moved
    /// it on, which is what a person sees as a progress bar that stutters
    /// backwards -- 5,458 backward steps over one film export before this rule
    /// was written down. A `store` is only correct where nothing else can be
    /// publishing at all, which now means the terminal [`PROGRESS_SCALE`] and
    /// the audio-only export ([`run_audio`]) that has no second thread. The
    /// rule outlives one attempt at the file: an export rerun in software
    /// because the hardware seat died writes this same atomic, and the bar
    /// holds where the dead seat left it rather than starting again.
    progress: AtomicU32,
    cancel: AtomicBool,
    finished: AtomicBool,
    outcome: Mutex<Option<crate::Result<()>>>,
    /// The encoders this job really opened, published the moment they are
    /// chosen and never changed after -- one seat for the whole file, which is
    /// what [`Enc`] states. `None` until then. The one thing that joins it
    /// afterwards is how much of the edit never got a picture
    /// ([`Shared::truncated`]), and [`ExportHandle::encoders`] does that
    /// joining, so a notice cannot land on the line twice.
    encoders: Mutex<Option<String>>,
    /// Timeline frames of the edit the walk could not write because a file ran
    /// out before its clip did ([`run`]'s break). The bar reaches its end
    /// whatever happens, so without this a truncated file reads as a whole one.
    /// Published the moment the walk gives up and read by
    /// [`ExportHandle::encoders`] beside the seat line.
    truncated: AtomicU32,
    /// A stage that runs before a span can encode -- a stabilised clip's shake
    /// being measured ([`crate::stabilise::ensure`]) -- as `1 +` its thousandths
    /// done, `0` while none runs. Its own fraction, beside [`Shared::progress`]
    /// rather than in it, which holds still meanwhile ([`ExportHandle::stage`]).
    measuring: AtomicU32,
    /// [`ExportSettings::loudness`], for the audio stages that only see `Shared`.
    loudness: Option<LoudnessTarget>,
}

/// A running export. Poll [`is_finished`](ExportHandle::is_finished) once per
/// rendered frame and take the [`result`](ExportHandle::result) when it flips;
/// dropping the handle does *not* stop the worker, [`cancel`](ExportHandle::cancel)
/// does.
pub struct ExportHandle {
    shared: Arc<Shared>,
    /// The length of the file this export writes, in seconds: the range's, or the
    /// picture and sound's end for a whole-timeline export. Known before the first frame.
    length_secs: f64,
}

impl ExportHandle {
    /// Fraction of the timeline written, `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        self.shared.progress.load(Ordering::Relaxed) as f32 / PROGRESS_SCALE as f32
    }

    /// The length of the file this export writes, in seconds: the frames it holds at the
    /// canvas's frame rate, which is the file's duration as a player shows it (a range is
    /// cut to the picture's end the same way the encoder cuts it).
    pub fn length_secs(&self) -> f64 {
        self.length_secs
    }

    /// Asks the worker to stop at its next checkpoint and delete the partial
    /// file. The outcome then reports the cancellation as an error. Checkpoints
    /// run to the last instant before the rename, so even a cancel at full
    /// progress leaves no output.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether [`cancel`](Self::cancel) was asked: the worker has not yet reached its
    /// next checkpoint, so the export is still running though it is going to stop.
    pub fn is_cancelling(&self) -> bool {
        self.shared.cancel.load(Ordering::Relaxed)
    }

    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }

    /// What this export is encoding with -- the video seat and what the sound
    /// is doing -- as the worker chose them, hardware fallback included.
    /// `None` for the first instants of a job, before the encoder is open.
    ///
    /// ...and, where a file ran out under its clip, how much of the edit never
    /// got a picture. Composed here rather than written into the stored line,
    /// because that line is published more than once on a rerun (a hardware
    /// seat that died) and a notice appended at the write site lands twice.
    pub fn encoders(&self) -> Option<String> {
        let line = self.shared.encoders.lock().unwrap().clone()?;
        Some(match self.shared.truncated.load(Ordering::Relaxed) {
            0 => line,
            past => format!("{line} · {past} frames of the edit past the last picture"),
        })
    }

    /// The stage the export is in when it is not encoding: its name and its own
    /// fraction done, `0.0..=1.0` -- `("Measuring shake", 0.4)` while a stabilised
    /// clip with no finished analysis is measured. `None` while it encodes, when
    /// [`progress`](Self::progress) is the one to show.
    pub fn stage(&self) -> Option<(&'static str, f32)> {
        match self.shared.measuring.load(Ordering::Relaxed) {
            0 => None,
            n => Some((MEASURING_SHAKE, ((n - 1) as f32 / 1000.0).clamp(0.0, 1.0))),
        }
    }

    /// The outcome, once — taken out of the handle, so a caller that already
    /// reported it sees `None` afterwards. `None` while the export is running.
    pub fn result(&self) -> Option<crate::Result<()>> {
        self.shared.outcome.lock().unwrap().take()
    }
}

/// Starts the worker. Failures are reported through the handle rather than
/// returned, so a caller has exactly one place to look. The files to read are
/// the project's own [`sources`](Project::sources) -- every clip names one --
/// so nothing but the edit list decides what is decoded.
pub fn start(
    project: Project,
    meta: VideoMeta,
    out: &Path,
    settings: &ExportSettings,
    sample_rate: Option<u32>,
) -> ExportHandle {
    let settings = settings.clone();
    // The canvas this export draws at: the project's, unless the settings name another.
    let meta = output_meta(&meta, &settings);
    let media = project.media_frames();
    let frames = match settings.range {
        Some((from, to)) => to.min(media).saturating_sub(from),
        None => media,
    };
    let length_secs = f64::from(frames) / meta.frame_rate;
    let shared = Arc::new(Shared {
        progress: AtomicU32::new(0),
        cancel: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        outcome: Mutex::new(None),
        encoders: Mutex::new(None),
        truncated: AtomicU32::new(0),
        measuring: AtomicU32::new(0),
        loudness: settings.loudness,
    });
    let worker = Arc::clone(&shared);
    // A second handle to the same slot, held outside the caught body below --
    // what settles it if that body never gets to its own [`settle`] call at
    // all, because a codec parser deep inside `run` panicked instead of
    // returning the parse error it should have. Without this a panic there
    // strands the slot: [`ExportHandle::is_finished`] never flips, and a
    // proxy or export that hit a malformed frame sits in "Making" forever.
    let panic_worker = Arc::clone(&worker);
    let out = out.to_path_buf();
    // `<out>.part`, appended rather than substituted: the temporary of
    // `a.export.mp4` is `a.export.mp4.part`, which no other export claims.
    let mut part = out.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let spawned = thread::Builder::new().name("export".into()).spawn(move || {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Every file this export will read, named before a byte of one is.
            // These are the project's own sources and can be nothing else: a
            // stand-in ([`crate::proxy`]) is a substitution playback makes when it
            // opens a span, and nothing here knows such a thing exists. Printed so
            // that "the delivery came off the film itself" is a grep rather than a
            // belief.
            for source in project.sources() {
                if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export source: {}", source.path.display()); }
            }
            // The rename is the last step and the only one that publishes a file
            // under the name the caller asked for; it stays on the same directory,
            // so it is atomic.
            //
            // An emptied timeline is a legal project and an illegal file: no
            // picture, no sound, and a muxer that is never created because no coded
            // frame arrives. Refused by name here -- before the format is even
            // looked at, so an mp4 and a WAV refuse in the same words -- rather than
            // left to write a file of no frames that nothing opens. Every caller of
            // `start` comes through this, the app's own door
            // (`PlaybackSession::is_empty`) being only the first fence.
            //
            // An audio-only timeline has no picture to give a file, and an mp4 of
            // it would be black picture over the sound -- minutes of encoding for a
            // video of nothing. So an mp4 of it is written as the sound alone, the
            // same AAC track in an mp4 with no picture entry ([`run_aac`]); the
            // Matroska and WebM containers, which have no such form, are refused by
            // name. The app says the same on the format row before a destination
            // is even picked (`mp4_refusal`).
            let written = match settings.format {
                _ if project.timeline_frames() == 0 => {
                    Err("the timeline is empty: there is nothing to export".into())
                }
                Format::PngSequence | Format::Still | Format::Gif if !has_picture(&project) => Err(format!(
                    "the timeline has no picture: {} would be black. Export an mp4, an \
                 M4A, WAV, FLAC, MP3, Ogg or Opus, which are the sound itself",
                    settings.format.name()
                )
                .into()),
                // The pictures are composed one timeline frame at a time, with no sound.
                Format::PngSequence => run_png_sequence(&project, &meta, &out, &worker, &settings),
                Format::Still => run_still(&project, &meta, &part, &settings),
                Format::Gif => run_gif(&project, &meta, &part, &worker, &settings),
                format if format.ext() == "mp4" && !has_picture(&project) => {
                    run_aac(&project, &meta, &part, &worker, &settings, sample_rate)
                }
                format if format.has_video() && !has_picture(&project) => Err(format!(
                    "the timeline has no picture: {} would be black. Export an mp4, an \
                 M4A, WAV, FLAC, MP3, Ogg or Opus, which are the sound itself",
                    format.name()
                )
                .into()),
                format if format.has_video() => {
                    let first = run(&project, &meta, &part, &worker, &settings, sample_rate);
                    match first {
                        // The hardware encoder's *process* died under the driver
                        // ([`crate::hwproc`]) -- the editor is still here, and the
                        // file is still owed. Written again by the software encoder
                        // for this codec, from the first frame: the pictures the
                        // dead seat coded are not ones this one may continue from.
                        //
                        // Not where a person picked the GPU by name: that export
                        // refuses in words, the same answer [`hw_seat`] already
                        // gives a machine with no seat at all, rather than quietly
                        // spending an hour in an encoder nobody chose.
                        Err(e)
                            if crate::hwproc::is_lost(&e)
                                && settings.seat != EncoderSeat::Hardware =>
                        {
                            if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export video: {e}; writing it again in software"); }
                            let _ = std::fs::remove_file(&part);
                            // The bar is *not* put back to zero: the rerun writes
                            // it with `fetch_max` like everything else
                            // ([`Shared::progress`]), so it holds where the dead
                            // seat left it and moves again once the software
                            // encoder passes that mark. A person whose GPU has just
                            // died is the last one who should also watch the bar
                            // run backwards.
                            let mut software = settings.clone();
                            software.seat = EncoderSeat::Software;
                            run(&project, &meta, &part, &worker, &software, sample_rate)
                        }
                        other => other,
                    }
                }
                Format::M4a => run_aac(&project, &meta, &part, &worker, &settings, sample_rate),
                // Opus codes at 48 kHz and nothing else, so the mix is made there.
                Format::Opus => run_audio(&project, &meta, &part, &worker, &settings, Some(OPUS_RATE)),
                _ => run_audio(&project, &meta, &part, &worker, &settings, sample_rate),
            };
            let result = match settings.format {
                // A run of pictures is written into the folder it was asked for, which has no
                // temporary of its own to publish.
                Format::PngSequence => written,
                _ => written.and_then(|()| std::fs::rename(&part, &out).map_err(Into::into)),
            };
            if result.is_err() {
                // The muxer -- and with it the file handle -- died with `run`.
                let _ = std::fs::remove_file(&part);
            }
            settle(&worker, result);
        }));
        if let Err(payload) = caught {
            // The body above panicked before it could settle its own slot --
            // the panic message becomes the outcome, so a caller sees a
            // failure and a notice rather than a bar that never moves again.
            let message = payload
                .downcast_ref::<&str>()
                .map(ToString::to_string)
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            if !panic_worker.finished.load(Ordering::Acquire) {
                // ...and the `.part` the body left behind is deleted here: the
                // module promises every failure path removes it, and the only
                // path that did was the body's own `result.is_err()` arm, which
                // a panic never reaches. Done inside this branch only -- a body
                // that *did* settle has already renamed or deleted the file.
                let _ = std::fs::remove_file(&part);
                settle(
                    &panic_worker,
                    Err(format!("export worker panicked: {message}").into()),
                );
            }
        }
    });
    if let Err(e) = spawned {
        settle(&shared, Err(e.into()));
    }
    ExportHandle { shared, length_secs }
}

/// Whether any video lane holds anything at all -- what tells an audio-only
/// timeline from one that merely opens on a gap. Asked of the *lanes* rather
/// than of the sources, because a project may name a video file no clip plays
/// from any more.
fn has_picture(project: &Project) -> bool {
    // A title is a picture of its own: a timeline of nothing but a title card
    // over black is a picture, not a black one.
    !project.titles().is_empty()
        || project
            .lanes()
            .into_iter()
            .any(|lane| lane.kind == LaneKind::Video && !project.lane(lane).is_empty())
}

/// Whether any audio lane holds anything, [`has_picture`]'s pair: what tells a
/// timeline whose export will carry sound from one whose file has none.
fn has_sound(project: &Project) -> bool {
    project
        .lanes()
        .into_iter()
        .any(|lane| lane.kind == LaneKind::Audio && !project.lane(lane).is_empty())
}

/// Whether any equalizer on the sound is doing something -- the one edit a
/// packet copy cannot carry, so an mp4 whose lane has one is decoded and
/// re-encoded ([`encode_audio`]) instead of copied. Asked of every audio lane:
/// the copy path has exactly one by the time it asks, and [`planned_audio`] is
/// asked before that is settled.
fn equalized(project: &Project) -> bool {
    project.audio_lanes().into_iter().any(|lane| {
        (0..project.lane(lane).len())
            .any(|idx| project.eq_of(lane, idx).is_some_and(|eq| !eq.is_identity()))
    })
}

/// How many timeline frames before a ranged export's start the mix is opened, so the voice
/// chain's envelopes and the noise reduction's overlap are warm when the range begins
/// instead of starting from silence ([`voice`](crate::voice)). Half a second, as far back as
/// the timeline goes; none when no lane has a processor on. [`warm_up_samples`] is how much
/// of the mix's front that is, taken off again.
fn warm_up(project: &Project, offset: u32, fps: f64) -> u32 {
    if offset == 0 || !project.audio_voices().iter().any(crate::voice::VoiceFx::is_active) {
        return 0;
    }
    offset.min((fps * 0.5).ceil() as u32)
}

/// How many interleaved samples the `pre` frames of [`warm_up`] before `range_start` are:
/// the samples the timeline's mix puts between the two frames, counted with the mixer's own
/// rounding ([`timeline_sample`](crate::audio::timeline_sample)), so the range starts on the
/// same sample a full export has there. Rounding the pre-roll's own length instead can miss
/// by one at 29.97 or 23.976 fps.
fn warm_up_samples(range_start: u32, pre: u32, fps: f64, rate: u32, channels: usize) -> usize {
    let at = |frame: u32| crate::audio::timeline_sample(f64::from(frame) / fps, rate);
    (at(range_start) - at(range_start - pre)) as usize * channels
}

/// Whether the *mix* is doing something -- a lane turned off unity, a lane
/// ducked under another, or a limiter in circuit. [`equalized`]'s pair, and a
/// copy cannot carry any of them: all live where the lanes are summed
/// ([`AudioSession::open_mixed_streams_master`]), and a copied packet was never
/// summed at all. An mp4 whose timeline is mixed is decoded and re-encoded.
fn mastered(project: &Project) -> bool {
    project.limiter().is_active()
        || project.audio_gains().iter().any(|&g| g != 1.0)
        || project.audio_ducks().iter().any(|lane| !lane.is_empty())
        || project.audio_voices().iter().any(crate::voice::VoiceFx::is_active)
}

/// Whether any audio clip has a fade -- [`equalized`]'s pair again: a fade is
/// gain math on decoded samples ([`crate::project::Fade::apply`]), and a
/// packet copy carries neither an equalizer nor a fade. An mp4 whose timeline
/// has one is decoded and re-encoded so the fade actually reaches the file.
fn faded(project: &Project) -> bool {
    project.audio_lanes().into_iter().any(|lane| {
        project
            .lane(lane)
            .iter()
            .any(|c| c.fade_in > 0 || c.fade_out > 0)
    })
}

/// The bitrate an export codes at: the caller's number clamped into what an
/// asked-for rate may be ([`MIN_BITRATE`]..[`MAX_EXPLICIT_BITRATE`]), the
/// derived one where there was none. Clamped rather than refused, for the
/// reason [`Enc::open`] states.
fn bitrate_of(meta: &VideoMeta, settings: &ExportSettings) -> u64 {
    settings.bitrate.map_or_else(
        || bitrate_for(meta),
        |b| b.clamp(MIN_BITRATE, MAX_EXPLICIT_BITRATE),
    )
}

/// The rate the sound codes at: the caller's pick clamped into the offered
/// range, [`DEFAULT_AUDIO_KBPS`] where there was none. Clamped and not refused,
/// for [`bitrate_of`]'s reason -- a number out of range is a caller's slip and
/// not a file worth failing.
fn audio_kbps_of(settings: &ExportSettings) -> u32 {
    // 384 is above the offered rows, which a preset asks for (YouTube's AAC); MP3 has no such rate.
    let ceiling = if settings.format == Format::Mp3 { AUDIO_KBPS[3] } else { 384 };
    settings.audio_kbps.map_or(DEFAULT_AUDIO_KBPS, |k| k.clamp(AUDIO_KBPS[0], ceiling))
}

/// The hardware seat for these settings, opened exactly as the export opens it
/// -- the same pins, the same dimensions a driver may refuse -- or `None` where
/// the software encoder takes the file. The single place that choice is made,
/// which is what lets [`planned_video`] *measure* it instead of promising it.
///
/// `Err` is the one answer the software encoder does **not** take over from:
/// [`EncoderSeat::Hardware`] asked for the GPU by name and this machine has no
/// seat for the codec, so the export refuses in words rather than falling back
/// to an encoder nobody picked. Every caller is before a file exists
/// ([`Enc::open`] runs ahead of the muxer), which is what makes the refusal
/// free.
fn hw_seat(meta: &VideoMeta, settings: &ExportSettings) -> crate::Result<Option<HwEncoder>> {
    if settings.seat == EncoderSeat::Software || forced("VE_SW_ENC") {
        return Ok(None);
    }
    // The plugin's HEVC seat codes intra pictures only ([`open_hw_seat`]), so a
    // GOP export cannot be had from the GPU. Said here rather than written as an
    // intra file under a card that promised I+P.
    if settings.seat == EncoderSeat::Hardware && settings.format.is_hevc() && !settings.intra_only {
        return Err("hardware HEVC codes intra pictures only and this export is I+P: pick Auto or Software on the export card, or an intra-only export for the GPU".into());
    }
    let opened = open_hw_seat(meta, settings);
    if opened.is_none() && settings.seat == EncoderSeat::Hardware {
        return Err(format!(
            "hardware encoding was asked for and this machine has no VA-API seat for {}: \
             pick Auto or Software on the export card",
            settings.format.name()
        )
        .into());
    }
    Ok(opened)
}

/// The plugin half of [`hw_seat`]: what the driver gives for this codec and
/// this picture, with no policy in it but the AV1 one.
fn open_hw_seat(meta: &VideoMeta, settings: &ExportSettings) -> Option<HwEncoder> {
    let (fps_num, fps_den) = crate::mux::frame_timing(meta.frame_rate).ok()?;
    let bitrate = bitrate_of(meta, settings);
    match settings.format {
        // Both HEVC formats sit on the plugin's HEVC seat, and it is the
        // default again: what it codes is intra-only, the very file the
        // software seat writes, so preferring the GPU changes how long an
        // export takes and not what comes out of it. What made that untrue for
        // one day was the vendored encoder's parameter sets promising coding
        // tools the driver was not using; the pictures are now measured against
        // their sources rather than counted
        // (`hevc_export::a_1080p_hardware_export_leaves_a_decoder_with_nothing_to_say`).
        // `None` here is the plugin's own "no" -- no entrypoint, or a picture
        // under the driver's 384x384 floor -- and the software seat takes it.
        // The colour goes in at the open, from the same [`ColorDescription::output`]
        // rule the software seat's VUI and both containers' tags come from, so
        // the file cannot say one thing in its bitstream and another in its
        // header.
        // The GOP default is software: Auto takes `ec-h265` for an I+P file
        // rather than the GPU's intra one, which is what the card says it is.
        format if format.is_hevc() && !settings.intra_only => None,
        format if format.is_hevc() => HwEncoder::open_hevc(
            meta.width,
            meta.height,
            fps_num,
            fps_den,
            bitrate,
            ColorDescription::output_sized(meta.width, meta.height),
        ),
        // Asked for by name only, for the reason [`Enc::open_av1`] states in
        // full: `Auto` takes the software encoder here and nowhere else. Both
        // AV1 formats sit on it: the container is not the encoder's business.
        format if format.is_av1() && settings.seat != EncoderSeat::Hardware => None,
        format if format.is_av1() => {
            HwEncoder::open_av1(meta.width, meta.height, fps_num, fps_den, bitrate)
        }
        // An intra-only H.264 file is a *different seat*, not the same seat with
        // a flag on the pictures: asking this one for a key frame every time
        // gets I slices that are not IDRs and carry no parameter sets, which is
        // a file with one starting point in it however many I pictures it holds
        // (measured: a 60-frame hardware proxy, 1 sync point of 60). `None`
        // where the plugin is older than that seat, and then the software
        // encoder takes the file -- its `gop_size = 1` writes what was asked
        // for, slowly, which is the only honest fallback here.
        //
        // Only H.264 reaches this arm: the two arms above take every HEVC and
        // AV1 format, and the HEVC seat is intra-only already.
        _ if settings.intra_only => {
            HwEncoder::open_intra(meta.width, meta.height, fps_num, fps_den, bitrate)
        }
        _ => HwEncoder::open(meta.width, meta.height, fps_num, fps_den, bitrate),
    }
}

/// What a hardware encode is called wherever one is named -- the card's line
/// before the export and the progress line during it.
const HW_LABEL: &str = "HW encode (VA-API)";

/// ...and what a card says instead where [`EncoderSeat::Hardware`] was picked
/// and there is no seat: the export's own refusal in the width of a card row,
/// shown *before* the button is pressed rather than a minute into a file.
pub const NO_HW_SEAT: &str = "no HW seat here — pick Auto or Software";

/// How a front-end names a video seat: which of the two encoders has the file,
/// and which library that is. Not the codec -- a caller shows this beside the
/// format it picked, and `rav1e` names AV1 as `ec-h264` names H.264. One
/// place, so what a card says before an export and what its progress line says
/// during one cannot drift apart.
fn video_label(format: Format, hw: bool, intra: bool) -> &'static str {
    match (format, hw) {
        (_, true) => HW_LABEL,
        (Format::Hevc | Format::HevcMp4, false) if intra => "SW encode (ec-h265 intra)",
        (Format::Hevc | Format::HevcMp4, false) => "SW encode (ec-h265 GOP)",
        (Format::Av1 | Format::Av1Mp4 | Format::Webm, false) => "SW encode (rav1e)",
        (Format::ProRes, false) => "SW encode (ec-prores)",
        (_, false) => "SW encode (ec-h264)",
    }
}

/// Whether the *edit* has touched the samples a copy would carry out: the one
/// predicate both copy-or-encode decisions ask, with `lanes` the sound's own
/// list as the caller has it -- every audio lane for the question a card asks
/// before the button ([`forces_encode`]), and the single lane the segments have
/// already narrowed a copy to ([`copy_audio`]).
///
/// One function and not two lists of the same conditions, which is what they
/// were: `faded` reached [`forces_encode`] and not [`copy_audio`]'s gate, so an
/// mp4 whose lane carried a fade wrote the source's own packets with the fade
/// nowhere, while the card had promised a re-encode -- and the WAV, FLAC and
/// MP3 exports of the same project applied it. Two gates that cannot be given
/// two answers is the fix; the drift was the defect.
fn sample_altered(project: &Project, lanes: &[Lane]) -> bool {
    // Two lanes are a mix and a mix is not a copy; a speeded lane is resampled;
    // an equalizer, a fade and the mix itself are gain math on decoded samples
    // ([`equalized`], [`faded`], [`mastered`]). None of them is inside a packet.
    lanes.len() > 1
        || lanes
            .iter()
            .any(|&lane| project.lane(lane).iter().any(|c| !c.speed.is_normal() || !c.speed_keys.is_empty() || c.reversed))
        || equalized(project)
        || mastered(project)
        || faded(project)
}

/// The cheap half of "does this timeline's sound have to be re-encoded": what
/// the *edit* says, with no file opened ([`sample_altered`], asked of every
/// audio lane). The other half -- whether the sources can be copied out of at
/// all -- costs a file open each and belongs to [`copy_audio`], which may
/// re-encode where this says copy.
fn forces_encode(project: &Project, ranged: bool) -> bool {
    // A ranged export needs an exact offset and an exact length, which a copy
    // of the source's own packets cannot give it ([`copy_audio`]) -- so a
    // range marked at all always encodes, whatever the lanes look like.
    ranged || sample_altered(project, &project.audio_lanes())
}

/// What the sound is written by. Every audio encoder here is software -- there
/// is no hardware AAC, PCM, FLAC or MP3 seat -- so this names *which* one, and
/// whether a container escapes encoding altogether by copying the packets it was
/// given. Both AV1 formats are in it like the mp4: an AV1 export carries the
/// timeline's sound too, so leaving it unnamed would be the old lie.
///
/// A prediction, and it says so: `copied` is what [`copy_audio`] *did*, which a
/// card asking before the export has no way to know (a source that cannot be
/// copied out of is only found by opening it). `None` predicts from the edit
/// alone.
fn audio_label(
    project: &Project,
    format: Format,
    sound: bool,
    copied: Option<bool>,
    ranged: bool,
) -> &'static str {
    if !sound {
        return "no sound to write";
    }
    match format {
        Format::Wav => "PCM · SW (ec-riff)",
        Format::Flac => "FLAC · SW (ec-flac)",
        Format::Mp3 => "MP3 · SW (ec-mp3)",
        Format::Ogg => "Vorbis · SW (ec-vorbis)",
        Format::Opus => "Opus · SW (ec-opus)",
        Format::ProRes => "PCM · SW (16-bit, mov)",
        // Never a copy: WebM carries Opus, and the sound is mixed and encoded
        // for it at 48 kHz ([`start`], [`encode_audio`]).
        Format::Webm => "Opus · SW encode (ec-opus)",
        _ => match copied.unwrap_or(!forces_encode(project, ranged)) {
            true => "AAC copy",
            // A Matroska file gets Opus where the mix allows it -- 48 kHz and
            // stereo, which is every Opus source and most of everything else --
            // and AAC where it does not ([`encode_audio`]). Which of the two it
            // will be cannot be known here: both answers need a source opened,
            // and this function may be asked per repaint. So the card names the
            // codec it will nearly always be, as a prediction like the copy above
            // it, and the line the export publishes while it runs is the measured
            // one ([`run`]) -- it says AAC when the fallback took it, so the
            // fallback is visible rather than silent.
            //
            // It is one line and 76 characters wide (`summary_head` in the app
            // has the test), which is why the condition is in this comment
            // instead of in the string.
            false if format.is_mkv() => "Opus · SW encode (ec-opus)",
            false => "AAC · SW encode (ec-aac)",
        },
    }
}

/// What the export *did* to the sound, for the line it publishes while it runs:
/// [`audio_label`]'s words with the one thing no prediction can know filled in
/// -- whether the Opus seat took the track or handed it back
/// ([`encode_opus`]'s gate).
///
/// `sound` is `Some(copied)` for a track and `None` for a file with no sound.
/// Naming Opus for *every* Matroska encode is what made that fallback
/// invisible: a track the gate refused was written as AAC and the line still
/// said Opus, on the one path where the difference is audible.
fn measured_audio_label(
    project: &Project,
    format: Format,
    sound: Option<bool>,
    opus: bool,
) -> &'static str {
    match (sound, opus) {
        (_, true) => "Opus · SW encode (ec-opus)",
        // Encoded, into a container that would have carried Opus, and it is not
        // Opus: the fidelity gate sent it to AAC ([`OPUS_MIN_FIDELITY`]), or
        // the mix was not 48 kHz at a width the seat is measured in.
        (Some(false), false) if format.is_mkv() => "AAC · SW encode (ec-aac)",
        // `sound` is already what happened, not a prediction, so `copied` is
        // filled in and `ranged` never reaches the branch that reads it.
        _ => audio_label(project, format, sound.is_some(), sound, false),
    }
}

/// What [`start`] would write this timeline's sound with, before one is
/// started. Pure: no probe and no file opened, so a card may ask it per
/// repaint. `ranged` is whether the export a card is describing is bounded to
/// a marked range -- a copy needs the source's own packets to *be* the file,
/// which a range's exact offset and length rule out ([`copy_audio`]).
pub fn planned_audio(project: &Project, format: Format, ranged: bool) -> &'static str {
    audio_label(project, format, has_sound(project), None, ranged)
}

/// Why the sound's rate is nothing this timeline can be asked about in this
/// format, or `None` where it is a real choice: there is no sound to write, the
/// format codes the samples themselves, or the packets are the source's own and
/// carry the rate *it* was coded at. Pure, like [`planned_audio`], and a
/// prediction in the same way -- a copy that turns out impossible (a source no
/// mp4 sample table holds) falls through to [`encode_audio`], which codes at
/// [`ExportSettings::audio_kbps`] like every other encode. So the value travels
/// even where this refuses; what it cannot do is silently write a rate nobody
/// picked.
pub fn audio_rate_refusal(project: &Project, format: Format, ranged: bool) -> Option<&'static str> {
    match format {
        _ if !has_sound(project) => Some("no sound to write"),
        Format::Wav | Format::Flac | Format::ProRes => Some("lossless — the samples themselves, at no rate"),
        // Vorbis is written at one measured operating point ([`VORBIS_QUALITY`]
        // + [`VORBIS_TARGET_BPS`]), not at a rate a user may pick: its actual
        // landing moves with the content around that target (124-134 kbps
        // across this suite's bench inputs), so a live rate row would print a
        // figure the file does not hold -- the one thing this card is built
        // not to do.
        Format::Ogg => Some("quality-coded — Vorbis holds no rate to pick"),
        // The bitrate is the one choice WebM leaves: Opus at the rate picked.
        Format::Mp3 | Format::Webm | Format::Opus => None,
        _ if forces_encode(project, ranged) => None,
        _ => Some("the source's own packets are copied — at the rate they hold"),
    }
}

/// The soft subtitle tracks an export carries: the lanes the timeline places, each
/// as its placement (the lanes a pick reaches), then every picked track no lane
/// places, whole. Empty where it carries none: no pick and no placement, a pick
/// nothing answers, a track that could not be read, one with no cue left on the
/// exported timeline -- and every format that is the sound alone, which has
/// nowhere to put them. Filtered *per track*, so one unusable pick costs its own
/// row and not the others.
///
/// Both containers, since the mp4 muxer gained its `tx3g` track: what the two
/// do with the very same [`SubParams`] is theirs to say
/// ([`crate::mux::Mp4Muxer::write_subtitles`], [`MkvMuxer`]), and nothing here
/// asks which one is being written.
fn export_subtitles(
    project: &Project,
    meta: &VideoMeta,
    settings: &ExportSettings,
) -> Vec<SubParams> {
    // WebM has no subtitle track this writer can state: [`planned_subtitles`]
    // says so on the card, and nothing is written.
    if !settings.format.has_video() || matches!(settings.format, Format::Webm | Format::ProRes) || !has_picture(project) {
        return Vec::new();
    }
    // Burned captions are the only ones this file carries: a soft track of the
    // same words would show them twice.
    if settings.no_soft_subtitles {
        return Vec::new();
    }
    // Words placed on a lane are written through the lanes, at their placement
    // (`lane_subtitle`; none when nothing is placed). A picked track that no lane
    // places is written whole beside them, as it always was: the palette's pick
    // is the only say it has in the file, so it must not be dropped for a lane
    // elsewhere. A placed track is never written twice. The tracks go in the
    // order the caller gave them ([`soft_order`]), placed or not.
    let lanes = placed_lanes(project);
    let mut subs = Vec::new();
    for entry in soft_order(project, &lanes, &settings.subtitles) {
        match entry {
            SoftEntry::Lane(lane) => subs.extend(lane_subtitle(
                project,
                lane,
                meta.frame_rate,
                settings.range,
                &settings.subtitles,
            )),
            SoftEntry::Track(pick) => subs.extend(whole_track(project, meta, settings, pick)),
        }
    }
    subs
}

/// Whether a track has a cue the file would carry: any cue at all on the whole timeline, or one
/// left inside `range` (on the clock the range is cut to, as [`written_cues`] cuts it).
fn track_has_cues(project: &Project, track: &SubtitleTrack, range: Option<(u32, u32)>, fps: f64) -> bool {
    match range {
        None => !track.cues.is_empty(),
        Some(_) => !written_cues(timeline_cues(project, track, fps), range, fps, project.media_frames()).is_empty(),
    }
}

/// The reason a track with nothing to carry is dropped, in the card's words.
fn no_cues_word(range: Option<(u32, u32)>) -> &'static str {
    if range.is_some() { "no cues in range" } else { "no cues" }
}

/// The labels of the written soft tracks whose styling the file drops: a standalone ASS or SSA
/// script whose lines carry override blocks (`{\i1}`, `{\an8}`), which a cue's plain text does not
/// keep. An embedded ASS track is not read here, so it is not named. Empty when none is.
pub fn stripped_styling(project: &Project, picks: &[usize]) -> Vec<String> {
    let lanes = placed_lanes(project);
    let placed = placed_tracks(project);
    let mut used: std::collections::BTreeSet<usize> = picks.iter().copied().filter(|p| !placed.contains(p)).collect();
    for &lane in &lanes {
        let on_lane = project.sub_lane(lane);
        if picked(on_lane, picks) {
            used.extend(on_lane.iter().map(|s| s.track));
        }
    }
    used.into_iter()
        .filter_map(|i| project.subtitles().get(i))
        .filter(|t| t.refused.is_none() && !t.is_bitmap() && t.track.is_none() && has_override_blocks(&t.path))
        .map(|t| t.label.clone())
        .collect()
}

/// Whether `path` is an ASS or SSA script with an override block in it.
fn has_override_blocks(path: &Path) -> bool {
    let ass = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ass") || e.eq_ignore_ascii_case("ssa"));
    ass && std::fs::read_to_string(path).is_ok_and(|text| text.contains("{\\"))
}

/// One soft track of the file: a subtitle lane (written through its placements) or a picked
/// track no lane places (written whole).
#[derive(Clone, Copy, Debug, PartialEq)]
enum SoftEntry {
    Lane(Lane),
    Track(usize),
}

/// The soft tracks the file writes, in the order the file declares them. With picks, that is
/// the order the caller gave them: a lane goes where the first pick that reaches it sits, and a
/// pick no lane places goes whole in its own place. Without picks, every placed lane in lane
/// order and no whole track.
fn soft_order(project: &Project, lanes: &[Lane], picks: &[usize]) -> Vec<SoftEntry> {
    if picks.is_empty() {
        return lanes.iter().map(|&l| SoftEntry::Lane(l)).collect();
    }
    let placed = placed_tracks(project);
    let mut out: Vec<SoftEntry> = Vec::new();
    for &pick in picks {
        let entry = if placed.contains(&pick) {
            match lanes.iter().find(|&&l| project.sub_lane(l).iter().any(|s| s.track == pick)) {
                Some(&lane) => SoftEntry::Lane(lane),
                None => continue,
            }
        } else {
            SoftEntry::Track(pick)
        };
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    out
}

/// One picked track written whole, on the timeline's clock and clipped to the range
/// and the picture. `None` for a track that writes nothing: no row for the pick, a
/// track that could not be read, a picture track (the muxer writes words), or no cue
/// left in the file.
fn whole_track(
    project: &Project,
    meta: &VideoMeta,
    settings: &ExportSettings,
    pick: usize,
) -> Option<SubParams> {
    let track = project.subtitles().get(pick)?;
    // A picture is not a line: the muxer writes an `S_TEXT/UTF8` track and a PGS cue
    // has no words to put in one. `planned_subtitles` says so before the button is
    // pressed rather than the file coming out short.
    if track.refused.is_some() || track.is_bitmap() {
        return None;
    }
    let cues = written_cues(
        timeline_cues(project, track, meta.frame_rate),
        settings.range,
        meta.frame_rate,
        project.media_frames(),
    );
    // The container's own two fields, carried as two
    // ([`SubtitleTrack::language`]): a `TrackEntry` with no `Language` means `eng`
    // by spec, so a French track that arrives here as one flattened string leaves
    // as an English one.
    (!cues.is_empty()).then(|| SubParams {
        language: track.language.clone(),
        name: subtitle_title(track),
        cues,
    })
}

/// The title a soft subtitle track is written under. An embedded track keeps the
/// title its container gave it; a standalone file has none of its own, so it is
/// titled with its language's name (`French`) or left untitled -- never with its
/// file name.
fn subtitle_title(track: &SubtitleTrack) -> String {
    match track.track {
        Some(_) => track.name.clone(),
        None => crate::subtitle::language_name(&track.language).unwrap_or_default().to_string(),
    }
}

/// The indices of the tracks some placement on a subtitle lane names. Those tracks
/// are written through their lanes, so a pick of one of them is not written whole.
fn placed_tracks(project: &Project) -> std::collections::BTreeSet<usize> {
    project
        .subtitle_lanes()
        .into_iter()
        .flat_map(|lane| project.sub_lane(lane).iter().map(|s| s.track))
        .collect()
}

/// The labels of the soft subtitle tracks this export writes whose cues overlap. An mp4
/// writes an overlap as one stretch with both lines in it (`mux`'s `timed_text`), so the
/// cue times the timeline holds are not the file's; an mkv keeps every cue. Empty when
/// nothing is written or no two cues of a written track overlap.
pub fn overlapping_subtitles(project: &Project, meta: &VideoMeta, settings: &ExportSettings) -> Vec<String> {
    export_subtitles(project, meta, settings)
        .into_iter()
        .filter(|t| {
            let mut spans: Vec<(i64, i64)> = t.cues.iter().map(|c| (c.start_us, c.end_us)).collect();
            spans.sort_unstable();
            let mut reach = i64::MIN;
            spans.iter().any(|&(start, end)| {
                let overlap = start < reach;
                reach = reach.max(end);
                overlap
            })
        })
        .map(|t| match (t.name.is_empty(), t.language.is_empty()) {
            (false, _) => t.name,
            (true, false) => t.language,
            (true, true) => "an unnamed track".to_string(),
        })
        .collect()
}

/// Whether a lane with these placements is in the file: an empty pick list is
/// every lane, otherwise the lane must hold at least one placement of a picked
/// track ([`Project::subtitles`] index, as [`ExportSettings::subtitles`] gives).
fn picked(placed: &[crate::project::SubClip], picks: &[usize]) -> bool {
    picks.is_empty() || placed.iter().any(|s| picks.contains(&s.track))
}

/// The project's subtitle lanes in display order, and empty where **nothing is
/// placed on any of them** -- an empty list is what the export and the card both
/// read as "this project does not use lanes yet", so a project holding a bare
/// `S1` nobody dropped a caption on still exports by the pick list.
fn placed_lanes(project: &Project) -> Vec<Lane> {
    let lanes = project.subtitle_lanes();
    match lanes.iter().any(|&l| !project.sub_lane(l).is_empty()) {
        true => lanes,
        false => Vec::new(),
    }
}

/// One track per subtitle *lane*, in lane order -- so the file declares them
/// top-down as the timeline shows them and a player's subtitle menu lists them
/// in that order.
///
/// The cues are [`Project::sub_lane_cues`]: each placement's window of its
/// palette track, on the timeline's own clock. Clipped again here to what the
/// export *writes*, which ends with the last picture or sound
/// ([`Project::media_frames`]) -- a caption hanging past both holds the
/// timeline open ([`Project::timeline_frames`]) but there is no picture under
/// it to write, so a cue out there is dropped and one straddling the end keeps
/// the half that is inside. [`planned_subtitles`] says so before the button is
/// pressed.
///
/// A lane is dropped whole -- no track at all in the file -- where any
/// placement on it is a picture ([`SubtitleTrack::is_bitmap`]), a track that
/// could not be read, or a palette row that has since gone: the muxers write
/// words, and half a lane's cues under the other half's name would be worse
/// than none. Its language and title are the lane's *first* placement's track,
/// which is the one thing the whole lane is named by.
///
/// `picks` narrows the lanes the same way [`ExportSettings::subtitles`] does: a
/// lane is kept when it holds a placement of a picked track ([`picked`]), and an
/// empty list keeps every lane.
#[cfg(test)]
fn lane_subtitles(
    project: &Project,
    fps: f64,
    range: Option<(u32, u32)>,
    picks: &[usize],
) -> Vec<SubParams> {
    placed_lanes(project)
        .into_iter()
        .filter_map(|lane| lane_subtitle(project, lane, fps, range, picks))
        .collect()
}

/// One lane written as its track ([`lane_subtitles`] for a single lane); `None` where the
/// lane is not in the file (see there for the reasons).
fn lane_subtitle(
    project: &Project,
    lane: Lane,
    fps: f64,
    range: Option<(u32, u32)>,
    picks: &[usize],
) -> Option<SubParams> {
    let end = (f64::from(project.media_frames()) / fps * 1e6).round() as i64;
    let placed = project.sub_lane(lane);
    if !picked(placed, picks) {
        return None;
    }
    let first = placed.first()?;
    let writable = |track: usize| {
        project
            .subtitles()
            .get(track)
            .filter(|t| t.refused.is_none() && !t.is_bitmap())
    };
    let named = writable(first.track)?;
    if placed.iter().any(|s| writable(s.track).is_none()) {
        return None;
    }
    let cues: Vec<Cue> = written_cues(
        project
            .sub_lane_cues(lane, fps)
            .into_iter()
            .filter_map(|cue| {
                let (start_us, end_us) = (cue.start_us.max(0), cue.end_us.min(end));
                (end_us > start_us).then_some(Cue {
                    start_us,
                    end_us,
                    text: cue.text,
                    image: cue.image,
                    words: Vec::new(),
                    karaoke: Vec::new(),
                })
            })
            .collect(),
        range,
        fps,
        project.media_frames(),
    );
    (!cues.is_empty()).then(|| SubParams {
        language: named.language.clone(),
        name: subtitle_title(named),
        cues,
    })
}

/// `cues` on the clock the export *writes*, for a range-relative file.
///
/// A cue is timed on the timeline; a ranged export's file starts its own clock
/// at zero where the range starts, so every cue moves back by the range's start.
/// What falls before the range is dropped, what falls after it is dropped, and a
/// cue straddling either edge keeps the half inside. The window is the range
/// clamped to the media, the same clamp [`export_spans`] makes, so the cues stop
/// where the pictures do. Both ends are whole microseconds of the same
/// frame-to-time mapping the cues were timed with, so the only rounding left is
/// the container's own tick.
///
/// Unranged, the cues come back as they were.
fn written_cues(
    cues: Vec<Cue>,
    range: Option<(u32, u32)>,
    fps: f64,
    media_frames: u32,
) -> Vec<Cue> {
    let Some((r_start, r_end)) = range else {
        return cues;
    };
    let us = |frame: u32| (f64::from(frame) / fps * 1e6).round() as i64;
    let from = us(r_start);
    let to = us(r_end.min(media_frames).max(r_start));
    cues.into_iter()
        .filter_map(|cue| {
            let start_us = cue.start_us.max(from);
            let end_us = cue.end_us.min(to);
            (end_us > start_us).then_some(Cue {
                start_us: start_us - from,
                end_us: end_us - from,
                text: cue.text,
                image: cue.image,
                words: Vec::new(),
                karaoke: Vec::new(),
            })
        })
        .collect()
}

/// The composite spans an export *writes*: [`Project::composite_spans_from`]
/// cut to where the media runs out ([`Project::media_frames`]), and, with
/// `range` set, to that window too.
///
/// The walk itself is the timeline's ([`Project::timeline_frames`]), because a
/// caption placed past the last picture holds the timeline open under it and
/// playback parks on that tail -- but there is nothing to *write* out there, so
/// an unbounded walk encoded and muxed black frames past the end of the media
/// and padded the sound to match. This is the one place that end is decided:
/// the frame count [`run`] publishes, the cues [`lane_subtitles`] clips and the
/// blocks [`CopyPlan`] copies are then the same number by construction, which
/// is what makes [`planned_lanes`]' "cut off there" true of the file.
///
/// `span.start` stays the *project's* own timeline coordinate throughout --
/// what [`Project::composite_color_at`] and its fit sibling are keyed on -- so
/// a caller writing a range-relative output shifts it at the one place that
/// actually emits a position (`CopyPlan`'s own region, since the encode loop
/// counts pictures rather than reading `span.start` for its output clock).
/// Trimming the left edge mid-span moves `from`'s source frame by
/// [`Speed::source_at`], the same mapping a legal cut is placed by.
fn export_spans(project: &Project, range: Option<(u32, u32)>) -> Vec<crate::project::Span> {
    let end = project.media_frames();
    let mut spans = stack_spans_from(project, 0);
    spans.retain(|span| span.start < end);
    for span in &mut spans {
        span.len = span.len.min(end - span.start);
    }
    // A title past the last clip is picture: the frames up to its end are black
    // under it, a gap like any other. Only titles extend the picture here; a
    // sound that runs on still does not, as it never did.
    let title_end = project
        .titles()
        .iter()
        .map(Title::end)
        .max()
        .unwrap_or(0)
        .min(end);
    let covered = spans.last().map_or(0, crate::project::Span::end);
    if covered < title_end {
        spans.push(crate::project::Span {
            ramp: None,
            start: covered,
            len: title_end - covered,
            from: None,
            speed: Speed::NORMAL,
            reverse: false,
            pitch: 0,
        });
    }
    let Some((r_start, r_end)) = range else {
        return spans;
    };
    let r_end = r_end.min(end).max(r_start);
    spans.retain(|span| span.start < r_end && span.start + span.len > r_start);
    for span in &mut spans {
        if span.start < r_start {
            let cut = r_start - span.start;
            if let Some((source, in_frame)) = span.from {
                span.from = Some((source, span.source_at(in_frame, cut)));
            }
            // A ramp's offsets are counted from the span's start, which has moved.
            span.ramp = span.ramp.map(|r| r.advanced(cut));
            span.len -= cut;
            span.start = r_start;
        }
        span.len = span.len.min(r_end - span.start);
    }
    spans
}

/// The visible video lanes, bottom first: the last one is the topmost, the order
/// [`Project::composite_clip_at`] counts in.
fn visible_video_lanes(project: &Project) -> Vec<Lane> {
    project
        .lanes()
        .into_iter()
        .filter(|lane| lane.kind == LaneKind::Video && !project.lane_hidden(*lane))
        .collect()
}

/// Every visible video clip covering `frame`, bottom first: the stack the
/// picture is composed from. One entry is the single-clip picture every path
/// has always drawn; two or more are a layered picture ([`layered_at`]), where
/// each clip is drawn over the ones beneath it.
pub(crate) fn stack_at(project: &Project, frame: u32) -> Vec<(Lane, usize)> {
    visible_video_lanes(project)
        .into_iter()
        .filter_map(|lane| Some((lane, project.lane_clip_at(lane, frame)?)))
        .collect()
}

/// Whether `frame` is a layered picture: more than one visible clip covers it.
pub(crate) fn layered_at(project: &Project, frame: u32) -> bool {
    let stack = stack_at(project, frame);
    stack.len() >= 2 || animated_at(project, frame) || generated_at(project, &stack)
}

/// Whether a clip covering `frame` is an adjustment layer or an animated generator
/// ([`crate::generate`]): neither is a picture the single-clip path draws.
fn generated_at(project: &Project, stack: &[(Lane, usize)]) -> bool {
    stack.iter().any(|&(lane, idx)| {
        let src = project.sources().get(project.lane(lane)[idx].source);
        src.and_then(|s| crate::generate::Gen::parse(&s.path)).is_some_and(|g| g.animated() || g.is_adjust() || g.has_alpha())
    })
}

/// The one clip covering `frame` when its only animation is colour keys or an
/// effects stack and nothing dissolves out of it: a 10-bit export keeps such a
/// clip on the single-clip path, grading each picture by [`clip_color_at`] and
/// drawing its effects at 10 bits ([`crate::effects`]), rather than composing it
/// layered at 8 bits.
fn color_keyed_alone_at(project: &Project, frame: u32) -> Option<&crate::project::Clip> {
    let [(lane, idx)] = stack_at(project, frame)[..] else {
        return None;
    };
    let clip = &project.lane(lane)[idx];
    ((!clip.color_keys.is_empty() || crate::effects::active(&clip.effects))
        && clip.transform_keys.is_empty()
        && clip.opacity_keys.is_empty()
        && cross_window(project, lane, idx).is_none())
    .then_some(clip)
}

/// Whether the picture at `frame` is one a single clip cannot show: the clip on
/// top has placement keys, or its dissolve partner does. A keyed picture is
/// composed as a layered one, which places each frame's own placement
/// ([`LayerFeed::advance`]), so a single clip's span-long transform never has
/// to stand in for a moving one.
fn animated_at(project: &Project, frame: u32) -> bool {
    visible_video_lanes(project).into_iter().any(|lane| {
        let Some(idx) = project.lane_clip_at(lane, frame) else {
            return false;
        };
        if crate::project::clip_animated(&project.lane(lane)[idx]) {
            return true;
        }
        matches!(
            cross_window(project, lane, idx),
            Some((window, start)) if frame >= start && frame - start < window
        ) && project.lane(lane).get(idx + 1).is_some_and(crate::project::clip_animated)
    })
}

/// [`Project::composite_span_at`] over the stack. The topmost covering clip
/// supplies `from` and `speed`, and the span runs until any visible video lane
/// changes what covers the frame -- a clip there starts or ends. With one
/// covering clip that is exactly the composite's own span; across a stack it
/// also cuts where a lower lane changes beneath the picture on top, since the
/// picture under it is then a different one.
pub(crate) fn stack_span_at(project: &Project, frame: u32) -> Option<crate::project::Span> {
    let total = project.timeline_frames();
    if frame >= total {
        return None;
    }
    let mut until = total;
    for lane in visible_video_lanes(project) {
        let clips = project.lane(lane);
        match project.lane_clip_at(lane, frame) {
            Some(idx) => until = until.min(clips[idx].end()),
            None => {
                if let Some(next) = clips.iter().find(|c| c.start > frame) {
                    until = until.min(next.start);
                }
            }
        }
    }
    let (from, speed, ramp, reverse) = match stack_at(project, frame).pop() {
        Some((lane, idx)) => {
            let clip = &project.lane(lane)[idx];
            let (speed, ramp) = crate::project::span_speed_of(clip, frame);
            (
                Some((clip.source, clip_source_frame(clip, frame))),
                speed,
                ramp,
                clip.reversed,
            )
        }
        None => (None, Speed::NORMAL, None, false),
    };
    Some(crate::project::Span {
        start: frame,
        len: until - frame,
        from,
        speed,
        ramp,
        reverse,
        pitch: 0,
    })
}

/// [`Project::composite_spans_from`] over the stack: every span from `frame` on.
pub(crate) fn stack_spans_from(project: &Project, frame: u32) -> Vec<crate::project::Span> {
    let mut out = Vec::new();
    let mut t = frame;
    while let Some(span) = stack_span_at(project, t) {
        t = span.end();
        out.push(span);
    }
    out
}

/// Which source frame a clip plays at timeline `frame` -- the project's own
/// rule, clamped inside the clip's range.
fn clip_source_frame(clip: &crate::project::Clip, frame: u32) -> u32 {
    (crate::project::source_pos(clip, frame - clip.start).floor() as u32).min(clip.out_frame - 1)
}

/// One clip of a layered picture, open for the span: its decoder, read forward
/// as the picture advances, and the canvas its picture lands on -- the same
/// placement a single clip gets ([`place_picture`]).
struct LayerFeed {
    clip: crate::project::Clip,
    info: Placed,
    meta: VideoMeta,
    grade: Option<crate::color::ColorParams>,
    /// The grade the current picture was graded with, so a colour-keyed clip is
    /// graded again the frame its grade moves, even on the same source frame.
    graded_with: Option<crate::color::ColorParams>,
    /// The clip's base placement ([`Clip::transform`] resolved); its keys, when
    /// it has any, are read from `clip` at each frame ([`clip_transform_at`]).
    transform: TransformParams,
    /// The placement the current picture was placed with, so a keyed clip is
    /// placed again the frame its placement moves, even on the same source frame.
    placed_with: TransformParams,
    /// The clip's own opacity ([`TransformParams::opacity`]), applied as its
    /// coverage over the layers beneath rather than in its placement.
    opacity: f32,
    fit_canvas: Composer,
    rotated: Planes,
    stretched: Planes,
    graded: Planes,
    transformed: Planes,
    /// The decoder, and the file frame its next picture is.
    decoder: Option<(ClipDecoder, u32)>,
    /// The placed picture, and the file frame it is of.
    current: Option<(u32, Planes)>,
    /// The same picture composed at 10 bits ([`LayerStack::picture16`]), and the
    /// scratch only that path uses.
    current16: Option<(u32, Planes16)>,
    scratch16: Scratch16,
    widened: Planes16,
    steadied16: Planes16,
    adjusted16: Planes16,
    fx16: crate::effects::Scratch<u16>,
    /// Luma-sized coverage of the placed picture on the canvas: 255 where the
    /// clip's own pixels are, 0 in the letterbox bars and off the canvas. A
    /// layer above lets the one beneath show wherever this is 0.
    cover: Option<Vec<u8>>,
    matrix: Matrix,
    /// The dissolve this clip's tail owes the clip after it on its lane, when
    /// one is set and adjacent ([`cross_window`]).
    cross: Option<Box<LayerCross>>,
    /// The effects stack's scratch, and what it made of the current picture: its
    /// coverage (the placed footprint times the keys and masks) where it has any,
    /// and the mode it is laid down in ([`crate::effects`]).
    fx: crate::effects::Scratch<u8>,
    fx_cover: Option<Vec<u8>>,
    fx_mode: crate::effects::BlendMode,
    /// The clip's stabilisation, once its analysis is cached
    /// ([`crate::stabilise::correction`]): applied to the decoded picture, by its
    /// file frame, before anything else is.
    steady: Option<std::sync::Arc<crate::stabilise::Correction>>,
    steadied: Planes,
    /// The clip's deinterlace setting as it applies to its file. `Field` makes
    /// `info.rate` count fields; stabilisation is measured per file frame, so a
    /// field takes its frame's correction.
    deint: Deinterlace,
    /// An adjustment layer ([`crate::generate`]): no picture, its grade and effects act on the
    /// layers beneath ([`LayerFeed::adjust_over`]).
    adjust: bool,
    /// An adjustment layer's scratch, kept between frames like `steadied`: the placed and
    /// effected copy of the layers beneath, and its coverage.
    adjusted: Planes,
    adjust_cover: Vec<u8>,
    /// A generator with a transparent background ([`crate::generate::Gen::has_alpha`]) and its
    /// alpha at the size last asked for, kept so the mask is drawn once, not per coverage.
    alpha_gen: Option<crate::generate::Gen>,
    alpha_mask: Option<(u32, u32, Vec<u8>)>,
}

/// The clip a layer's tail dissolves into: its own feed, and the window of
/// timeline frames the blend runs over, from `start` for `window` frames.
struct LayerCross {
    next: LayerFeed,
    window: u32,
    start: u32,
}

/// The dissolve window a clip's tail owes its successor, as [`dissolve_window`]
/// reads it -- without the span rules, which a layered picture does not have:
/// it blends wherever its lane is on the clip, whatever is above it. `None` for
/// a hard cut, a stale transition out (the clips are no longer adjacent), or an
/// empty window. `(window, start)` in timeline frames.
fn cross_window(project: &Project, lane: Lane, idx: usize) -> Option<(u32, u32)> {
    let clips = project.lane(lane);
    let a = clips.get(idx)?;
    if a.transition_out == 0 {
        return None;
    }
    let b = clips.get(idx + 1)?;
    if a.end() != b.start {
        return None;
    }
    let window = a.transition_out.min(a.frames()).min(b.frames());
    (window > 0).then_some((window, a.end() - window))
}

impl LayerFeed {
    /// The feed for clip `idx` of `lane`, with the dissolve it owes its
    /// successor opened beside it. `paths`, when given, is the file each source
    /// is decoded from in place of its own (a proxy stand-in), by source index.
    /// `measure`, when given, is the cancel flag a stabilised clip's missing
    /// analysis is run under before this returns ([`crate::stabilise::ensure`])
    /// and the stage it reports to ([`Shared::measuring`]); `None` takes only a
    /// finished one (the viewer, which shows the analysis's progress and redraws
    /// when it lands).
    fn open(
        project: &Project,
        meta: &VideoMeta,
        lane: Lane,
        idx: usize,
        matrix: Matrix,
        paths: Option<&[PathBuf]>,
        measure: Option<Measure<'_>>,
    ) -> crate::Result<Self> {
        let mut feed = Self::plain(project, meta, lane, idx, matrix, paths, measure)?;
        if let Some((window, start)) = cross_window(project, lane, idx) {
            let next = Self::plain(project, meta, lane, idx + 1, matrix, paths, measure)?;
            feed.cross = Some(Box::new(LayerCross {
                next,
                window,
                start,
            }));
        }
        Ok(feed)
    }

    fn plain(
        project: &Project,
        meta: &VideoMeta,
        lane: Lane,
        idx: usize,
        matrix: Matrix,
        paths: Option<&[PathBuf]>,
        measure: Option<Measure<'_>>,
    ) -> crate::Result<Self> {
        let Some(clip) = project.lane(lane).get(idx).cloned() else {
            return Err("a layer names a clip that is not there".into());
        };
        let mut info = placed_info(project, meta, clip.source)?;
        if let Some(path) = paths.and_then(|paths| paths.get(clip.source)) {
            info.path = path.clone();
        }
        // Smoothed at the timeline's rate, as the viewer smooths it, over every
        // file frame the clip plays: an export measures what is missing rather
        // than write the clip unstabilised.
        let steady = match (clip.stabilise, project.sources().get(clip.source)) {
            (Some(st), Some(source)) => {
                let (lo, hi) = crate::stabilise::file_range(&clip, info.rate);
                match measure {
                    Some((cancel, stage)) => {
                        let mut tell = |f: f32| stage.store(1 + (f.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);
                        let c = crate::stabilise::ensure(&source.path, lo, hi, st, meta.frame_rate, &mut tell, cancel);
                        stage.store(0, Ordering::Relaxed);
                        Some(c?)
                    }
                    None => crate::stabilise::correction_or_start(&source.path, lo, hi, st, meta.frame_rate),
                }
            }
            _ => None,
        };
        let deint = crate::deinterlace::effective(clip.deinterlace, &info.path);
        info.rate = crate::deinterlace::counted_rate(deint, info.rate);
        Ok(Self {
            steady,
            steadied: Default::default(),
            deint,
            adjust: crate::generate::is_adjust(&info.path),
            adjusted: Default::default(),
            adjust_cover: Vec::new(),
            alpha_gen: crate::generate::Gen::parse(&info.path).filter(|g| g.has_alpha()),
            alpha_mask: None,
            info,
            meta: *meta,
            grade: project.color_of(lane, idx).copied(),
            graded_with: None,
            // The layer's opacity is its coverage in [`LayerStack::picture`], so
            // the placement below runs at opacity 1: a placed mask at opacity
            // below 1 would fade the coverage it is read for.
            transform: {
                let whole = project.transform_of(lane, idx).copied().unwrap_or_default();
                TransformParams {
                    opacity: 1.0,
                    ..whole
                }
            },
            opacity: project
                .transform_of(lane, idx)
                .map_or(1.0, |t| t.opacity),
            fit_canvas: Composer::new(meta.width, meta.height, project.fit_of(lane, idx)),
            clip,
            rotated: Default::default(),
            stretched: Default::default(),
            graded: Default::default(),
            transformed: Default::default(),
            decoder: None,
            current: None,
            current16: None,
            scratch16: Default::default(),
            widened: Default::default(),
            steadied16: Default::default(),
            adjusted16: Default::default(),
            fx16: Default::default(),
            cover: None,
            placed_with: TransformParams::default(),
            matrix,
            cross: None,
            fx: Default::default(),
            fx_cover: None,
            fx_mode: Default::default(),
        })
    }

    /// The dissolve picture this clip shows at timeline `frame` while its tail
    /// blends into the next clip on its lane: the two placed pictures mixed by
    /// [`dissolve_weight`], and the coverage between the two clips' footprints
    /// mixed the same way. `None` outside the window, and where the next clip's
    /// file ends first -- then this clip's own picture stands, as a single
    /// clip's does. The next clip is read forward as this one is.
    fn cross_at(&mut self, frame: u32) -> crate::Result<Option<(Planes, Vec<u8>)>> {
        let Some(cross) = self.cross.as_mut() else {
            return Ok(None);
        };
        if frame < cross.start || frame - cross.start >= cross.window {
            return Ok(None);
        }
        // The next clip shows its own picture from its first frame on, as the
        // single clip's dissolve reads it ([`compose_at_with`]): window frame k
        // is its frame k, not the timeline frame, which is before its start.
        let k = frame - cross.start;
        if !cross.next.advance(cross.next.clip.start + k)? {
            return Ok(None);
        }
        let t = dissolve_weight(k, cross.window);
        let a_cover = self.fx_cover.as_deref().or(self.cover.as_deref());
        let (Some((_, a)), Some(a_cover)) = (self.current.as_ref(), a_cover) else {
            return Ok(None);
        };
        let b_cover = cross.next.fx_cover.as_deref().or(cross.next.cover.as_deref());
        let (Some((_, b)), Some(b_cover)) = (cross.next.current.as_ref(), b_cover)
        else {
            return Ok(None);
        };
        let planes = crate::transitions::blend_planes(
            self.clip.transition_kind,
            (&a.0, &a.1, &a.2),
            (&b.0, &b.1, &b.2),
            t,
            self.meta.width as usize,
            self.meta.height as usize,
        );
        let cover = a_cover
            .iter()
            .zip(b_cover)
            .map(|(&x, &y)| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8)
            .collect();
        Ok(Some((planes, cover)))
    }

    /// [`LayerFeed::cross_at`] at 10 bits.
    fn cross_at16(&mut self, frame: u32) -> crate::Result<Option<(Planes16, Vec<u8>)>> {
        let Some(cross) = self.cross.as_mut() else {
            return Ok(None);
        };
        if frame < cross.start || frame - cross.start >= cross.window {
            return Ok(None);
        }
        let k = frame - cross.start;
        if !cross.next.advance16(cross.next.clip.start + k)? {
            return Ok(None);
        }
        let t = dissolve_weight(k, cross.window);
        let a_cover = self.fx_cover.as_deref().or(self.cover.as_deref());
        let (Some((_, a)), Some(a_cover)) = (self.current16.as_ref(), a_cover) else {
            return Ok(None);
        };
        let b_cover = cross.next.fx_cover.as_deref().or(cross.next.cover.as_deref());
        let (Some((_, b)), Some(b_cover)) = (cross.next.current16.as_ref(), b_cover) else {
            return Ok(None);
        };
        let planes = crate::transitions::blend_planes(
            self.clip.transition_kind,
            (&a.0, &a.1, &a.2),
            (&b.0, &b.1, &b.2),
            t,
            self.meta.width as usize,
            self.meta.height as usize,
        );
        let cover = a_cover
            .iter()
            .zip(b_cover)
            .map(|(&x, &y)| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8)
            .collect();
        Ok(Some((planes, cover)))
    }

    /// Makes the placed picture for timeline `frame` the current one, reading
    /// the decoder forward to it. `false` when the file ends first.
    fn advance(&mut self, frame: u32) -> crate::Result<bool> {
        self.advance_as(frame, false)
    }

    /// [`LayerFeed::advance`] at 10 bits: the placed picture lands in `current16`.
    fn advance16(&mut self, frame: u32) -> crate::Result<bool> {
        self.advance_as(frame, true)
    }

    fn advance_as(&mut self, frame: u32, wide: bool) -> crate::Result<bool> {
        let want = self.info.rate.source_at(clip_source_frame(&self.clip, frame));
        // Coverage is the layer's own (see `LayerFeed::picture`), so the placement
        // runs at opacity 1 whatever the keys say.
        let tf = TransformParams {
            opacity: 1.0,
            ..crate::project::clip_transform_at(&self.clip, self.transform, frame)
        };
        // A colour-keyed clip's grade moves with the frame ([`clip_color_at`]).
        let grade = match self.clip.color_keys.is_empty() {
            true => self.grade,
            false => Some(crate::project::clip_color_at(
                &self.clip,
                self.grade.unwrap_or_default(),
                frame,
            )),
        };
        let have = match wide {
            true => self.current16.as_ref().map(|(at, _)| *at),
            false => self.current.as_ref().map(|(at, _)| *at),
        };
        if have == Some(want)
            && self.placed_with == tf
            && self.graded_with == grade
            && !crate::effects::animated(&self.clip.effects)
        {
            return Ok(true);
        }
        // A decoder that has read past the picture wanted is reopened at it;
        // playback and export only ever move forward through a span -- down it,
        // for a reversed clip, whose decoder counts the file frame it is on
        // downwards.
        let backwards = self.clip.reversed;
        let behind = matches!(&self.decoder, Some((_, pos)) if match backwards {
            true => *pos < want,
            false => *pos > want,
        });
        if self.decoder.is_none() || behind {
            let path = self.info.path.clone();
            let flags = self.clip.visualizer;
            let decoder = if crate::is_audio(&path) || flags & 1 != 0 {
                ClipDecoder::visualizer(
                    &path,
                    self.info.stream,
                    want,
                    (self.meta.width, self.meta.height),
                    self.meta.frame_rate,
                    flags,
                    self.clip.viz_paint,
                )?
            } else if backwards && !crate::is_image(&path) {
                ClipDecoder::reversed(&path, want, self.info.rate.source_at(self.clip.in_frame), self.deint)
            } else {
                ClipDecoder::open_with(&path, want, None, None, wide, self.deint)?
            };
            self.decoder = Some((decoder, want));
        }
        let (decoder, pos) = self.decoder.as_mut().expect("opened above");
        while match backwards {
            true => *pos > want,
            false => *pos < want,
        } {
            if decoder.next()?.is_none() {
                return Ok(false);
            }
            *pos = if backwards { *pos - 1 } else { *pos + 1 };
        }
        let Some(picture) = decoder.next()? else {
            return Ok(false);
        };
        *pos = if backwards { pos.saturating_sub(1) } else { *pos + 1 };
        if wide {
            let (y, u, v, w, h): (&[u16], &[u16], &[u16], u32, u32) = match picture {
                Frame::Wide(y, u, v, w, h) => (y, u, v, w, h),
                Frame::Pixels(y, u, v, w, h) => {
                    widen_into(y, &mut self.widened.0);
                    widen_into(u, &mut self.widened.1);
                    widen_into(v, &mut self.widened.2);
                    (&self.widened.0[..], &self.widened.1[..], &self.widened.2[..], w, h)
                }
                Frame::Dma(_) => return Err("a layer's decoder produced no pixels".into()),
            };
            let (y, u, v) = match &self.steady {
                Some(c) => {
                    let m = c.at(crate::deinterlace::file_frame(self.deint, &self.info.path, want), w).ok_or_else(|| -> crate::Error {
                        format!("stabilise: file frame {want} lies before the measured range").into()
                    })?;
                    crate::stabilise::warp_i420(y, u, v, w, h, m, &mut self.steadied16);
                    (&self.steadied16.0[..], &self.steadied16.1[..], &self.steadied16.2[..])
                }
                None => (y, u, v),
            };
            let remap = remap_into(Some(self.info.color), self.info.mapper.is_some(), self.matrix);
            let (py, pu, pv, pw, ph) = place_picture16(
                y,
                u,
                v,
                w,
                h,
                self.info.rotation,
                self.info.pixel_aspect,
                grade,
                tf,
                remap,
                self.info.mapper.as_ref(),
                &mut self.scratch16,
                &mut self.fit_canvas,
            );
            let mut planes = (py.to_vec(), pu.to_vec(), pv.to_vec());
            if self.cover.is_none() || self.placed_with != tf {
                self.cover = Some(self.coverage(w, h, tf));
            }
            debug_assert_eq!((pw, ph), (self.meta.width, self.meta.height), "a placed layer is canvas-sized");
            let secs = f64::from(frame) / self.meta.frame_rate;
            let src = crate::project::key_sample_pos(&self.clip, frame);
            let (mode, fx_cover) = layer_effects(
                &self.clip.effects,
                &mut planes,
                (pw, ph),
                secs,
                src,
                self.matrix,
                &mut self.fx16,
                self.cover.as_deref().expect("set above"),
            );
            self.fx_mode = mode;
            self.fx_cover = fx_cover;
            self.placed_with = tf;
            self.graded_with = grade;
            self.current16 = Some((want, planes));
            return Ok(true);
        }
        let Frame::Pixels(y, u, v, w, h) = picture else {
            return Err("a layer's decoder produced no pixels".into());
        };
        let (y, u, v) = match &self.steady {
            Some(c) => {
                let m = c.at(crate::deinterlace::file_frame(self.deint, &self.info.path, want), w).ok_or_else(|| -> crate::Error {
                    format!("stabilise: file frame {want} lies before the measured range").into()
                })?;
                crate::stabilise::warp_i420(y, u, v, w, h, m, &mut self.steadied);
                (&self.steadied.0[..], &self.steadied.1[..], &self.steadied.2[..])
            }
            None => (y, u, v),
        };
        let remap = remap_into(
            Some(self.info.color),
            self.info.mapper.is_some(),
            self.matrix,
        );
        let (py, pu, pv, pw, ph) = place_picture(
            y,
            u,
            v,
            w,
            h,
            self.info.rotation,
            self.info.pixel_aspect,
            grade,
            tf,
            remap,
            self.info.mapper.as_ref(),
            &mut self.rotated,
            &mut self.stretched,
            &mut self.graded,
            &mut self.transformed,
            &mut self.fit_canvas,
        );
        let mut planes = (py.to_vec(), pu.to_vec(), pv.to_vec());
        if self.cover.is_none() || self.placed_with != tf {
            self.cover = Some(self.coverage(w, h, tf));
        }
        // The effects stack, on the placed picture: its filters rewrite the
        // samples, and its keys and masks narrow the footprint it lands with.
        debug_assert_eq!((pw, ph), (self.meta.width, self.meta.height), "a placed layer is canvas-sized");
        let secs = f64::from(frame) / self.meta.frame_rate;
        let src = crate::project::key_sample_pos(&self.clip, frame);
        let (mode, fx_cover) = layer_effects(
            &self.clip.effects,
            &mut planes,
            (pw, ph),
            secs,
            src,
            self.matrix,
            &mut self.fx,
            self.cover.as_deref().expect("set above"),
        );
        self.fx_mode = mode;
        self.fx_cover = fx_cover;
        self.placed_with = tf;
        self.graded_with = grade;
        self.current = Some((want, planes));
        Ok(true)
    }

    /// The layers composed so far, `base`, with this adjustment layer's grade, placement and
    /// effects applied to them, at strength `k` (its fade times its opacity).
    ///
    /// The clip's transform moves the composite below it (as Motion on an adjustment layer does
    /// in Premiere), so what the placement uncovers -- a shrunk, shifted or rotated composite --
    /// goes black. Crop is different: the layer only reaches where its crop footprint
    /// ([`LayerFeed::coverage`], placement left out) is, so a cropped-away region keeps the base.
    fn adjust_over(&mut self, base: Planes, k: f32, frame: u32) -> Planes {
        let (w, h) = (self.meta.width, self.meta.height);
        let grade = match self.clip.color_keys.is_empty() {
            true => self.grade,
            false => Some(crate::project::clip_color_at(&self.clip, self.grade.unwrap_or_default(), frame)),
        };
        let tf = TransformParams { opacity: 1.0, ..crate::project::clip_transform_at(&self.clip, self.transform, frame) };
        let (py, pu, pv, _, _) = place_picture(
            &base.0, &base.1, &base.2, w, h, Rotation::None, crate::demux::PixelAspect::SQUARE, grade, tf, None, None,
            &mut self.rotated, &mut self.stretched, &mut self.graded, &mut self.transformed, &mut self.fit_canvas,
        );
        let mut adjusted = std::mem::take(&mut self.adjusted);
        for (dst, src) in [(&mut adjusted.0, py), (&mut adjusted.1, pu), (&mut adjusted.2, pv)] {
            dst.clear();
            dst.extend_from_slice(src);
        }
        let mut cover = std::mem::take(&mut self.adjust_cover);
        cover.clear();
        // Only the crop limits the layer: the placement moved the composite itself, and
        // what it uncovers is the black of its own fit bars.
        // With crop and a transform together the crop footprint stays at the identity
        // position while the picture moves: the crop limits where the layer acts, not what moves.
        let crop = TransformParams { crop_l: tf.crop_l, crop_r: tf.crop_r, crop_t: tf.crop_t, crop_b: tf.crop_b, ..TransformParams::default() };
        match crop == TransformParams::default() {
            true => cover.resize(w as usize * h as usize, 255),
            false => cover.extend(self.coverage(w, h, crop)),
        }
        if crate::effects::active(&self.clip.effects) {
            let out = crate::effects::apply(
                &self.clip.effects,
                (&mut adjusted.0, &mut adjusted.1, &mut adjusted.2),
                w as usize,
                h as usize,
                f64::from(frame) / self.meta.frame_rate,
                crate::project::key_sample_pos(&self.clip, frame),
                self.matrix,
                &mut self.fx,
            );
            if let Some(alpha) = out.alpha {
                for (c, a) in cover.iter_mut().zip(alpha) {
                    *c = ((u32::from(*c) * u32::from(a) + 127) / 255) as u8;
                }
            }
        }
        let mut result = base;
        layer_over(&mut result, &adjusted, &cover, k, w as usize, h as usize);
        self.adjusted = adjusted;
        self.adjust_cover = cover;
        result
    }

    /// [`LayerFeed::adjust_over`] at 10 bits.
    fn adjust_over16(&mut self, base: Planes16, k: f32, frame: u32) -> Planes16 {
        let (w, h) = (self.meta.width, self.meta.height);
        let grade = match self.clip.color_keys.is_empty() {
            true => self.grade,
            false => Some(crate::project::clip_color_at(&self.clip, self.grade.unwrap_or_default(), frame)),
        };
        let tf = TransformParams { opacity: 1.0, ..crate::project::clip_transform_at(&self.clip, self.transform, frame) };
        let (py, pu, pv, _, _) = place_picture16(
            &base.0, &base.1, &base.2, w, h, Rotation::None, crate::demux::PixelAspect::SQUARE, grade, tf, None, None,
            &mut self.scratch16, &mut self.fit_canvas,
        );
        let mut adjusted = std::mem::take(&mut self.adjusted16);
        for (dst, src) in [(&mut adjusted.0, py), (&mut adjusted.1, pu), (&mut adjusted.2, pv)] {
            dst.clear();
            dst.extend_from_slice(src);
        }
        let mut cover = std::mem::take(&mut self.adjust_cover);
        cover.clear();
        let crop = TransformParams { crop_l: tf.crop_l, crop_r: tf.crop_r, crop_t: tf.crop_t, crop_b: tf.crop_b, ..TransformParams::default() };
        match crop == TransformParams::default() {
            true => cover.resize(w as usize * h as usize, 255),
            false => cover.extend(self.coverage(w, h, crop)),
        }
        if crate::effects::active(&self.clip.effects) {
            let out = crate::effects::apply(
                &self.clip.effects,
                (&mut adjusted.0, &mut adjusted.1, &mut adjusted.2),
                w as usize,
                h as usize,
                f64::from(frame) / self.meta.frame_rate,
                crate::project::key_sample_pos(&self.clip, frame),
                self.matrix,
                &mut self.fx16,
            );
            if let Some(alpha) = out.alpha {
                for (c, a) in cover.iter_mut().zip(alpha) {
                    *c = ((u32::from(*c) * u32::from(a) + 127) / 255) as u8;
                }
            }
        }
        let mut result = base;
        layer_over(&mut result, &adjusted, &cover, k, w as usize, h as usize);
        self.adjusted16 = adjusted;
        self.adjust_cover = cover;
        result
    }

    /// Where a `w` x `h` picture of this clip lands on the canvas, as luma: the
    /// same placement, run on a picture that is white everywhere, so the cover
    /// is exactly the placed picture's footprint -- transform, crop, fit bars
    /// and all -- without a second copy of that geometry to keep in step.
    fn coverage(&mut self, w: u32, h: u32, transform: TransformParams) -> Vec<u8> {
        // A generator with a transparent background ([`crate::generate::Gen::has_alpha`]) places
        // its alpha as the luma (limited range, so the fit bars are 0) and is covered by degree.
        // The mask is drawn at the picture's own size, so a canvas that is not the generator's
        // still gets a shape, not the full frame. It reaches `place_picture` as plain luma with
        // no grade, remap or tonemap (all `None` below): only geometry and resampling touch it.
        if let Some(g) = self.alpha_gen
            && !matches!(&self.alpha_mask, Some((mw, mh, _)) if (*mw, *mh) == (w, h))
        {
            let mask = crate::generate::Gen::new(g.kind, w, h).alpha().expect("an alpha generator has a mask");
            self.alpha_mask = Some((w, h, mask));
        }
        let alpha = self.alpha_gen.and(self.alpha_mask.as_ref()).map(|(_, _, m)| m);
        let ones: Vec<u8> = match alpha {
            Some(a) => a.iter().map(|&v| 16 + (u32::from(v) * 219 / 255) as u8).collect(),
            None => vec![255u8; w as usize * h as usize],
        };
        let chroma = vec![128u8; w.div_ceil(2) as usize * h.div_ceil(2) as usize];
        let (cy, _, _, _, _) = place_picture(
            &ones,
            &chroma,
            &chroma,
            w,
            h,
            self.info.rotation,
            self.info.pixel_aspect,
            None,
            transform,
            None,
            None,
            &mut self.rotated,
            &mut self.stretched,
            &mut self.graded,
            &mut self.transformed,
            &mut self.fit_canvas,
        );
        match alpha.is_some() {
            true => cy.iter().map(|&p| ((u32::from(p.clamp(16, 235)) - 16) * 255 / 219) as u8).collect(),
            false => cy.iter().map(|&p| if p >= 128 { 255 } else { 0 }).collect(),
        }
    }
}

/// A layer's effects stack drawn on its placed picture `planes` (a canvas of `size`): the
/// blend mode it lands in and the coverage its keys and masks leave of `cover`, `None` where
/// it has no effects (or none that cut the footprint). Generic over the sample so the 8-bit
/// and the 10-bit layer paths draw one stack the same way.
#[allow(clippy::too_many_arguments)]
fn layer_effects<T: crate::scale::Sample>(
    effects: &[crate::effects::Effect],
    planes: &mut Planes<T>,
    size: (u32, u32),
    secs: f64,
    src: f64,
    matrix: Matrix,
    scratch: &mut crate::effects::Scratch<T>,
    cover: &[u8],
) -> (crate::effects::BlendMode, Option<Vec<u8>>) {
    if !crate::effects::active(effects) {
        return (Default::default(), None);
    }
    let out = crate::effects::apply(
        effects,
        (&mut planes.0, &mut planes.1, &mut planes.2),
        size.0 as usize,
        size.1 as usize,
        secs,
        src,
        matrix,
        scratch,
    );
    let fx_cover = out.alpha.map(|alpha| {
        debug_assert_eq!(cover.len(), alpha.len(), "the cover and the effects' alpha are one size");
        cover
            .iter()
            .zip(alpha)
            .map(|(&c, a)| ((u32::from(c) * u32::from(a) + 127) / 255) as u8)
            .collect()
    });
    (out.mode, fx_cover)
}

/// A layered picture's clips, open together: read forward by timeline frame and
/// composed bottom to top ([`LayerStack::picture`]).
pub(crate) struct LayerStack {
    feeds: Vec<LayerFeed>,
    width: usize,
    height: usize,
}

impl LayerStack {
    /// The clips of `stack` ([`stack_at`]) on a `meta`-sized canvas, colour
    /// converted with `matrix`, decoded from `paths` where given (see
    /// [`LayerFeed::open`]). Nothing is decoded until a frame is asked for.
    pub(crate) fn open(
        project: &Project,
        meta: &VideoMeta,
        stack: &[(Lane, usize)],
        matrix: Matrix,
        paths: Option<&[PathBuf]>,
        measure: Option<Measure<'_>>,
    ) -> crate::Result<Self> {
        let feeds = stack
            .iter()
            .map(|&(lane, idx)| LayerFeed::open(project, meta, lane, idx, matrix, paths, measure))
            .collect::<crate::Result<Vec<_>>>()?;
        Ok(Self {
            feeds,
            width: meta.width as usize,
            height: meta.height as usize,
        })
    }

    /// The composed I420 picture at timeline `frame`, or `None` once a clip's
    /// file ends before it. The bottom clip is drawn exactly as a single clip
    /// is (its bars black); each clip above lands on it where its own picture
    /// covers, at its own fade -- so a fade on a picture on top shows the one
    /// beneath through it, rather than fading the whole frame. A clip whose
    /// tail is dissolving into the next clip on its lane shows the blend of the
    /// two over the window ([`LayerFeed::cross_at`]), and the layers above
    /// composite over that -- so a dissolve under a picture-in-picture blends
    /// the same as one without it.
    pub(crate) fn picture(&mut self, frame: u32) -> crate::Result<Option<Planes>> {
        let (w, h) = (self.width, self.height);
        let mut out: Option<Planes> = None;
        for feed in &mut self.feeds {
            if !feed.adjust && !feed.advance(frame)? {
                return Ok(None);
            }
            // The fade and the opacity both scale this layer's coverage.
            let opacity = crate::project::clip_transform_at(
                &feed.clip,
                TransformParams {
                    opacity: feed.opacity,
                    ..TransformParams::default()
                },
                frame,
            )
            .opacity;
            let opacity = crate::project::clip_opacity_at(&feed.clip, opacity, frame);
            let k = clip_fade(&feed.clip, frame).unwrap_or(1.0) * opacity;
            // An adjustment layer with nothing beneath it is a no-op. Adjacent adjustment clips
            // hard-cut: `cross_at` is skipped for them, so a dissolve between two does nothing.
            if feed.adjust {
                out = out.take().map(|base| feed.adjust_over(base, k, frame));
                continue;
            }
            let blended = feed.cross_at(frame)?;
            let (planes, cover): (&Planes, &[u8]) = match blended.as_ref() {
                Some((planes, cover)) => (planes, cover.as_slice()),
                None => {
                    let Some((_, planes)) = feed.current.as_ref() else {
                        return Ok(None);
                    };
                    let cover = feed.fx_cover.as_deref().or(feed.cover.as_deref());
                    (planes, cover.expect("set with the picture"))
                }
            };
            // A clip whose effects key, mask or blend it is laid over what is
            // beneath -- black, at the bottom -- by [`crate::effects::blend_over`].
            let mode = feed.fx_mode;
            if feed.fx_cover.is_some() || mode != crate::effects::BlendMode::Normal {
                let base = out.get_or_insert_with(|| crate::scale::black_i420(w, h));
                crate::effects::blend_over(
                    (&mut base.0, &mut base.1, &mut base.2),
                    borrow_planes(planes),
                    cover,
                    k,
                    mode,
                    feed.matrix,
                    w,
                    h,
                );
                continue;
            }
            // A picture with a transparent background lands on black, as the bottom layer does.
            if out.is_none() && feed.alpha_gen.is_some() {
                out = Some(crate::scale::black_i420(w, h));
            }
            match out.as_mut() {
                // The bottom layer sits on black, as a single clip does.
                None => {
                    out = Some(match k < 1.0 {
                        true => fade_planes(borrow_planes(planes), k),
                        false => planes.clone(),
                    });
                }
                Some(base) => layer_over(base, planes, cover, k, w, h),
            }
        }
        // Nothing drew (a stack of adjustment layers alone) is a gap: black.
        Ok(Some(out.unwrap_or_else(|| crate::scale::black_i420(w, h))))
    }

    /// [`LayerStack::picture`] at 10 bits: 10-bit layers keep every sample, 8-bit ones are
    /// widened where they meet, and the grade, the placement, the blends and the effects all
    /// run on 10-bit codes.
    pub(crate) fn picture16(&mut self, frame: u32) -> crate::Result<Option<Planes16>> {
        let (w, h) = (self.width, self.height);
        let mut out: Option<Planes16> = None;
        for feed in &mut self.feeds {
            if !feed.adjust && !feed.advance16(frame)? {
                return Ok(None);
            }
            let opacity = crate::project::clip_transform_at(
                &feed.clip,
                TransformParams {
                    opacity: feed.opacity,
                    ..TransformParams::default()
                },
                frame,
            )
            .opacity;
            let opacity = crate::project::clip_opacity_at(&feed.clip, opacity, frame);
            let k = clip_fade(&feed.clip, frame).unwrap_or(1.0) * opacity;
            if feed.adjust {
                out = out.take().map(|base| feed.adjust_over16(base, k, frame));
                continue;
            }
            let blended = feed.cross_at16(frame)?;
            let (planes, cover): (&Planes16, &[u8]) = match blended.as_ref() {
                Some((planes, cover)) => (planes, cover.as_slice()),
                None => {
                    let Some((_, planes)) = feed.current16.as_ref() else {
                        return Ok(None);
                    };
                    let cover = feed.fx_cover.as_deref().or(feed.cover.as_deref());
                    (planes, cover.expect("set with the picture"))
                }
            };
            let mode = feed.fx_mode;
            if feed.fx_cover.is_some() || mode != crate::effects::BlendMode::Normal {
                let base = out.get_or_insert_with(|| crate::scale::black_i420_wide(w, h));
                crate::effects::blend_over(
                    (&mut base.0, &mut base.1, &mut base.2),
                    borrow_planes(planes),
                    cover,
                    k,
                    mode,
                    feed.matrix,
                    w,
                    h,
                );
                continue;
            }
            if out.is_none() && feed.alpha_gen.is_some() {
                out = Some(crate::scale::black_i420_wide(w, h));
            }
            match out.as_mut() {
                None => {
                    out = Some(match k < 1.0 {
                        true => fade_planes(borrow_planes(planes), k),
                        false => planes.clone(),
                    });
                }
                Some(base) => layer_over(base, planes, cover, k, w, h),
            }
        }
        Ok(Some(out.unwrap_or_else(|| crate::scale::black_i420_wide(w, h))))
    }
}

/// `top` laid over `base` where `cover` says the picture is, at strength `k`
/// (its fade times 1). Luma takes each sample's own coverage; chroma takes the
/// coverage of the luma sample at its block's top left.
fn layer_over<T: crate::scale::Sample>(base: &mut Planes<T>, top: &Planes<T>, cover: &[u8], k: f32, w: usize, h: usize) {
    let mix = |b: T, t: T, a: f32| {
        let (b, t) = (b.code() as f32, t.code() as f32);
        T::from_f32(b + (t - b) * a)
    };
    for (i, (b, &t)) in base.0.iter_mut().zip(&top.0).enumerate() {
        let a = f32::from(cover[i]) / 255.0 * k;
        if a > 0.0 {
            *b = mix(*b, t, a);
        }
    }
    let cw = w.div_ceil(2);
    for i in 0..cw * h.div_ceil(2) {
        let (cx, cy) = (i % cw, i / cw);
        let a = f32::from(cover[2 * cy * w + 2 * cx]) / 255.0 * k;
        if a > 0.0 {
            base.1[i] = mix(base.1[i], top.1[i], a);
            base.2[i] = mix(base.2[i], top.2[i], a);
        }
    }
}

/// A layered span as a playback stream: its composed frames, one per picture
/// change, decoded on a thread of their own. Each is `view`-sized and numbered
/// the way a decoder numbers its pictures -- the span's first source frame plus
/// the speed's step -- so the playback clock maps it like any other clip.
pub(crate) fn layered_stream(
    project: Project,
    view: VideoMeta,
    out_color: ColorDescription,
    span: crate::project::Span,
    paths: Option<Vec<PathBuf>>,
) -> crate::decode::FrameStream {
    let stack = stack_at(&project, span.start);
    let base = span.from.map_or(0, |(_, in_frame)| in_frame);
    crate::decode::DecodeSession::spawn_stream(
        "layers",
        crate::decode::Backend::Software,
        move |tx, cancel| {
            let Ok(mut layers) =
                LayerStack::open(&project, &view, &stack, out_color.matrix, paths.as_deref(), None)
            else {
                return;
            };
            let mut last = None;
            for offset in 0..span.len {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                // One picture per source step, as a decoder yields one per frame:
                // a slowed clip's repeats are held by the clock, not decoded again.
                let step = span.source_offset(offset);
                if last == Some(step) {
                    continue;
                }
                last = Some(step);
                let Ok(Some((y, u, v))) = layers.picture(span.start + offset) else {
                    return;
                };
                let bgra = crate::convert::i420_to_bgra(
                    &out_color,
                    &y,
                    &u,
                    &v,
                    view.width as usize,
                    view.height as usize,
                );
                let frame = crate::decode::Frame {
                    index: base + step,
                    width: view.width,
                    height: view.height,
                    bgra,
                };
                if tx.send(frame).is_err() {
                    return;
                }
            }
        },
    )
}

/// The cross-dissolve this span's tail owes its successor, or `None` for a
/// hard cut. `(b_source, b_in_frame, window, b_start)`: which source and
/// source frame the dissolve's B side starts reading, how many *output*
/// frames the blend ramps across, and where B sits on the timeline (which is
/// what its own grade and fit are keyed on, [`Project::composite_color_at`]
/// and [`Project::composite_fit_at`]).
///
/// [`Clip::transition_out`] is trusted only after adjacency is checked again
/// here: a trim or a split since it was set leaves the stored number in
/// place but not its meaning, so a stale one plays as the hard cut it now
/// is rather than as a dissolve into a clip that no longer follows.
pub(crate) fn dissolve_window(
    project: &Project,
    span: &crate::project::Span,
) -> Option<(usize, u32, u32, u32)> {
    // A layered picture is composed beneath its top clip, and a dissolve of that
    // clip would blend a picture of one clip alone -- the layers are not blended.
    if layered_at(project, span.start) {
        return None;
    }
    let (source, _) = span.from?;
    let (lane, idx) = project.composite_clip_at(span.start)?;
    let clips = project.lane(lane);
    let a = clips.get(idx)?;
    if a.source != source || a.transition_out == 0 {
        return None;
    }
    let b = clips.get(idx + 1)?;
    if a.end() != b.start {
        return None; // trimmed or split apart since the dissolve was set
    }
    // The span must run uninterrupted all the way to A's own end: a higher
    // video lane taking over before that means there is no tail of A left
    // here to blend into anything.
    if span.start + span.len != a.end() {
        return None;
    }
    let window = a
        .transition_out
        .min(a.frames())
        .min(b.frames())
        .min(span.len);
    (window > 0).then_some((b.source, b.in_frame, window, b.start))
}

/// `B`'s weight in [`crate::scale::blend_i420`] at output frame `idx_in_window` of `window`
/// (`idx_in_window < window`): a ramp from just above 0 to just below 1 that
/// never actually reaches either end, so the first blended frame still shows
/// a trace of A and the last still a trace of B -- the cut a hard transition
/// would have made lands one frame *after* the window, at full B, rather than
/// inside it.
pub(crate) fn dissolve_weight(idx_in_window: u32, window: u32) -> f32 {
    (idx_in_window + 1) as f32 / (window + 1) as f32
}

/// The picture's black fade at timeline `frame`: the share of the picture that
/// survives, `None` where no fade reaches it. The clip on screen
/// ([`Project::composite_clip_at`]) owns its own fades -- [`Clip::fade_in`] ramps
/// from black on its first frame to the whole picture at `fade_in`, and
/// [`Clip::fade_out`] ramps back to black over its last `fade_out` frames, the
/// same edges [`crate::project::Fade`] ramps the sound on. A linear ramp: the
/// first frame of a fade-in is pure black and the frame a fade-out ends on is
/// one step short of it, so nothing shows the picture at full strength while
/// the ramp is still running.
pub(crate) fn picture_fade_at(project: &Project, frame: u32) -> Option<f32> {
    let (lane, idx) = project.composite_clip_at(frame)?;
    clip_fade(project.lane(lane).get(idx)?, frame)
}

/// [`picture_fade_at`] for one clip: the share of its picture that survives at
/// `frame`. A layered picture asks it of each clip, so a fade belongs to the
/// clip it is on.
fn clip_fade(clip: &crate::project::Clip, frame: u32) -> Option<f32> {
    if clip.fade_in == 0 && clip.fade_out == 0 {
        return None;
    }
    let pos = frame.checked_sub(clip.start)?;
    let total = clip.frames();
    let mut k = 1.0f32;
    if clip.fade_in > 0 && pos < clip.fade_in {
        k = pos as f32 / clip.fade_in as f32;
    }
    if clip.fade_out > 0 && pos >= total.saturating_sub(clip.fade_out) {
        k = k.min((total - pos) as f32 / clip.fade_out as f32);
    }
    (k < 1.0).then_some(k)
}

/// Whether any frame of `span` is inside a picture fade ([`picture_fade_at`]):
/// the span's pixels then have to be touched, which a zero-copy span never is.
fn picture_faded_in(project: &Project, span: &crate::project::Span) -> bool {
    (span.start..span.start.saturating_add(span.len))
        .any(|frame| picture_fade_at(project, frame).is_some())
}

/// Whether any video clip on the timeline has a picture fade -- the
/// picture-side twin of [`faded`], which asks the same of sound: a fade is
/// pixel math, so a stream copy cannot carry one.
fn picture_faded(project: &Project) -> bool {
    project
        .lanes()
        .into_iter()
        .filter(|lane| lane.kind == LaneKind::Video)
        .any(|lane| {
            project
                .lane(lane)
                .iter()
                .any(|c| c.fade_in > 0 || c.fade_out > 0)
        })
}

/// `planes` drawn toward black by `k` ([`picture_fade_at`]): luma moves toward
/// 16 and chroma toward 128, the black a gap is composed as, so a faded frame
/// and a gap agree on what black is.
fn fade_planes<T: Sample>(planes: (&[T], &[T], &[T]), k: f32) -> Planes<T> {
    let toward = |s: &[T], black: T| -> Vec<T> {
        s.iter()
            .map(|&p| {
                let (black, p) = (black.code() as f32, p.code() as f32);
                T::from_f32(black + (p - black) * k)
            })
            .collect()
    };
    (
        toward(planes.0, T::BLACK),
        toward(planes.1, T::NEUTRAL),
        toward(planes.2, T::NEUTRAL),
    )
}

/// A dissolve's B side, open beside A's own decoder for this span's tail:
/// its own decoder, and everything [`place_picture`] needs to put its frames
/// on the same canvas, in the same space, graded the way B is graded rather
/// than the way A is -- [`Project::composite_color_at`]/`composite_fit_at`
/// asked at B's own start, not at the timeline position the blend writes to,
/// which is still inside A's span.
struct Dissolve<'a> {
    /// How many output frames the blend ramps across.
    window: u32,
    kind: crate::transitions::TransitionKind,
    /// The first output frame of this span's tail that is inside the window.
    tail_start: u32,
    decoder: ClipDecoder,
    mapper: Option<&'a ToneMapper>,
    remap: Option<Remap>,
    grade: Option<crate::color::ColorParams>,
    transform: TransformParams,
    /// B's own file's display-matrix turn, [`place_picture`]'s first stage;
    /// B is a different file and may be turned differently from A.
    rotation: Rotation,
    /// B's own file's sample aspect, [`place_picture`]'s second stage, for
    /// the same reason the turn is per file.
    pixel_aspect: crate::demux::PixelAspect,
    canvas: Composer,
    /// The turn's, the grade's and the transform's scratch, exactly as A's
    /// span keeps them: refilled per picture, allocated once.
    rotated: (Vec<u8>, Vec<u8>, Vec<u8>),
    /// The stretch's own scratch, [`place_picture`]'s second stage -- the
    /// pixel aspect's, beside the turn's.
    stretched: (Vec<u8>, Vec<u8>, Vec<u8>),
    graded: (Vec<u8>, Vec<u8>, Vec<u8>),
    transformed: (Vec<u8>, Vec<u8>, Vec<u8>),
    /// The 10-bit scratch of B's placement, for a 10-bit export.
    scratch16: Scratch16,
}

/// The Videos folder a default export lands in: the user's own (`XDG_VIDEOS_DIR`,
/// else the one `~/.config/user-dirs.dirs` names, else `~/Videos`), then the
/// project's folder, never the media's own folder by default.
pub fn default_export_folder(project: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_VIDEOS_DIR").map(PathBuf::from).or_else(|| {
        let home = home.as_ref()?;
        let text = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).ok()?;
        let line = text.lines().find_map(|l| l.trim().strip_prefix("XDG_VIDEOS_DIR="))?;
        let value = line.trim().trim_matches('"');
        Some(PathBuf::from(value.replace("$HOME", &home.to_string_lossy())))
    });
    let videos = xdg.filter(|d| d.is_dir()).or_else(|| home.map(|h| h.join("Videos")).filter(|d| d.is_dir()));
    videos
        .or_else(|| project.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The track's cues where the *exported* timeline puts them, which is the only
/// clock the file has: the export writes timeline frame 0 onwards and nothing
/// else, exactly as the sound is mixed from frame 0 and resized to the
/// timeline's own length ([`encode_audio`]).
///
/// Which clock the cues arrive in is what the track itself says:
///
/// * a track out of a *file* (`SubtitleTrack::track`) is timed against that
///   file, so each cue is carried through the spans that play it -- a cue over
///   a stretch that was cut out is gone, one straddling a cut keeps the half
///   that is still there, and one after a rippled delete moves back with the
///   picture it belongs to. The *edges* a cut clips to are frames -- that is
///   what a cut is -- but a cue nothing clipped keeps its own microseconds, so
///   an untouched timeline exports the times the source states.
/// * a *standalone* file (`subs.srt`) is timed against nothing else -- there is
///   no clip to hang it on and no offset in the project to shift it by -- so it
///   is the timeline's own clock, which is where it was drawn while editing
///   (`Player::subtitle_overlay`). Clipped to the exported length and no more.
///
/// This is also what the *preview* draws: the plate over the picture and the
/// timeline strip both ask it through
/// [`PlaybackSession::timeline_cues`](crate::PlaybackSession::timeline_cues), so
/// what a rippled timeline shows and what its export writes are one answer by
/// construction rather than two maps kept in step by hand.
///
/// Pure and cheap enough to ask per repaint (a walk of the spans and of the
/// cues, no file opened), which is how a front-end asks it.
pub fn timeline_cues(project: &Project, track: &SubtitleTrack, fps: f64) -> Vec<Cue> {
    let frames = project.timeline_frames();
    let us = |frame: u32| (f64::from(frame) / fps * 1e6).round() as i64;
    let end = us(frames);
    // The file the cues are timed against, when there is one on this timeline.
    // A track whose media has since been dropped from the project has no span
    // to be carried through and falls back to the timeline's clock, which is
    // what it was drawn on.
    //
    // Through the same `canonical` a source is indexed by, or the two spellings
    // of one file (an argv `../assets/film.mkv`, a symlinked folder) would read
    // as two files and the cues would quietly fall back to the timeline's clock.
    let source = track.track.and_then(|_| {
        let path = crate::project::canonical(&track.path);
        project.sources().iter().position(|s| s.path == path)
    });
    let Some(source) = source else {
        return track
            .cues
            .iter()
            .filter_map(|cue| {
                let (start_us, end_us) = (cue.start_us.max(0), cue.end_us.min(end));
                (end_us > start_us).then_some(Cue {
                    start_us,
                    end_us,
                    text: cue.text.clone(),
                    image: cue.image.clone(),
                    words: crate::subtitle::map_words(&cue.words, (start_us, end_us), (start_us, end_us)),
                    karaoke: crate::subtitle::map_karaoke(&cue.karaoke, (start_us, end_us), (start_us, end_us)),
                })
            })
            .collect();
    };
    let mut out = Vec::new();
    for span in stack_spans_from(project, 0) {
        let Some((_, in_frame)) = span.from.filter(|&(from, _)| from == source) else {
            continue;
        };
        // What of the file this span plays, as *time in the file*: a clip counts
        // the timeline's frames whatever its file was shot at ([`Rate`]), which
        // is what makes those frames the same seconds a cue is timed in.
        let (first, last) = match span.source_window() {
            Some((_, lo, hi)) => (us(lo), us(hi)),
            None => (us(in_frame), us(in_frame + span.source_len())),
        };
        // Where that lands, and how fast: a cue is carried by the same factor
        // the pictures under it are, so a slowed clip stretches its subtitles
        // with it instead of leaving them behind.
        let start = us(span.start);
        // `rel` is the time since the span's head in the file's own direction of play:
        // forwards from `first`, and for a reversed clip back from `last` (the cue
        // [s, e] is mirrored to [last - e, last - s]).
        let onto = |rel: i64| {
            let Some(_) = span.ramp else {
                return start + (rel as f64 / span.speed.as_f64()).round() as i64;
            };
            // A ramp has no one factor: carry the cue through the ramp's own
            // inverse (`Span::timeline_at`), interpolating between whole frames.
            let f = (rel as f64 / 1e6 * fps).max(0.0);
            let (lo, hi) = (f.floor() as u32, f.ceil() as u32);
            let at = |s: u32| f64::from(span.timeline_at(in_frame + s).min(span.start + span.len) - span.start);
            let (a, b) = (at(lo), at(hi));
            let frames = if hi == lo { b } else { a + (b - a) * (f - f64::from(lo)) };
            start + (frames / fps * 1e6).round() as i64
        };
        for cue in &track.cues {
            let (a, b) = (cue.start_us.max(first), cue.end_us.min(last));
            if b <= a {
                continue; // wholly outside what this span kept
            }
            let (from, to) = if span.reverse { (last - b, last - a) } else { (a - first, b - first) };
            out.push(Cue {
                start_us: onto(from),
                end_us: onto(to).min(end),
                text: cue.text.clone(),
                image: cue.image.clone(),
                // Each word through the same mapping as the cue, so a ramp places it where
                // its own sound plays; a reversed clip keeps none, its cue shown whole.
                words: if span.reverse {
                    Vec::new()
                } else {
                    cue.words
                        .iter()
                        .filter(|(s, _, _)| *s < b)
                        .map(|(s, e, w)| {
                            let at = |t: i64| onto(t.clamp(a, b) - first).min(end);
                            (at(*s), at(*e), w.clone())
                        })
                        .collect()
                },
                karaoke: if span.reverse {
                    Vec::new()
                } else {
                    cue.karaoke
                        .iter()
                        .filter(|k| k.start_us < b)
                        .map(|k| {
                            let at = |t: i64| onto(t.clamp(a, b) - first).min(end);
                            crate::subtitle::Syllable { start_us: at(k.start_us), end_us: at(k.end_us), ..k.clone() }
                        })
                        .collect()
                },
            });
        }
    }
    // The spans come in timeline order but a cue's *own* order across them does
    // not: a Matroska muxer writes blocks as they come up.
    out.sort_by_key(|cue| cue.start_us);
    out
}

/// What [`start`] would do about the subtitles, in the words a card shows: what
/// travels, and the reason beside every pick that does not. Pure, like
/// [`planned_audio`]: no file is opened, so it may be asked per repaint.
///
/// `picks` is any run of indices into [`Project::subtitles`] -- an
/// [`ExportSettings::subtitles`] list (`picks.iter().copied()`) as much as a
/// single `Some(row)`, which is what a front-end holding one row still hands it.
pub fn planned_subtitles(
    project: &Project,
    format: Format,
    picks: impl IntoIterator<Item = usize>,
) -> String {
    planned_subtitles_in(project, format, picks, None, 0.0)
}

/// [`planned_subtitles`] for an export of `range` (the frames a ranged file keeps) at `fps`: a
/// track or lane with no cue left in the range is named as dropped for that reason, since the
/// file would carry it empty. `None` is the whole timeline, and `fps` is then not read.
pub fn planned_subtitles_in(
    project: &Project,
    format: Format,
    picks: impl IntoIterator<Item = usize>,
    range: Option<(u32, u32)>,
    fps: f64,
) -> String {
    let picks: Vec<usize> = picks.into_iter().collect();
    let lanes = placed_lanes(project);
    // WebM is the one container that names no subtitle track here, placed or
    // picked: said first and once, as the reason nothing travels.
    if format == Format::Webm && (!picks.is_empty() || !lanes.is_empty()) {
        return "none — a WebM file carries no subtitles".into();
    }
    if format == Format::ProRes && (!picks.is_empty() || !lanes.is_empty()) {
        return "none — a ProRes QuickTime file carries no subtitles".into();
    }
    // An mp4 of a timeline with no picture is the sound alone too: no track of
    // either kind is written, placed or picked ([`export_subtitles`]).
    if format.ext() == "mp4" && !has_picture(project) {
        return "none — a timeline with no picture is the sound alone".into();
    }
    // One statement about the *file*, said before any about a track: a format
    // that is the sound alone has nowhere to put a single one of them, and the
    // reason a pick would not have travelled anyway (a picture track, no cues)
    // is not the reason it does not travel here. Nothing picked is "none" --
    // there is nothing to say a container cannot carry.
    if !format.has_video() && (!picks.is_empty() || !lanes.is_empty()) {
        return "none — this format is the sound alone".into();
    }
    // The lanes are the words placed on the timeline and go as they are placed
    // ([`export_subtitles`]); each picked track no lane places is written whole,
    // beside them. The clauses go in the order the file writes the tracks
    // ([`soft_order`]), which is the order the caller gave them.
    let (mut embedded, mut dropped) = (Vec::new(), Vec::new());
    let (mut lane_count, mut trailing) = (0, false);
    for entry in soft_order(project, &lanes, &picks) {
        let pick = match entry {
            SoftEntry::Lane(lane) => {
                let (e, d, t) = planned_lanes(project, &[lane], &picks, range, fps);
                lane_count += e.len();
                embedded.extend(e);
                dropped.extend(d);
                trailing |= t;
                continue;
            }
            SoftEntry::Track(pick) => pick,
        };
        // A pick the project has no row for: a caller holding an index the list
        // no longer has (a removed row shifts every later one down). Every other
        // reason a pick does not travel is named on this card, so this one is
        // too -- a bug that says nothing is a bug found in the finished file.
        let Some(track) = project.subtitles().get(pick) else {
            dropped.push(format!("#{pick} — no such track"));
            continue;
        };
        if let Some(why) = &track.refused {
            dropped.push(format!("{} — {why}", track.label));
        } else if track.is_bitmap() {
            // Drawn over the picture, written into no file: the exported track
            // is text and these cues are bitmaps. Said whatever the format,
            // because it is the track and not the container that cannot be
            // carried.
            dropped.push(format!("{} — pictures; drawn, not written", track.label));
        } else if !track_has_cues(project, track, range, fps) {
            dropped.push(format!("{} — {}", track.label, no_cues_word(range)));
        } else {
            // Whichever container: a Matroska carries the track as
            // `S_TEXT/UTF8` blocks and an mp4 as a `tx3g` timed-text track, and
            // a pick travels either way. There is no container refusal left on
            // this card -- the sound-alone one above is about the *file* and the
            // ones beside it about the *track*.
            embedded.push(track.label.clone());
        }
    }
    // Lanes only are named as lanes; a picked track in the mix makes them tracks.
    let noun = if lane_count == embedded.len() { "lanes" } else { "tracks" };
    // A Matroska block numbers its track in one byte, so a file carries at most
    // MAX_SUB_TRACKS of them, placed or picked; the muxer refuses more again.
    if format.is_mkv() && embedded.len() > crate::mux::MAX_SUB_TRACKS {
        return format!(
            "{} subtitle {noun}: a Matroska block writes its track number in one byte, so one file \
             carries at most {}",
            embedded.len(),
            crate::mux::MAX_SUB_TRACKS
        );
    }
    let mut parts = match embedded.len() {
        0 => Vec::new(),
        1 => vec![format!("{} → embedded", embedded[0])],
        n => vec![format!("{n} {noun} → embedded ({})", embedded.join(", "))],
    };
    parts.extend(dropped);
    if trailing {
        parts.push("cues past the last picture are cut off there".into());
    }
    match parts.is_empty() {
        true => "none".into(),
        false => parts.join("; "),
    }
}

/// [`planned_subtitles`] for a timeline that *places* its words: one clause per
/// subtitle lane, named by the lane and by the palette row it shows, in the
/// order the file will declare them ([`lane_subtitles`]).
///
/// Every reason a lane does not travel is said here rather than found in the
/// file: a picture track placed on it, a track that could not be read, a palette
/// row that has since gone, a lane sitting entirely past the last picture, and
/// -- before a byte is written -- more lanes than a Matroska block can number
/// ([`crate::mux::MAX_SUB_TRACKS`], which the muxer refuses again).
fn planned_lanes(
    project: &Project,
    lanes: &[Lane],
    picks: &[usize],
    range: Option<(u32, u32)>,
    fps: f64,
) -> (Vec<String>, Vec<String>, bool) {
    let end = project.media_frames();
    let (mut embedded, mut dropped, mut trailing) = (Vec::new(), Vec::new(), false);
    for &lane in lanes {
        let placed = project.sub_lane(lane);
        // An empty lane is a lane nobody has used yet, not a refusal. A lane no
        // pick reaches is not in this file at all, so it is not named either.
        let Some(first) = placed.first() else {
            continue;
        };
        if !picked(placed, picks) {
            continue;
        }
        let row = |s: &crate::project::SubClip| project.subtitles().get(s.track);
        let (label, head) = (lane.label(), row(first));
        let name = head.map_or_else(String::new, |t| format!("{} ", t.label));
        if let Some(gone) = placed.iter().find(|s| row(s).is_none()) {
            dropped.push(format!("{label} — #{} is no longer a track", gone.track));
        } else if let Some((t, why)) = placed
            .iter()
            .find_map(|s| row(s).and_then(|t| t.refused.as_ref().map(|why| (t, why))))
        {
            dropped.push(format!("{label} {} — {why}", t.label));
        } else if let Some(t) = placed.iter().find_map(|s| row(s).filter(|t| t.is_bitmap())) {
            // Drawn over the picture, written into no file -- the same sentence
            // a picked track gets, because it is the track and not the lane that
            // cannot be carried.
            dropped.push(format!(
                "{label} {} — pictures; drawn, not written",
                t.label
            ));
        } else if first.start >= end {
            dropped.push(format!("{label} {name}— past the last picture"));
        } else if range.is_some() && lane_subtitle(project, lane, fps, range, picks).is_none() {
            dropped.push(format!("{label} {name}— {}", no_cues_word(range)));
        } else {
            embedded.push(format!("{label} {}", name.trim_end()));
            // Something on it hangs past the end of the media: the timeline is
            // held open under it ([`Project::timeline_frames`]) but the file
            // stops with the last picture, so those cues are cut off there.
            trailing |= placed.last().is_some_and(|s| s.end() > end);
        }
    }
    (embedded, dropped, trailing)
}

/// What [`start`] would encode the picture with, probed the way the export
/// probes it -- the very encoder is opened and closed again -- so this is a
/// measurement and not a promise. `None` for a format that carries no picture.
///
/// Costs that open (~100 ms here): ask it off a render thread and keep the
/// answer until the format, the resolution or the bitrate changes.
pub fn planned_video(meta: &VideoMeta, settings: &ExportSettings) -> Option<&'static str> {
    settings
        .format
        .has_video()
        .then(|| match hw_seat(meta, settings) {
            Ok(hw) => video_label(settings.format, hw.is_some(), settings.intra_only),
            // The seat the settings demand and this machine has not: the very
            // refusal [`start`] would settle with, said on the card instead.
            Err(_) => NO_HW_SEAT,
        })
}

/// What [`start`] would open for *this* timeline: the picture's seat -- or the
/// copy that means there is no encoder at all -- and the sound's, asked of the
/// very gates the export asks ([`CopyPlan::of`] and [`copy_audio`]'s) rather
/// than of the format alone.
///
/// This exists because the card and the running export used to disagree in both
/// directions on the same file: the card named a software HEVC encode for a cut
/// the engine copied, and named an AAC copy for a Matroska source whose sound
/// no sample table holds and which therefore always re-encodes. A prediction
/// that is not the decision is worse than no prediction, because a person plans
/// their evening around it.
///
/// Costs what [`planned_video`] costs (an encoder opened and closed) plus a
/// header read per source and, once per file, its cluster walk: ask it off a
/// render thread and keep the answer until the timeline or the settings change.
pub fn planned_seats(
    project: &Project,
    meta: &VideoMeta,
    settings: &ExportSettings,
) -> (Option<&'static str>, &'static str) {
    // The canvas the file is drawn at, which is what the seat and the copy gate are about.
    let meta = &output_meta(meta, settings);
    let video = settings.format.has_video().then(|| {
        match CopyPlan::of(project, meta, settings).is_some() {
            true => "copy (source packets)",
            false => planned_video(meta, settings).unwrap_or(NO_HW_SEAT),
        }
    });
    // The half [`audio_label`] cannot know from the format: a copy is a *sample
    // table's* packets ([`copy_audio`] hands them to `copy_multi_streams`), and
    // a Matroska file has none -- so every mkv source re-encodes, however
    // untouched its lane is. Extension only, which is what `is_matroska`
    // reads, so this costs nothing beyond what is already open. A standalone
    // audio file (mp3, wav, flac...) has no sample table either: its sound is
    // decoded and encoded again, as [`copy_multi_streams`] refusing it sends it.
    let copyable = !forces_encode(project, settings.range.is_some())
        && !project.sources().iter().any(|source|
    {
        crate::demux::is_matroska(&source.path)
            || crate::demux::is_mpeg_system(&source.path)
            || crate::is_audio(&source.path)
    });
    let sound = match undecodable_sound(project) {
        Some(label) if has_sound(project) => label,
        // A Matroska file that is encoded (not copied) takes Opus or AAC as the mix decides, so
        // the card names the one this mix will get, read from the sources' headers.
        None if has_sound(project)
            && !copyable
            && settings.format.is_mkv()
            && settings.format != Format::Webm =>
        {
            match mkv_mix_is_opus(project, meta.frame_rate) {
                true => "Opus · SW encode (ec-opus)",
                false => "AAC · SW encode (ec-aac)",
            }
        }
        _ => audio_label(
            project,
            settings.format,
            has_sound(project),
            Some(copyable),
            settings.range.is_some(),
        ),
    };
    (video, sound)
}

/// Whether the mix a Matroska export encodes would be written as Opus: the rule
/// [`encode_audio`] applies after the mix is open -- 48 kHz, mono or stereo -- worked
/// out from the lanes' sources' headers instead ([`AudioSession::probe`]), since no mix
/// is opened to ask. The mix runs at the highest source rate, 48 kHz when every source
/// is below 44.1 kHz ([`crate::audio`]'s `mix_rate`), and as wide as the widest source.
/// The fidelity gate that can still hand the track back to AAC is measured only once
/// the encode has run, so this is the prediction the export's own line corrects.
fn mkv_mix_is_opus(project: &Project, fps: f64) -> bool {
    let sources = project.audio_sources();
    let mut rates = Vec::new();
    let mut channels = 0u16;
    for lane in project.audio_segments_from(0, fps) {
        for (source, ..) in lane {
            let Some((path, stream)) = source.and_then(|i| sources.get(i)) else {
                continue;
            };
            if crate::is_image(path) {
                continue;
            }
            if let Ok(Some(probe)) = AudioSession::probe(path, *stream) {
                rates.push(probe.sample_rate);
                channels = channels.max(probe.channels);
            }
        }
    }
    let rate = rates.into_iter().max().map(|r| if r < 44_100 { OPUS_RATE } else { r });
    rate == Some(OPUS_RATE) && (1..=2).contains(&channels)
}

/// The sound a timeline's sources cannot give: `Some(label)` when every source
/// on the audio lanes is one [`AudioSession::unsupported`] names as undecodable,
/// which is what the export's own open answers with no sound at all ("no audio
/// to write"). `None` when any source decodes, or when a source has no sound
/// this question can name.
fn undecodable_sound(project: &Project) -> Option<&'static str> {
    let reasons: Vec<Option<String>> = project
        .audio_sources()
        .iter()
        .map(|(path, _)| AudioSession::unsupported(path).ok().flatten())
        .collect();
    if reasons.is_empty() || reasons.iter().any(Option::is_none) {
        return None;
    }
    // The reason's first words name the codec: a Matroska file by its codec id
    // ("the A_DTS track ..."), an mp4 by its media type ("the dts audio track
    // ..."). Only DTS is named by name; any other codec this engine cannot decode
    // gets the general words.
    let dts = reasons.iter().flatten().all(|reason| {
        let reason = reason.to_ascii_lowercase();
        reason.starts_with("the a_dts ") || reason.starts_with("the dts ")
    });
    Some(match dts {
        true => "none — DTS can't be decoded",
        false => "none — this codec can't be decoded",
    })
}

/// The tone map a picture export puts on its HDR sources, as a sentence for the
/// start line and the plan: the picture is written 8-bit BT.709 SDR, so a PQ or
/// HLG source is tone-mapped down silently otherwise. `None` where no picture
/// is tone-mapped -- a sound-only format, a preview that keeps its source's
/// curve ([`ExportSettings::keep_source_colour`]), or no HDR source in the
/// project. Asked of every source, as [`planned_seats`] asks its sound.
///
/// Header reads only, one per source, the same colour the export's own tone map
/// is built from ([`source_rate`]).
pub fn planned_tone(project: &Project, meta: &VideoMeta, settings: &ExportSettings) -> Option<String> {
    if !settings.format.has_video() || settings.keep_source_colour {
        return None;
    }
    // The matrix the file is tagged with: the one [`start`] writes for this canvas
    // ([`ColorDescription::output_sized`]), so the words name what the file says.
    let out = output_meta(meta, settings);
    let matrix = match ColorDescription::output_sized(out.width, out.height).matrix {
        Matrix::Bt601 => "BT.601",
        _ => "BT.709",
    };
    let mut transfers = project.sources().iter().filter_map(|source| {
        if crate::is_image(&source.path) || crate::is_audio(&source.path) {
            return None;
        }
        Demuxer::open(&source.path).ok().map(|(meta, _)| meta.color.transfer)
    });
    transfers.find_map(|transfer| match transfer {
        Transfer::Pq => Some(format!("HDR (PQ) source tone-mapped to SDR {matrix}")),
        Transfer::Hlg => Some(format!("HDR (HLG) source tone-mapped to SDR {matrix}")),
        Transfer::Sdr => None,
    })
}

fn settle(shared: &Shared, result: crate::Result<()>) {
    *shared.outcome.lock().unwrap() = Some(result);
    // Published last: a caller that sees the flag is guaranteed the outcome.
    shared.finished.store(true, Ordering::Release);
}

/// The audio track an export writes, and how it was made -- a copy is the
/// source's own packets, bit for bit, and anything else is this project's
/// encoder over the decoded mix. [`run`] publishes which of the two really
/// happened, so the encoder line a user reads is measured and not promised.
pub(crate) struct ExportAudio {
    pub params: crate::AacTrackParams,
    pub packets: Vec<crate::AacPacket>,
    /// Whether those packets are the source's, untouched.
    pub copied: bool,
    /// The track's pre-skip in samples where those packets are **Opus** and not
    /// AAC ([`encode_opus`]), which is what the Matroska track entry states in
    /// its `OpusHead` and its `CodecDelay`. `None` is an AAC track, which is
    /// every copy and every mp4.
    pub opus_pre_skip: Option<u16>,
    /// The language the track is tagged with ([`audio_language`]), `None` for
    /// `und`. Set once where the track is made, from the project it came from.
    pub language: Option<String>,
    /// The sound's length in PCM frames per channel, where the packets were
    /// encoded from a mix of exactly that many: the count the encoder was fed,
    /// not the packets' sum, which runs on past it by the encoder's priming and
    /// the last frame's padding. `None` for a copy, whose length is the
    /// source's own packets.
    pub pcm_frames: Option<u64>,
}

/// The timeline's audio track for a file that carries picture: copied packet for
/// packet where a copy says exactly what the timeline says, and decoded, mixed
/// and encoded again ([`encode_audio`]) where it cannot -- a second audio lane,
/// a speeded clip, an equalizer, a source that is not AAC inside an mp4 sample
/// table. Nothing is refused for being uncopyable any more; the only refusals
/// left below the copy are about sound this engine cannot *decode* either, and
/// those come out of the decode path by name.
///
/// The copy is still the default and still bit-exact: a timeline nobody has
/// touched leaves as the very packets its source holds.
///
/// corner-cut: this holds the whole exported AAC track in memory (~3 kB per
/// 23 ms packet, so ~500 MB for an hour). Upgrade path is a streaming
/// `copy_segments` that yields packets instead of collecting them.
#[allow(clippy::too_many_arguments)]
fn copy_audio(
    project: &Project,
    meta: &VideoMeta,
    kbps: u32,
    mkv: bool,
    webm: bool,
    shared: &Shared,
    sample_rate: Option<u32>,
    range: Option<(u32, u32)>,
) -> crate::Result<Option<ExportAudio>> {
    // WebM's sound is Opus, which no source's packets are: always the mix. A loudness target is a
    // gain over the mix, which a copied packet cannot carry.
    if webm || shared.loudness.is_some() {
        return encode_audio(project, meta, kbps, mkv, webm, shared, sample_rate, range);
    }
    // A ranged export needs an exact offset and an exact length, which a
    // packet copy cannot give -- it carries whole segments named by
    // `audio_segments_from(0, ..)`, not an arbitrary window into them. The
    // decoded path already resamples per segment, so it is the one that can
    // start and stop anywhere; this falls to it exactly as a speeded or
    // equalized lane already does.
    if range.is_some() {
        return encode_audio(project, meta, kbps, mkv, webm, shared, sample_rate, range);
    }
    // The segments name their source, and the copy carries its packet-rounding
    // debt across a source join exactly as across a cut, so a timeline spanning
    // files stays in sync. A source whose AAC parameters disagree with the
    // first one is an `Err` from there -- import refuses those up front, this
    // is the backstop -- and the caller deletes the `.part`.
    //
    // The stream each source is copied from is the one it plays: `audio_sources`
    // is the same list `PlaybackSession::seek` hands the decoder, so an export
    // of a timeline playing a file's second audio track carries *that* track.
    //
    // One lane is the only shape a *copy* has: summing two means decoding both,
    // and a sum is not a copy. Two or more go to [`encode_audio`], which is the
    // very mix the WAV path writes -- it used to be a refusal, and a refusal is
    // what a file missing half its sound deserves, not what a mix does.
    //
    // *Which* lane is the same question `audio_segments_from` answers for
    // playback, and it is asked here rather than assumed: the lane that holds
    // the sound need not be `A1` (a project may leave `A1` empty and place
    // everything on `A2`), and copying `A1`'s list in that case would write a
    // file with no audio track at all -- silently, which is the failure this
    // whole comment is about.
    let mut lanes = project.audio_segments_from(0, meta.frame_rate);
    // ...cut to what the picture writes ([`export_spans`]). The play list is
    // the *timeline's* ([`Project::timeline_frames`]), so a caption hanging past
    // the last picture ends every audio lane with a trailing gap -- silence this
    // path would copy out past the last frame, three seconds of it on the film
    // this was found on. Only a gap can be out there (`media_frames` is the last
    // clip's own end), and the mixed paths already resize to the same length
    // ([`encode_audio`]); a copy carries source packets and has no resize.
    let over = f64::from(project.timeline_frames() - project.media_frames()) / meta.frame_rate;
    for lane in &mut lanes {
        match lane.last_mut() {
            Some((None, _, len)) if *len > over => *len -= over,
            Some((None, _, _)) if over > 0.0 => {
                lane.pop();
            }
            _ => {}
        }
    }
    let [segments] = &lanes[..] else {
        return encode_audio(project, meta, kbps, mkv, webm, shared, sample_rate, None);
    };
    // ...and the same list names *which* lane those clips sit on, which is the
    // only way to ask what has been done to them.
    let lane = project.audio_lanes()[0];
    // ...and the same list decides whether any clip this would copy plays at a
    // rate other than the one it was recorded at. A copy hands the muxer the
    // very AAC packets the source holds and there is no rate inside a packet to
    // change: the picture would come out re-timed (the walk in [`run`] honours
    // it) over sound at 1.00x, which is the drift a person finds only after the
    // file has gone somewhere. Decoded and resampled instead, by the same worker
    // playback feeds from.
    //
    // ...and whether any clip on it carries an equalizer, which is sample math a
    // copy never reaches: copying such a lane would write the clip *flat*,
    // silently.
    //
    // A frame *rate* of its own is not one of those, and that is deliberate: do
    // not "fix" this into a re-encode. A clip counts the **timeline's** frames
    // whatever its file was shot at ([`crate::project::Rate`]), so the window
    // this copy trims a source's sound to is `start..end` in whole timeline
    // frames -- exactly the seconds the picture covers -- and the file is read
    // at the pitch it was recorded at, which is what playing at the rate it was
    // shot at means. There is no per-clip stretch to express, so the packets are
    // already the right sound in the right place; re-encoding a conformed lane
    // would buy nothing and cost a generation of loss on every mixed-rate export
    // (`tests/mixed_fps.rs`, `an_export_of_two_rates_runs_at_the_speed_it_was_shot`
    // measures the copied sound against the picture). A *speed* is different
    // because it resamples, which is why it leaves through the re-encode above.
    //
    // Only these: a timeline nobody has touched still leaves through the copy
    // below, packet for packet, so no passthrough quietly becomes a generation
    // of loss.
    // ...and whether the mix itself does anything a packet never went through:
    // a lane volume is applied at the sum and a limiter over it, so copying
    // such a lane would write it at unity and unlimited, silently.
    // ...asked as the very predicate [`forces_encode`] is, over the one lane
    // the segments came off, so what a card predicted and what this decides are
    // one answer. `lane` here is that lane, and not `A1` by assumption:
    // [`Project::audio_lanes`] is the list built for, and filtered by, these
    // very segments.
    if sample_altered(project, &[lane]) {
        return encode_audio(project, meta, kbps, mkv, webm, shared, sample_rate, None);
    }
    // What is left is a copy the *sources* may still not be able to give: AAC
    // inside a Matroska file (readable, but not out of a sample table this walks
    // -- there is none), an mp3 or a wav on the timeline, an AC-3 track, two
    // sources whose AAC parameters disagree. Every one of them decodes, and what
    // decodes can be encoded again, so the copy's refusal is now a route: the
    // error a *decode* cannot get past -- Opus or AC-3 inside a Matroska file,
    // which symphonia has no decoder for at any version -- is the one that comes
    // back, by name, from [`encode_audio`].
    match AudioSession::copy_multi_streams(&project.audio_sources(), segments) {
        Ok(copied) => Ok(copied.map(|(params, packets)| ExportAudio {
            params,
            packets,
            copied: true,
            // A copy is the source's own AAC packets: there is no Opus copy
            // path here, since the packets an mkv source holds never reach this
            // (they are not in a sample table) and every one of them re-encodes.
            opus_pre_skip: None,
            language: None,
            pcm_frames: None,
        })),
        Err(_) => encode_audio(project, meta, kbps, mkv, webm, shared, sample_rate, None),
    }
}

/// The picture's half of [`copy_audio`]: a Matroska export whose every span is
/// its source's own coded blocks, written back out without a decoder or an
/// encoder anywhere in the path.
///
/// **When.** A copy is entered only where it says *exactly* what the timeline
/// says, which is the audio gate's rule and not a looser one: the file this
/// export writes is Matroska, the source's codec is the one that file would
/// carry ([`Format::Hevc`] over HEVC, [`Format::Av1`] over AV1), and nothing on
/// the way has touched a sample -- no grade, no speed, no rate conversion, no
/// gap to fill with black, no source of another size to place on the canvas,
/// no `ContentEncodings` packing to undo. Anything else re-encodes exactly as
/// it always did; there is no half-copied file.
///
/// **All or nothing, per export.** A copied span and an encoded one cannot
/// share one track: this project's encoders write 8-bit SDR streams whose
/// parameter sets are their own, so splicing them into (say) a Main 10 HDR
/// source would change bit depth and colour mid-track, and a container states
/// its configuration record once. So the head and tail fragments of a cut that
/// lands mid-GOP are *not* re-encoded into the copy -- they are what makes the
/// whole export fall back to the encoder. What a copy needs instead is that
/// every span begins on a sync point and ends where the next one begins, which
/// is what a cut placed on a keyframe gives it.
///
/// **What a cut costs.** An open-GOP source (x265's default) writes leading
/// pictures after a sync point that are shown *before* it and reference the
/// group before it. At the start of a copied region those references are not in
/// the file, so they are dropped -- the picture before the cut is held for those
/// few frames (four at this film's B-depth, a sixth of a second) rather than
/// being written as pictures that cannot decode. The region still starts at the
/// timeline frame it owns, so the sound never drifts.
///
/// corner-cut: mp4 is not copied (its `hvc1` sample entry forbids in-band
/// parameter sets, and the mp4 muxer here writes its sample table from a frame
/// counter), and neither is H.264, which this project only writes into mp4. The
/// upgrade path for both is a sample-table writer that takes explicit
/// timestamps, which is what [`MkvMuxer::write_block`] already is for Matroska.
struct CopyPlan {
    /// Every source a region reads, by the index the project names it with;
    /// `None` for a source no clip plays from.
    sources: Vec<Option<MkvDemuxer>>,
    regions: Vec<CopyRegion>,
    codec_id: &'static [u8],
    /// The `CodecPrivate` every region's source agrees on -- a track is declared
    /// once, so two sources that disagree about their parameter sets are not one
    /// copy.
    private: Vec<u8>,
    /// The picture's language as the first source states it ([`video_language`]), which the
    /// copied track declares.
    language: Option<String>,
    /// The container's colour tags exactly as the source wrote them, which the
    /// copied track declares: a field the source left out stays left out, and
    /// nothing is resolved in its place ([`MkvDemuxer::colour_tags`]).
    colour: Tags,
    /// The coded shape the copied track declares: the samples under it are the
    /// source's own, so the header names what they are stored at -- not the
    /// shape they are drawn at.
    width: u32,
    height: u32,
    /// The aspect the copied samples are drawn at, written into the file as
    /// display metadata ([`CopyParams::pixel_aspect`]): square for every
    /// ordinary source, so the header gains nothing.
    pixel_aspect: crate::demux::PixelAspect,
}

/// A run of source blocks copied in one piece: what a cut leaves behind, and
/// what two spans that continue each other in the same file merge back into.
struct CopyRegion {
    source: usize,
    /// Blocks of that source, in the file's own decode order.
    blocks: std::ops::Range<usize>,
    /// The *output* frame the region's first picture is shown at -- the
    /// project's own timeline frame less the range's own start, since a
    /// ranged export's file starts a fresh clock at zero.
    start: u32,
}

/// Why [`ExportSettings::exact`] cannot be honoured for this project right
/// now, or `None` where [`CopyPlan`] already carries the whole file. A state
/// report, not an instruction: it says what the file *is* rather than what to
/// pick instead.
///
/// Two shapes of reason, told apart because only one of them is permanent.
/// **The container cannot carry the source's own codec at all** -- Exact was
/// asked of an mp4, or of a Matroska file whose codec is not the source's own
/// -- which is [`CopyPlan`]'s own corner-cut (mp4 copies nothing yet) and
/// [`CopyPlan::of`]'s codec check, so it is checked here the same way rather
/// than duplicated with different words. **The edit is what stops it** -- a
/// cut off a sync point, a grade, a speed, a dissolve, a moved or scaled
/// clip -- which the same format would carry exactly if the edit let it, so
/// those six [`CopyPlan::of`] gates are named together rather than picked
/// apart into six different sentences.
pub fn exact_refusal(
    project: &Project,
    meta: &VideoMeta,
    settings: &ExportSettings,
) -> Option<String> {
    if !settings.exact || CopyPlan::of(project, meta, settings).is_some() {
        return None;
    }
    // An anamorphic source asked of a format that cannot state the aspect: the
    // blocks would copy, but the sample entry this writer produces has no
    // `pasp` box to carry it in, and a header that declares the coded pair
    // while the pixels are drawn 4/3 wide is a file that lies about its own
    // shape. Refused by name -- the sentence a reader is owed -- before the
    // container's codec question, because this is the *nearer* reason: a
    // Matroska copy needs no refusal at all (its track carries the aspect
    // beside the blocks, [`CopyParams::pixel_aspect`], which is ffmpeg's own
    // copy behaviour), so an anamorphic source's Exact row only ever dies on
    // this for an mp4.
    if settings.burn_subtitles.is_some() {
        return Some(
            "captions are drawn into the picture, so it is decoded and encoded again rather \
             than copied byte for byte"
                .to_string(),
        );
    }
    if !project.titles().is_empty() {
        return Some(
            "titles are drawn into the picture, so it is decoded and encoded again rather \
             than copied byte for byte"
                .to_string(),
        );
    }
    if picture_faded(project) {
        return Some(
            "a picture fade is pixel math, so it is decoded and encoded again rather than \
             copied byte for byte"
                .to_string(),
        );
    }
    if !settings.format.is_mkv() {
        let anamorphic = project.sources().iter().any(|source| {
            Demuxer::open(&source.path)
                .ok()
                .is_some_and(|(source_meta, _)| !source_meta.pixel_aspect.is_square())
        });
        if anamorphic {
            return Some(
                "a copy would lose the pixel aspect — this format cannot state it beside the \
                 coded picture, so the export re-encodes it into the square pixels it is drawn at"
                    .to_string(),
            );
        }
    }
    let codec = match settings.format {
        Format::Hevc => Codec::Hevc,
        Format::Av1 => Codec::Av1,
        _ => {
            return Some(format!(
                "{} cannot carry a source's own coded picture byte for byte — pick an HEVC or AV1 Matroska to copy it exactly",
                settings.format.name()
            ));
        }
    };
    // The turn-baking corner-cut (a copied packet is the source's own coded
    // picture; this export bakes a display-matrix turn into the pixels) and
    // the rest of the reasons a copy says something other than what the
    // timeline says are [`CopyPlan::of`]'s own gates, so the container and
    // edit sentences below are checked the same way rather than duplicated
    // with different words.
    let carries = project.sources().iter().all(|source| {
        crate::demux::is_matroska(&source.path)
            && Demuxer::open(&source.path)
                .ok()
                .is_some_and(|(source_meta, _)| source_meta.codec == codec)
    });
    Some(match carries {
        false => format!(
            "{} cannot carry the source's own codec byte for byte",
            settings.format.name()
        ),
        true => "the edit does not let this file copy the source exactly — a cut off a sync \
                 point, a grade, a speed, a transform or a canvas the source is not forces a \
                 re-encode"
            .to_string(),
    })
}

impl CopyPlan {
    /// The plan for this timeline, or `None` where anything at all makes a copy
    /// say something other than what the timeline says -- in which case the
    /// caller encodes, which is what it always did.
    fn of(project: &Project, meta: &VideoMeta, settings: &ExportSettings) -> Option<Self> {
        // Burned-in captions and titles are drawn into pixels, which a copy
        // never has.
        if settings.burn_subtitles.is_some()
            || !project.titles().is_empty()
            || picture_faded(project)
        {
            return None;
        }
        // An explicit picture depth is a request for samples the source may not
        // have (8 bits from a 10-bit HEVC, say), which a copy would silently
        // keep: the picture is re-encoded at the depth asked for.
        if settings.video_bit_depth.is_some() {
            return None;
        }
        // An HDR transfer is a statement the encoder writes into its own stream
        // (VUI, SEIs); a copy carries the source's.
        if settings.hdr.transfer != HdrTransfer::Sdr {
            return None;
        }
        let (codec_id, codec): (&'static [u8], Codec) = match settings.format {
            Format::Hevc => (b"V_MPEGH/ISO/HEVC", Codec::Hevc),
            Format::Av1 => (b"V_AV1", Codec::Av1),
            _ => return None,
        };
        let entries = project.sources();
        let mut sources: Vec<Option<MkvDemuxer>> = (0..entries.len()).map(|_| None).collect();
        // The DRAWN pair each source is displayed at, kept beside its demuxer:
        // the fit gate below asks what a clip's policy does with this source's
        // picture on this canvas, and the picture the encoder places is the
        // drawn one ([`VideoMeta::width`]/`height`] -- the coded one with the
        // file's turn and its pixel aspect already applied), not the pair the
        // samples are stored at. A non-square source at its own displayed size
        // is therefore the identity again, which is what a copy of one is: the
        // aspect travels beside the track in the `Display*` elements rather
        // than being resampled into it.
        let mut drawn: Vec<Option<(u32, u32)>> = (0..entries.len()).map(|_| None).collect();
        let mut declared: Option<(Vec<u8>, Tags, crate::demux::PixelAspect)> = None;
        let mut regions: Vec<CopyRegion> = Vec::new();
        let r_start = settings.range.map_or(0, |(s, _)| s);
        for span in export_spans(project, settings.range) {
            let out_start = span.start - r_start;
            // A gap is black frames, which only an encoder makes.
            let (source, in_frame) = span.from?;
            if !span.speed.is_normal() || span.ramp.is_some() || span.reverse {
                return None;
            }
            // A layered picture is composed from more than one clip; no packet
            // of one of them is the picture.
            if layered_at(project, span.start) {
                return None;
            }
            // A copied packet is the source's own picture, never a blend: a
            // live dissolve on this span's tail forces the whole video track
            // through the encoder, exactly as a grade or a speed does below.
            //
            // corner-cut: the whole file falls back rather than only the
            // clips either side of the dissolve -- [`CopyPlan`] is already
            // all-or-nothing per export ([`CopyPlan`]'s own doc comment
            // states why), so one dissolve on a two-hour film re-encodes all
            // of it. Upgrade path: splice a re-encoded region between two
            // copied ones, which needs the sample-table writer that doc
            // comment already names as the cost of a finer copy.
            if dissolve_window(project, &span).is_some() {
                return None;
            }
            // The grade playback shows, which a copied packet never went
            // through. `None` and the identity are the same untouched picture.
            if project
                .composite_color_at(span.start)
                .is_some_and(|params| !params.is_identity())
            {
                return None;
            }
            // A moved, scaled, rotated or cropped clip is a different picture
            // than the source's own packets, exactly as a grade is.
            if project
                .composite_transform_at(span.start)
                .is_some_and(|params| !params.is_identity())
            {
                return None;
            }
            let path = &entries.get(source)?.path;
            if sources.get(source)?.is_none() {
                if !crate::demux::is_matroska(path) {
                    return None;
                }
                let (source_meta, demuxer) = Demuxer::open(path).ok()?;
                let Demuxer::Mkv(demuxer) = demuxer else {
                    return None;
                };
                // The stream this file would carry, at the size and the rate the
                // timeline is in: anything else is a picture that has to be
                // coded again to become this file's.
                //
                // A source whose display matrix asks for a turn is one of
                // those: a copied packet is the source's own coded picture, and
                // this export bakes the turn into the pixels -- so a copy would
                // ship the file sideways. Re-encoding is the only honest
                // answer here.
                //
                // The size the copy declares is the source's *coded* pair, so
                // that is what the comparison asks: coded to coded. A display
                // comparison would refuse every anamorphic source that happens
                // to sit at the project's displayed shape and then write a
                // track that lies about its own samples -- and a square
                // source's coded pair is its display pair, so this asks nothing
                // new of the ordinary file.
                //
                // A source whose samples are not square is the *other* kind of
                // picture a copy cannot carry here: the blocks are its own
                // coded pixels and this engine's Matroska writer can now state
                // the aspect beside them -- but only this engine's
                // ([`MkvMuxer::create_copy`] writes the `Display*` elements),
                // and the copy's declared shape must stay the coded one. The
                // aspect travels with the track instead of being baked in,
                // which is ffmpeg's own copy behaviour, so an anamorphic
                // source is copyable *into Matroska* and nowhere else.
                if !source_meta.rotation.is_none()
                    || source_meta.codec != codec
                    || source_meta.coded_width != meta.coded_width
                    || source_meta.coded_height != meta.coded_height
                    || Rate::from_fps(source_meta.frame_rate, meta.frame_rate).ok()?
                        != Rate::REAL_TIME
                    || !demuxer.plain_blocks()
                {
                    return None;
                }
                // Two sources that disagree about their pixel aspect are two
                // displayed pictures under one track; the copy declares one
                // aspect or none.
                let declares = (
                    demuxer.codec_private().to_vec(),
                    demuxer.colour_tags(),
                    source_meta.pixel_aspect,
                );
                if declared.get_or_insert(declares.clone()) != &declares {
                    return None;
                }
                sources[source] = Some(*demuxer);
                drawn[source] = Some((source_meta.width, source_meta.height));
            }
            let demuxer = sources.get_mut(source)?.as_mut()?;
            // **The canvas gate** -- the condition this path was missing, and
            // the one that decides whether a copy can state the project's size.
            //
            // A copied packet is the source's own coded picture; the file this
            // export writes has to be the size the *project* is, and those are
            // two different pairs held in two different fields:
            //
            //   * `meta.coded_width`/`coded_height` is the pair a *copied* track
            //     declares -- what the samples are stored at, and what the check
            //     above compares every source against. Only a *source* ever moves
            //     it.
            //   * `meta.width`/`meta.height` is the canvas -- what a person chose
            //     with [`crate::playback::PlaybackSession::set_resolution`],
            //     which writes those two and leaves the coded pair exactly where
            //     it was. The encode door sizes everything from it
            //     ([`Composer`]), so this is the size the file must have.
            //
            // ...and the two meet in the *drawn* pair: what this span is
            // composed from is the source's picture as a decoder delivers it,
            // turn and pixel aspect already applied
            // ([`VideoMeta::width`]/`height`]), which is what the check below is
            // asked about.
            //
            // A canvas that differs from the source's drawn pair means every
            // span is a picture the export has to *place onto* that canvas, and
            // there is no resampler on this path at all -- so a copy would ship
            // the source's frame size under the project's name, with the seat
            // line reading "copy · …" all the way out. That is the defect: not
            // a wrong rate or a wrong codec but the wrong geometry, silently,
            // on the one door that is supposed to be exact.
            //
            // Asked as a *placement* rather than as two separate conditions --
            // is this clip's policy the identity for this source on this canvas
            // -- because that is the one thing a condition has to mean here:
            // [`fit_is_identity`] computes it from [`scale::fit_rect`] itself,
            // so it holds for `Fill` on a canvas the size of the picture the
            // source draws at (there is nothing to crop there) and refuses a
            // canvas of another size under *every* policy, and a
            // [`crate::scale::FitPolicy`] added later is judged by what it
            // places rather than by being absent from a list here.
            if !fit_is_identity(
                drawn.get(source).copied().flatten()?,
                (meta.width, meta.height),
                project.composite_fit_at(span.start),
            ) {
                return None;
            }
            // The window this span reads, in the blocks the whole engine counts
            // a source's frames in. It has to *be* whole groups of pictures: a
            // copy that began between two sync points would hand a decoder
            // pictures whose references are not in the file, and one that ended
            // between them would drop a reference the pictures before it need.
            let (start, end) = (in_frame as usize, in_frame as usize + span.len as usize);
            if end > demuxer.block_count()
                || !demuxer.is_sync(start)
                || (end < demuxer.block_count() && !demuxer.is_sync(end))
            {
                return None;
            }
            // Two spans that continue each other in one file are one region:
            // nothing was cut between them, so the leading pictures across that
            // boundary still have every reference they need.
            match regions.last_mut() {
                Some(last)
                    if last.source == source
                        && last.blocks.end == start
                        && last.start + (last.blocks.end - last.blocks.start) as u32
                            == out_start =>
                {
                    last.blocks.end = end;
                }
                _ => regions.push(CopyRegion {
                    source,
                    blocks: start..end,
                    start: out_start,
                }),
            }
        }
        let (private, colour, pixel_aspect) = declared?;
        Some(Self {
            sources,
            regions,
            codec_id,
            private,
            language: video_language(project),
            colour,
            // The track the copy declares is the source's coded shape -- the
            // samples under it are the source's own -- with the aspect beside
            // it as the display metadata ([`CopyParams::pixel_aspect`]).
            width: meta.coded_width,
            height: meta.coded_height,
            pixel_aspect,
        })
    }

    /// The file itself: one Matroska muxer, every region's blocks through it in
    /// the order and at the timing their source states.
    #[allow(
        clippy::too_many_arguments,
        reason = "the copy's one run: its inputs are passed positionally at its one call site, and a struct would only rename them"
    )]
    fn run(
        mut self,
        out: &Path,
        meta: &VideoMeta,
        shared: &Shared,
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
        subs: Vec<SubParams>,
        total: u32,
        chapters: &[(u64, String)],
    ) -> crate::Result<()> {
        let mut muxer = MkvMuxer::create_copy(
            out,
            &CopyParams {
                width: self.width,
                height: self.height,
                frame_rate: meta.frame_rate,
                codec_id: self.codec_id,
                codec_private: &self.private,
                colour: self.colour,
                pixel_aspect: self.pixel_aspect,
                language: self.language.clone(),
            },
            audio,
            subs,
        )?;
        muxer.set_chapters(chapters)?;
        // Nanoseconds a timeline frame lasts, which is what a region's own
        // position on the timeline is measured in.
        let frame_ns = 1e9 / meta.frame_rate;
        let mut done = 0u32;
        for region in &self.regions {
            let demuxer = self.sources[region.source]
                .as_mut()
                .ok_or("a copied region names a source that was never opened")?;
            let base = (f64::from(region.start) * frame_ns).round() as i64;
            // When the region's first picture is shown on its source's clock;
            // everything after it is written that far along from `base`.
            let mut origin: Option<i64> = None;
            for index in region.blocks.clone() {
                cancelled(shared)?;
                let Some(block) = demuxer.coded_block(index)? else {
                    break;
                };
                let origin = *origin.get_or_insert(block.ts_ns);
                // The leading pictures of the group this region opens on: shown
                // before it, coded against the group before it, and that group
                // is not in this file. Dropped rather than written as pictures
                // no decoder can put back together.
                if block.ts_ns < origin {
                    continue;
                }
                muxer.write_block(block.bytes, block.key, base + block.ts_ns - origin)?;
                done += 1;
                shared
                    .progress
                    .fetch_max(picture_progress(done, total), Ordering::Relaxed);
            }
        }
        // Everything up to here is still cancellable, exactly as the encoded
        // path is up to its own `finish`.
        cancelled(shared)?;
        muxer.finish()?;
        shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed);
        Ok(())
    }
}

/// Whether `policy` places the picture `source` is DRAWN at onto this `canvas`
/// without touching a sample: the whole picture, at the whole canvas, 1:1.
///
/// The 1:1 is the half the rects cannot state: a source scaled onto a canvas
/// of another aspect is placed whole and cropped nowhere, which is a resize
/// and not an identity -- so the pairs themselves are compared first.
///
/// The pair asked about is the **drawn** one ([`VideoMeta::width`]/`height`]),
/// never the coded one: what the fit is composed against is what a decoder
/// hands the canvas after the file's turn and its pixel aspect have been
/// applied, so a 1440x1080 file with a 4:3 sample aspect is a 1920x1080
/// picture, and a project at 1920x1080 is copying it exactly. The coded pair
/// has its own question -- what a copied track *declares* -- and the coded-to-
/// coded check in [`CopyPlan::of`] already asks that one; asking it twice, in
/// the geometry's stead, refuses a copy main performs byte for byte.
///
/// Asked from [`scale::fit_rect`] -- the one function that *is* a fit policy's
/// meaning -- rather than from the policy itself, which is what makes this
/// canvas-aware twice over: `Fill` on a canvas the size of the source's own
/// samples is the identity (nothing is cropped, because there is nothing to
/// crop), while `Fit` on a canvas of another aspect is not (it bars or crops).
/// A policy added to [`FitPolicy`] later is judged by what it places, not by
/// being absent from a list here.
fn fit_is_identity(
    source: (u32, u32),
    canvas: (u32, u32),
    policy: crate::scale::FitPolicy,
) -> bool {
    // The pairs themselves: a picture is the canvas only when it *is* the
    // canvas, and the rects below cannot say so -- a source scaled onto a
    // canvas of another aspect is still placed whole and still cropped
    // nowhere, which is a resize, and a resize is exactly what a copied packet
    // cannot be.
    if source != canvas {
        return false;
    }
    let (placed, cropped) = crate::scale::fit_rect(source.0, source.1, canvas.0, canvas.1, policy);
    placed
        == crate::scale::Rect {
            x: 0,
            y: 0,
            w: canvas.0,
            h: canvas.1,
        }
        && cropped
            == crate::scale::Rect {
                x: 0,
                y: 0,
                w: source.0,
                h: source.1,
            }
}

/// The same lane, *decoded*: what a packet copy cannot carry comes out here as
/// AAC written by this project's own encoder.
///
/// The samples are the very ones [`run_audio`] would put in a WAV -- the same
/// opener with the same equalizers, the same rates and the same resize onto the
/// timeline's own length -- so an mp4 and a WAV of one timeline are one mix,
/// not two that could disagree.
///
/// AAC-LC's encoder delay is one 1024-frame block, which is exactly the priming
/// a reader drops when the file carries no edit list (`audio::DEFAULT_PRIMING`,
/// and [`crate::mux::Mp4Muxer`] writes none): the first packet is that delay, so
/// the sound starts at timeline frame 0 as a copied track's does.
///
/// corner-cut: the mix sits in memory as f32 before it is encoded (~46 MB a
/// minute of 48 kHz stereo), for [`run_audio`]'s reason -- sizing the track
/// against the timeline, publishing the mix third of the bar as it goes and
/// answering a cancel before the encode are whole-mix concerns here. The
/// upgrade path is a chunked push: `ec_aac`'s encoder itself streams.
#[allow(clippy::too_many_arguments)]
fn encode_audio(
    project: &Project,
    meta: &VideoMeta,
    kbps: u32,
    mkv: bool,
    webm: bool,
    shared: &Shared,
    sample_rate: Option<u32>,
    range: Option<(u32, u32)>,
) -> crate::Result<Option<ExportAudio>> {
    let sources = project.audio_sources();
    let range_start = range.map_or(0, |(s, _)| s);
    let pre = warm_up(project, range_start, meta.frame_rate);
    let offset = range_start - pre;
    let segs = project.audio_segments_from(offset, meta.frame_rate);
    let eqs = project.audio_eqs_from(offset, meta.frame_rate);
    let speeds = project.audio_speeds_from(offset, meta.frame_rate);
    let fades = project.audio_fades_from(offset, meta.frame_rate);
    // Coarse stage timers, in the same voice as `export video: copy` below: an
    // export of a feature film spends minutes in here before a byte of picture
    // is written, and which minutes went where is the first question asked of a
    // slow export. Three lines an export, so they stay on.
    let mixed = std::time::Instant::now();
    let Some((audio, chunks)) = AudioSession::open_mixed_streams_master_at_fade(
        &sources,
        &segs,
        &eqs,
        &speeds,
        &fades,
        meta.frame_rate,
        &project.audio_gains(),
        &project.audio_ducks(),
        &project.audio_voices(),
        project.limiter(),
        sample_rate,
    )?
    else {
        return Ok(None); // no audio to write, exactly as a copy of nothing is
    };
    let freq_index = ec_aac::sf_index_for_rate(audio.sample_rate).ok_or_else(|| {
        format!(
            "{} Hz is not an AAC sample rate: export WAV, FLAC, MP3 or OGG, which write it as \
             it is",
            audio.sample_rate
        )
    })?;
    let channels = usize::from(audio.channels.max(1));
    // The timeline's length, not the mix's: a segment resolves its window to
    // whole samples on its own, so the sum can miss by a sample or two and a
    // source that ran out early would leave the track short under a picture
    // that is not. One resize settles both, as [`run_audio`]'s does.
    let out_frames = range.map_or(project.media_frames(), |(s, e)| {
        e.min(project.media_frames()).saturating_sub(s)
    });
    let total = (f64::from(out_frames) / meta.frame_rate * f64::from(audio.sample_rate)).round()
        as usize
        * channels;
    let skip = warm_up_samples(range_start, pre, meta.frame_rate, audio.sample_rate, channels);
    let mut samples: Vec<f32> = Vec::with_capacity(total + skip);
    for chunk in chunks {
        // The only place a cancelled export can be answered before the whole
        // timeline has been mixed. This pass runs on a thread of its own now
        // ([`run`]), so without this a cancel left it mixing a feature film
        // for minutes behind a job nobody is waiting for any more, writing
        // into a `Shared` whose outcome was already settled.
        cancelled(shared)?;
        samples.extend(chunk.samples);
        // The bar's first third of [`AUDIO_BAND`]: the mix is the longest of
        // the three stages and the only one that can be counted as it goes.
        //
        // `fetch_max` and not `store`, for the reason [`Shared::progress`]
        // states: this runs on the audio thread while the picture publishes
        // numbers above [`AUDIO_BAND`] from the other, and a `store` of this
        // small one pulled the bar backwards 5,458 times over one film export.
        shared.progress.fetch_max(
            (samples.len().saturating_sub(skip).min(total) * (AUDIO_BAND / 3) as usize / total.max(1)) as u32,
            Ordering::Relaxed,
        );
        // The range is in: the mix past its end would only be cut off below.
        if samples.len() >= skip + total {
            break;
        }
    }
    samples.drain(..skip.min(samples.len()));
    samples.resize(total, 0.0);
    if let Some(target) = shared.loudness {
        level_to_target(&mut samples, audio.sample_rate, audio.channels.max(1), target);
    }
    if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!(
        "export audio: mixed {:.1} s of sound in {:.1} s",
        total as f64 / f64::from(audio.sample_rate.max(1)) / channels as f64,
        mixed.elapsed().as_secs_f64()
    ); }

    // Opus where the container carries it and the mix is inside the envelope
    // this encoder is *measured* correct in ([`encode_opus`]): a Matroska file,
    // 48 kHz, mono or stereo. An Opus source edited into an mkv export
    // therefore leaves as Opus instead of being turned into AAC on the way
    // out, which is the one generation of loss an all-Opus timeline used to
    // pay for being cut.
    //
    // Everything else falls through to the AAC below and is unchanged by this:
    // every mp4, a 44.1 kHz timeline. The rate is the mix's own -- there is no
    // resampler on this path, and inventing one to reach 48 kHz would resample
    // a whole timeline to satisfy a codec, which is a bigger change to the
    // sound than the codec is.
    // ...and again before either encoder is entered: both of them run to the
    // end of the mix without a checkpoint of their own, so this is the last
    // moment a cancel costs seconds rather than the whole encode.
    cancelled(shared)?;
    if mkv
        && audio.sample_rate == OPUS_RATE
        && (1..=2).contains(&channels)
        && let Some((packets, pre_skip)) = encode_opus(&samples, kbps, channels)?
    {
        return Ok(Some(ExportAudio {
            params: crate::AacTrackParams {
                freq_index,
                chan_conf: channels as u8,
                sample_rate: OPUS_RATE,
            },
            packets,
            copied: false,
            opus_pre_skip: Some(pre_skip),
            language: None,
            pcm_frames: Some(samples.len() as u64 / channels as u64),
        }));
    }

    // A WebM file carries Opus or nothing: a track the gate handed back is named
    // here rather than written as AAC into a file no WebM reader will open.
    if webm {
        return Err(
            "the sound could not be coded as Opus, the only sound a WebM file \
                    carries: export a Matroska instead"
                .into(),
        );
    }
    // The caller's rate, never the encoder's own 128 default -- see
    // [`DEFAULT_AUDIO_KBPS`] for why the untouched figure is 256.
    let mut encoder = ec_aac::AacEncoder::new(ec_aac::AacEncoderConfig {
        bitrate_bps: kbps * 1_000,
        ..Default::default()
    });
    encoder.push_pcm(&samples, audio.channels.max(1), audio.sample_rate)?;
    encoder.finish();
    let mut packets = Vec::new();
    // `next_packet` after `finish` yields packets until `Eof` and nothing else,
    // so the loop ends on the only error it can see.
    while let Ok(packet) = encoder.next_packet() {
        cancelled(shared)?;
        packets.push(crate::AacPacket {
            bytes: packet.data,
            samples: packet.duration,
        });
    }
    Ok(Some(ExportAudio {
        params: crate::AacTrackParams {
            freq_index,
            // 1 and 2 are the ISO channel configurations for mono and stereo,
            // and what the opener hands the timeline is one of those whatever
            // the source carried ([`crate::audio::downmix`]).
            chan_conf: audio.channels.max(1) as u8,
            sample_rate: audio.sample_rate,
        },
        packets,
        copied: false,
        opus_pre_skip: None,
        language: None,
        pcm_frames: Some(samples.len() as u64 / channels as u64),
    }))
}

/// The one rate Opus codes at. The container may state another one in its
/// `OpusHead` and every decoder still runs at 48 kHz (RFC 7845 §5.1), so a
/// timeline at any other rate is not "encoded at its own rate" here -- it is
/// resampled or it is not Opus, and [`encode_audio`] picks the second.
pub(crate) const OPUS_RATE: u32 = 48_000;

/// 20 ms at [`OPUS_RATE`], the frame every Opus muxer in the wild writes:
/// long enough that the per-packet overhead is nothing, short enough that a
/// Matroska block timestamp in whole milliseconds is exact.
const OPUS_FRAME: usize = 960;

/// The most this project asks of the Opus encoder, in kbps, and why it is a cap
/// rather than the four rates the Sound row offers.
///
/// History, because the number moved once already and the reason it could is
/// part of the record. Under `opus-rs 0.1.26` the ceiling was 128: that encoder
/// fell off a cliff at a rate that depended on the *content*, and above it a
/// conformant decoder got noise back. Measured on three one-second spans of a
/// real 5.1 Opus film, encoded there and decoded with `ruopus` -- which was not
/// the suspect, since it decoded ffmpeg's own libopus at 256 kbps to
/// correlation 1.00000 on the same material:
///
/// ```text
/// kbps     120     128     136     144     152     160
/// span A  0.945   0.937   0.452   0.206   0.223   0.163
/// span B  0.986   0.989   0.697   0.153   0.114   0.149
/// span C  0.999   0.994   0.951   0.335   0.266   0.326
/// libopus 0.997   0.998     --      --      --    0.996   (same spans, control)
/// ```
///
/// So: usable to 128, ruined by 136 on real film sound -- and mono worse, at
/// every rate, which is why a mono mix kept the AAC path entirely. `ec-opus`
/// has no such cliff: its own suite holds every rate of the format's table to
/// the reference decoders, mono, stereo and 5.1 alike, and this project
/// re-measures the high band and the mono path where the old encoder failed
/// (`the_opus_encoder_is_pinned_to_the_band_it_was_measured_in`).
///
/// It is still not *trusted*: every track is decoded again and measured before
/// it is written ([`opus_fidelity`]), because a rate nobody measured on *your*
/// sound is not something a constant can be safe against on its own.
/// The ceiling is the Sound row's ([`AUDIO_KBPS`]), not the format's 510 kbps:
/// a rate nothing in the product can ask for is a rate nothing here measures.
/// 320 covers the row honestly, including the 256 default
/// ([`DEFAULT_AUDIO_KBPS`]) the old cap silently halved on a Matroska export.
pub(crate) const OPUS_MAX_KBPS: u32 = 320;

/// How well a track has to survive its own round trip to be written as Opus:
/// the correlation between the mix and the decode of what was encoded from it.
/// A good encoder scores 0.98-0.999 on this material (libopus, measured above);
/// the failure mode is not subtle at all, scoring 0.1-0.7. 0.95 sits in the gap
/// with room on both sides, and what fails it is written as AAC -- the export's
/// own encoder line then says AAC, so the fallback is visible rather than
/// silent.
const OPUS_MIN_FIDELITY: f64 = 0.95;

/// The encoder's lookahead at [`OPUS_RATE`] in the mode this envelope always
/// selects (CELT-only: 2.5 ms). The pre-skip a file states is this *plus* the
/// warm-up frame [`encode_opus`] throws away. Measured by
/// cross-correlating the decode against the source, not taken from the crate --
/// it exposes no lookahead getter -- and asserted at both ends of the rate band
/// in `tests/opus_export.rs`, which is what catches a version that changes it.
const OPUS_PRE_SKIP: u16 = 120;

/// The mix as Opus packets, one per [`OPUS_FRAME`], plus the pre-skip that has
/// to be declared in front of them. Mono or stereo, at [`OPUS_RATE`] -- the
/// caller checks all of it, because outside that envelope this encoder is not
/// one this project is willing to write files with ([`OPUS_MAX_KBPS`]).
///
/// The tail frame is padded with silence rather than dropped: Opus codes whole
/// frames and a track a few milliseconds short under a picture that is not is
/// the drift every other path here is written to avoid. The pre-skip trims the
/// front; the container's duration trims the back.
fn encode_opus(
    samples: &[f32],
    kbps: u32,
    channels: usize,
) -> crate::Result<Option<(Vec<crate::AacPacket>, u16)>> {
    let mut encoder = ec_opus::Encoder::new(OPUS_RATE, channels, ec_opus::Application::Audio)
        .map_err(|e| format!("the Opus encoder refused 48 kHz {channels}-channel sound: {e}"))?;
    // CBR, the crate's default: every packet exactly the rate's size. The old
    // seat ran opus-rs's VBR; the gate below measures either, and a fixed
    // packet size is the friendlier neighbour to a muxer's block budget.
    encoder.set_bitrate(kbps.min(OPUS_MAX_KBPS) * 1_000);
    // No complexity knob on this crate: the reference's default (9) is baked
    // into its rate arithmetic, one percent of equiv rate away from the 10 the
    // old encoder was asked for.
    let mut packets = Vec::new();
    let mut frame = vec![0f32; OPUS_FRAME * channels];
    // 1275 bytes is the most one Opus frame may weigh (RFC 6716 §3.2); this is
    // that with room for the TOC and a padded frame, so `encode_float` never
    // has to refuse for want of a buffer.
    let mut out = vec![0u8; 1500];
    // The head, coded **twice**. An encoder's very first frame has no history
    // behind it and comes out ramped and out of phase -- measured on a 440 Hz
    // fixture, the first 20 ms of the export correlated 0.09 with the source
    // where every later window correlated 0.9997 -- and 20 ms of wrong sound at
    // the head of a file is the kind of thing nobody hears in a test and
    // everybody hears in the first frame of a cut. So the first frame is fed in
    // once to warm the encoder, and thrown away again by the pre-skip: exactly
    // what pre-skip is for (RFC 7845 §4.2), at the cost of one packet.
    let coded = std::time::Instant::now();
    let warm = samples.len().min(OPUS_FRAME * channels);
    // ...and one silent frame after the sound, for the same delay seen from the
    // other end: the encoder is [`OPUS_PRE_SKIP`] samples behind its input, so
    // without a frame to push them out the last 2.5 ms of the timeline stay
    // inside it and the track ends short under a picture that does not.
    let blocks = std::iter::once(&samples[..warm])
        .chain(samples.chunks(OPUS_FRAME * channels))
        .chain(std::iter::once(&samples[..0]));
    for block in blocks {
        frame[..block.len()].copy_from_slice(block);
        frame[block.len()..].fill(0.0);
        let len = encoder
            .encode_float(&frame, OPUS_FRAME, &mut out)
            .map_err(|e| format!("Opus encode failed: {e}"))?;
        packets.push(crate::AacPacket {
            bytes: out[..len].to_vec(),
            samples: OPUS_FRAME as u32,
        });
    }
    // ...and now the part that is not optimism: what was just written is
    // decoded again and measured against what went in, and a track that does
    // not come back is not written at all ([`OPUS_MIN_FIDELITY`]). The caller
    // falls through to AAC, which is exactly where it stood before this seat
    // existed -- a worse codec is a fair trade for a track that is the sound.
    //
    // corner-cut: this costs one Opus decode of the whole track (a minute of
    // sound in a few hundred milliseconds, against a video encode that is
    // minutes). Upgrade path is deleting it, the day this encoder can be
    // trusted at the rate it is asked for.
    let encoded = coded.elapsed().as_secs_f64();
    let pre_skip = OPUS_PRE_SKIP + OPUS_FRAME as u16;
    let measured = std::time::Instant::now();
    let fidelity = opus_fidelity(&packets, samples, usize::from(pre_skip), channels);
    if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!(
        "export audio: Opus encode {encoded:.1} s, fidelity {fidelity:.4} measured in {:.1} s",
        measured.elapsed().as_secs_f64()
    ); }
    Ok((fidelity >= OPUS_MIN_FIDELITY).then_some((packets, pre_skip)))
}

/// Packets one sampled window listens to: 5 s at [`OPUS_FRAME`].
const FIDELITY_WINDOW: usize = 250;

/// How many of them a long track is judged on -- a minute of sound, wherever
/// the track's length puts them, and a constant cost rather than one that grows
/// with the film ([`opus_fidelity`]).
const FIDELITY_WINDOWS: usize = 12;

/// Packets decoded into a window and thrown away before it is measured. CELT
/// predicts across frames, so a decoder started cold mid-stream is behind its
/// own history for a few frames; 100 ms is far more than it needs and is not
/// worth counting.
const FIDELITY_WARMUP: usize = 5;

/// How much sound a window must carry before its correlation means anything:
/// mean square per sample. A window of digital silence -- the head of a film,
/// the gap between a feature and its credits -- correlates with nothing, and
/// failing a track for it would send perfectly good sound to AAC.
const FIDELITY_FLOOR: f64 = 1e-9;

/// The correlation between `samples` and the decode of `packets` with the first
/// `pre_skip` frames dropped: 1.0 is the same waveform, and this codec's failure
/// mode lands near 0.1. Streamed packet by packet -- the decode is never held,
/// only three running sums -- because the mix behind it is already the biggest
/// thing in an export ([`encode_audio`]'s corner-cut).
///
/// A packet the decoder refuses is 0.0 and no argument: a track this project
/// cannot read is exactly what this is here to catch.
///
/// **Sampled above a minute of sound, and the median window is the answer.**
/// This decode was the whole cost of exporting a feature film under the
/// previous decoder: `ruopus` ran its inverse MDCT at 0.2x real time (measured,
/// 24.2 s for 121.5 s of stereo; `ec-opus`, the decoder here now, is far
/// quicker, and the sampling survives it because it costs almost nothing to
/// keep),
/// so judging every packet of a two-and-a-half-hour film costs half an hour
/// before a byte of picture is written -- which is what it did, and what made a
/// copy export that has no encoding to do at all sit at zero for twenty minutes.
/// [`FIDELITY_WINDOWS`] windows spread evenly across the track cost the same
/// minute of decode whatever the film's length, and what they are looking for
/// survives sampling: [`OPUS_MAX_KBPS`]'s cliff is a *rate* the encoder falls
/// off everywhere that rate is used, not a defect of one bar of music.
///
/// The **median** and not the worst, and that is measured rather than cautious.
/// Forty five-second windows of a real 5.1 film's mix, encoded here at 128 kbps
/// and decoded back, in order:
///
/// ```text
/// 0.803  0.835  0.938  0.940  0.960  0.987  0.994  ... median 0.998 ... 0.999
/// ```
///
/// Five of the thirty-nine that carried sound sit under [`OPUS_MIN_FIDELITY`] on
/// a track that is *good* -- which is the same thing the table in
/// [`OPUS_MAX_KBPS`] shows, where a span scoring 0.937 at 128 kbps is inside the
/// usable band. A worst-window rule at 0.95 therefore refuses films this encoder
/// handles perfectly well, while the failure it exists for is the opposite
/// shape: at 136 kbps the same three measured spans read 0.452, 0.697, 0.951,
/// and at 256 kbps the whole track correlates 0.06. Half a track cannot be
/// ruined without the middle of the list moving, so the median separates the two
/// by a margin no single window can (0.998 against under 0.7) -- and a track
/// under a minute is one window, where median and whole-track correlation are
/// the same number the gate always used.
fn opus_fidelity(
    packets: &[crate::AacPacket],
    samples: &[f32],
    pre_skip: usize,
    channels: usize,
) -> f64 {
    // Short enough to hear all of: every fixture, every unit test, and any
    // track under a minute. There is nothing to sample from.
    let all = FIDELITY_WINDOW * FIDELITY_WINDOWS;
    if packets.len() <= all {
        return window_fidelity(packets, samples, pre_skip, channels, 0, 0, packets.len())
            .unwrap_or(1.0);
    }
    // Evenly spread, first window at the head: the head is where an encoder's
    // own warm-up would show, and the last starts a window short of the end so
    // no window runs past the packets.
    let step = (packets.len() - FIDELITY_WINDOW) / (FIDELITY_WINDOWS - 1);
    let mut scores: Vec<f64> = Vec::with_capacity(FIDELITY_WINDOWS);
    for w in 0..FIDELITY_WINDOWS {
        let at = w * step;
        let from = at.saturating_sub(FIDELITY_WARMUP);
        let Some(fidelity) = window_fidelity(
            packets,
            samples,
            pre_skip,
            channels,
            from,
            at,
            at + FIDELITY_WINDOW,
        ) else {
            continue; // silence: nothing to correlate, and not a failure
        };
        scores.push(fidelity);
    }
    // Every window silent is a silent track, which this codec writes as silence.
    if scores.is_empty() {
        return 1.0;
    }
    scores.sort_by(f64::total_cmp);
    scores[scores.len() / 2]
}

/// [`opus_fidelity`] over `packets[measure..to]`, with `packets[from..measure]`
/// -- the warm-up -- decoded for its state and thrown away. `None` where the
/// source window carries no sound worth correlating ([`FIDELITY_FLOOR`]).
fn window_fidelity(
    packets: &[crate::AacPacket],
    samples: &[f32],
    pre_skip: usize,
    channels: usize,
    from: usize,
    measure: usize,
    to: usize,
) -> Option<f64> {
    let (streams, coupled, mapping) = match channels {
        1 => (1, 0, vec![0u8]),
        // One coupled stream: the stereo shape every fixture and every mix
        // here decodes back through.
        _ => (1, 1, vec![0, 1]),
    };
    let mut decoder = ec_opus::MultistreamDecoder::with_rate(OPUS_RATE, streams, coupled, &mapping);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    // Where in the mix this window's first decoded sample lands: every packet
    // is [`OPUS_FRAME`] long, and the pre-skip is what the front of the stream
    // owes before the first of them is the timeline's frame 0.
    let mut at = (from * OPUS_FRAME * channels).saturating_sub(pre_skip * channels);
    let mut drop = (pre_skip * channels).saturating_sub(from * OPUS_FRAME * channels);
    let mut counted = 0usize;
    for (index, packet) in packets.iter().enumerate().take(to).skip(from) {
        let Ok(pcm) = decoder.decode_packet(&packet.bytes) else {
            return Some(0.0);
        };
        let pcm = &pcm[drop.min(pcm.len())..];
        drop = drop.saturating_sub(packet.samples as usize * channels);
        // The warm-up is decoded for its state and not for its numbers.
        if index < measure {
            at += pcm.len();
            continue;
        }
        for (want, got) in samples[at.min(samples.len())..].iter().zip(pcm) {
            let (x, y) = (f64::from(*want), f64::from(*got));
            num += x * y;
            da += x * x;
            db += y * y;
            at += 1;
            counted += 1;
        }
    }
    (da / counted.max(1) as f64 > FIDELITY_FLOOR).then(|| num / (da.sqrt() * db.sqrt()).max(1e-12))
}

/// The audio pass, running beside the picture: what [`run`] holds while it
/// encodes and joins where the file it is writing needs the track.
type AudioJob = thread::JoinHandle<crate::Result<Option<ExportAudio>>>;

/// The pass while [`run`] holds it, joined however that function leaves --
/// including through a `?`.
///
/// A [`JoinHandle`](thread::JoinHandle) that is merely dropped *detaches* its
/// thread, and the one error that matters here is the hardware encoder's
/// process dying ([`crate::hwproc`]): the whole export is then written again by
/// [`start`], which starts an audio pass of its own. The abandoned one would go
/// on mixing and encoding the same sound beside it -- two passes of a feature
/// film's audio at once, on the machine that has just lost its GPU. Joined
/// instead, so an export has one audio pass at a time and none outlives it.
struct AudioPass(Option<AudioJob>);

impl AudioPass {
    /// Whether the pass has landed, so the picture loop may collect it without
    /// waiting.
    fn is_finished(&self) -> bool {
        self.0.as_ref().is_some_and(thread::JoinHandle::is_finished)
    }

    /// Whether a pass is still to be collected -- which is what "the sound is
    /// still being worked out" means on the encoder line.
    fn running(&self) -> bool {
        self.0.is_some()
    }

    /// The handle, for the one caller that is about to join it itself.
    fn take(&mut self) -> Option<AudioJob> {
        self.0.take()
    }
}

impl Drop for AudioPass {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            let _ = job.join();
        }
    }
}

/// The pass's answer, or the failure that is the export's failure. A panicked
/// audio thread reads as an error rather than as silence: an export that wrote
/// its picture and quietly no sound is the one outcome this must not have.
fn join_audio(job: AudioJob) -> crate::Result<Option<ExportAudio>> {
    job.join()
        .map_err(|_| crate::Error::from("the audio pass stopped without a picture or a reason"))?
}

/// The encoder line a running export shows: the seat the picture is really
/// being written by, and what the sound is -- or that the sound is still being
/// worked out, which an overlapped audio pass is until it is collected.
fn publish_seats(
    shared: &Shared,
    seat: &str,
    project: &Project,
    settings: &ExportSettings,
    sound: Option<bool>,
    opus: bool,
    waiting: bool,
) {
    let audio = match waiting {
        // Still mixing on the other thread. The same half-line the copied-
        // picture path has always shown, and for the same reason: what is
        // published is never a codec that did not run.
        true => "working out the sound…",
        false => measured_audio_label(project, settings.format, sound, opus),
    };
    *shared.encoders.lock().unwrap() = Some(format!("{seat} · {audio}"));
}

/// Hands a mix that finished mixing early to the mp4 it belongs in, the
/// moment both exist: `late` may land before the first coded picture has
/// opened `muxer` at all, so this is tried both right where the mix lands
/// and after every picture written afterwards, and does nothing once
/// `*fed` says an earlier try already succeeded. Declaring the track and
/// queuing its packets here is what lets [`crate::mux::Mp4Muxer`]'s own
/// hold interleave them under the pictures still to come, rather than the
/// track landing all at once after the picture is done.
fn feed_late_audio(
    muxer: &mut Option<Muxer>,
    late: &Option<ExportAudio>,
    fed: &mut bool,
) -> crate::Result<()> {
    if *fed {
        return Ok(());
    }
    let (Some(track), Some(Muxer::Mp4(mp4))) = (late.as_ref(), muxer.as_mut()) else {
        return Ok(());
    };
    mp4.add_audio_track(&params_of(track))?;
    for packet in &track.packets {
        mp4.write_audio_packet(&packet.bytes)?;
    }
    *fed = true;
    Ok(())
}

/// The language the timeline's sound is in, for the export to tag its audio track with: the one
/// language every audio clip's stream states. `None` when the clips disagree, when one of them
/// states none (`und`), or when there are no audio clips, and the track stays `und`.
pub(crate) fn audio_language(project: &Project) -> Option<String> {
    let sources = project.sources();
    shared_language(
        project
            .audio_lanes()
            .into_iter()
            .flat_map(|lane| project.lane(lane).iter())
            .map(|clip| sources.get(clip.source).and_then(|src| stream_language(&src.path, src.audio_stream))),
    )
}

/// The one language every stream in `langs` states, or `None` when there are none, when one
/// states none, or when they differ.
fn shared_language(langs: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let mut shared: Option<String> = None;
    for lang in langs {
        let lang = lang?;
        match &shared {
            None => shared = Some(lang),
            Some(s) if *s == lang => {}
            Some(_) => return None,
        }
    }
    shared
}

/// The language audio stream `audio_stream` of `path` states in its header, with `und` (none
/// stated) as `None`. Read from the header alone.
fn stream_language(path: &Path, audio_stream: usize) -> Option<String> {
    crate::probe_media(path)
        .streams
        .into_iter()
        .find(|s| s.kind == crate::StreamKind::Audio && s.index == audio_stream)?
        .language
        .filter(|l| l != "und")
}

/// The language the picture is tagged with, for the Matroska video track to state: the first
/// source's picture track, the one the export takes its picture from ([`source_colour_tags`]).
/// `None` where that track states none (`und`), or no source can be read.
fn video_language(project: &Project) -> Option<String> {
    let path = &project.sources().first()?.path;
    crate::probe_media(path)
        .streams
        .into_iter()
        .find(|s| s.kind == crate::StreamKind::Video && s.index == 0)?
        .language
        .filter(|l| l != "und")
}

/// The colour tags of the first source, exactly as its container states them:
/// what a file that keeps its source's colour declares, unspecified fields left
/// unspecified. Nothing when no source can be read, which declares nothing.
fn source_colour_tags(project: &Project) -> Tags {
    project
        .sources()
        .first()
        .and_then(|source| crate::demux::Demuxer::open(&source.path).ok())
        .map(|(_, demuxer)| demuxer.colour_tags())
        .unwrap_or_default()
}

fn params_of(track: &ExportAudio) -> AudioParams {
    AudioParams {
        freq_index: track.params.freq_index,
        chan_conf: track.params.chan_conf,
        sample_rate: track.params.sample_rate,
        opus_pre_skip: track.opus_pre_skip,
        language: track.language.clone(),
    }
}

fn run(
    project: &Project,
    meta: &VideoMeta,
    out: &Path,
    shared: &Arc<Shared>,
    settings: &ExportSettings,
    sample_rate: Option<u32>,
) -> crate::Result<()> {
    // The media's length and not the timeline's: a caption placed past the last
    // picture holds the *timeline* open ([`Project::timeline_frames`]) and there
    // is nothing to write under it -- the picture loop walks the composite spans
    // and stops with them, so a file padded to the caption's end would be
    // trailing silence over a frozen last frame. The export ends with the last
    // picture or sound, the cues are cut off there ([`lane_subtitles`]), and
    // [`planned_subtitles`] says so before the button is pressed.
    // The range's own length where one is marked, the media's otherwise --
    // `export_spans` walks the same window, so `done` ends on this `total`
    // exactly as it always did.
    let total = settings.range.map_or(project.media_frames(), |(s, e)| {
        e.min(project.media_frames()).saturating_sub(s)
    });
    // The two preview switches are the H.264 mp4 pair's alone. Only that
    // container carries a colour of its own back out ([`write_video`] tags it
    // there), so asking any other for a source-space file would write pictures
    // in one space and a header in another; and only that codec's seats honour
    // `intra_only` -- the H.264 and HEVC pairs do, and an AV1 one would quietly
    // write the long-GOP file it always did.
    if settings.keep_source_colour && settings.format != Format::Mp4 {
        return Err(format!(
            "{} cannot be written in its source's colour space",
            settings.format.name()
        )
        .into());
    }
    if settings.intra_only && !matches!(settings.format, Format::Mp4 | Format::Hevc | Format::HevcMp4) {
        return Err(format!("{} cannot be written intra-only", settings.format.name()).into());
    }
    if let Some(why) = exact_refusal(project, meta, settings) {
        return Err(why.into());
    }
    let hdr_colour = hdr_colour_of(settings)?;
    let sources = project.sources();
    // The picture's decision first, before a sample of sound is touched: it is
    // the answer that decides what the whole export *is*, it costs one header
    // read per source, and it is what the line on screen has been missing --
    // an export that copies its picture used to say nothing at all for as long
    // as its sound took, which on a feature film was minutes. Half a line now,
    // completed by the measured codec once the sound is known below: what is
    // published is never a codec that did not run.
    let plan = CopyPlan::of(project, meta, settings);
    if plan.is_some() {
        if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export video: copy (source packets)"); }
        *shared.encoders.lock().unwrap() = Some("copy · working out the sound…".into());
    }
    // ...then the audio, on a thread of its own, started before anything is
    // opened for the picture.
    //
    // Where it is *waited* for is the whole of this: a Matroska file interleaves
    // its audio into the clusters from the header on, so that path needs every
    // packet before the first coded picture and there is nothing to overlap. An
    // mp4 writes its audio track after the whole picture -- and now declares it
    // there too ([`MkvMuxer`]'s opposite, `Mp4Muxer::add_audio_track`) -- so the
    // picture's loop needs nothing at all from this until it has finished, and
    // the mix, its encode and the encode of every frame run at once. An export
    // used to cost the sound *plus* the picture; an mp4 one now costs the longer
    // of the two. On this project's own film that is 168 s of AC-3 that no
    // longer stands in front of 733 s of hardware H.264.
    // What a file keeping its source's colour declares, read once: the picture
    // is written in that space, so the tags are the source's own, not resolved.
    let source_colour = match settings.keep_source_colour {
        true => source_colour_tags(project),
        false => Tags::default(),
    };
    // The picture's language, which the Matroska video track states (`und` where none).
    let source_language = video_language(project);
    // A ProRes file's sound is PCM (24-bit unless asked for 16) mixed on a thread of its own and written
    // after the picture ([`PcmPass`]); no AAC pass runs beside it.
    let bits = prores_audio_bits(settings);
    let pcm_job = match settings.format.is_prores() {
        true => {
            let (project, meta, settings, shared) =
                (project.clone(), *meta, settings.clone(), Arc::clone(shared));
            PcmPass(Some(
                thread::Builder::new()
                    .name("export-pcm".into())
                    .spawn(move || mix_pcm16(&project, &meta, &shared, &settings, sample_rate, bits))?,
            ))
        }
        false => PcmPass(None),
    };
    let audio_job = if settings.format.is_prores() { None } else { Some({
        let (project, meta, settings, shared) =
            (project.clone(), *meta, settings.clone(), Arc::clone(shared));
        thread::Builder::new()
            .name("export-audio".into())
            .spawn(move || {
                let track = copy_audio(
                    &project,
                    &meta,
                    audio_kbps_of(&settings),
                    settings.format.is_mkv(),
                    settings.format == Format::Webm,
                    &shared,
                    // WebM's sound is mixed at the rate Opus codes at, so the
                    // one encoder path it takes is the one that resamples.
                    match settings.format {
                        Format::Webm => Some(OPUS_RATE),
                        _ => sample_rate,
                    },
                    settings.range,
                )?;
                // The language the sound is in, on the copy and the mix alike:
                // the one tag the whole track carries.
                Ok(track.map(|track| ExportAudio {
                    language: audio_language(&project),
                    ..track
                }))
            })?
    }) };
    // A Matroska file (and a copied one, whose muxer is the same) collects it
    // here; an mp4 leaves it running and collects it under the picture below.
    // Held in a guard from here on, so no way out of this function leaves the
    // pass running behind it ([`AudioPass`]).
    let mut audio_job = AudioPass(audio_job);
    // The condition is the *format*, and the copied-picture path below relies
    // on that covering it too: `plan` hands its packets straight to a Matroska
    // muxer, so it may only exist where this has already collected them --
    // which holds because [`CopyPlan::of`] returns `None` for every format
    // that is not `Hevc` or `Av1`, both of which are `is_mkv`. Said out loud
    // because a copy plan for an mp4 would otherwise reach `plan.run` with the
    // sound still being mixed on the other thread.
    debug_assert!(
        settings.format.is_mkv() || plan.is_none(),
        "a copy plan outside Matroska would run before its sound is collected"
    );
    let mut pcm_job = pcm_job;
    let audio = match settings.format.is_mkv() {
        true => join_audio(audio_job.take().expect("the job was just made"))?,
        false => None,
    };
    let audio_params = audio.as_ref().map(params_of);
    // What the track *is*, kept before the packets are handed to a muxer: the
    // encoder line names a copy or an encode and only `copy_audio` knows which.
    // `None` on the mp4 path until the pass is collected -- the line says it is
    // still working the sound out, which is what it is doing.
    let mut sound = audio.as_ref().map(|track| track.copied);
    // ...and *which* encoder, for the same line: the card predicts Opus from the
    // format alone and cannot know the mix's rate or width ([`audio_label`]),
    // while this is after the mix was opened, so it is the measured answer.
    let mut opus = audio
        .as_ref()
        .is_some_and(|track| track.opus_pre_skip.is_some());
    // Taken by the Matroska muxer at creation; the mp4 one writes its track
    // after the picture, so for that path this is filled in at the end.
    let mut packets = audio.map(|track| track.packets);
    // ...and the cues with them, for the same reason: the subtitle track is
    // declared in the header and its blocks are interleaved into the clusters,
    // so the muxer needs the lot before the first picture is written.
    let mut subs = export_subtitles(project, meta, settings);
    // Whatever the sound has reached, the picture's share starts here
    // ([`AUDIO_BAND`]) -- and the bar only ever moves forwards, which is what
    // `fetch_max` below keeps true while an audio pass is still publishing its
    // own third of that band from the other thread.
    shared.progress.fetch_max(AUDIO_BAND, Ordering::Relaxed);

    // Before an encoder is opened at all: a timeline nobody has touched, whose
    // sources this file can hold as they are, is written by copying their coded
    // blocks -- the picture's half of what `copy_audio` just did for the sound,
    // and the difference between an export bounded by the *edited* spans and one
    // that re-encodes a whole film to change a cut. [`CopyPlan`] states the
    // whole rule; anything at all outside it falls through to the walk below.
    if let Some(plan) = plan {
        *shared.encoders.lock().unwrap() = Some(format!(
            "copy · {}",
            // The measured codec, exactly as the encoder path below publishes
            // it: the mix has been opened by now, so a line that fell back to
            // AAC says AAC. Naming the prediction here would make the fallback
            // invisible on precisely the exports that are over in seconds.
            measured_audio_label(project, settings.format, sound, opus)
        ));
        return plan.run(
            out,
            meta,
            shared,
            audio_params.as_ref().zip(packets.take()),
            std::mem::take(&mut subs),
            total,
            &settings.chapters,
        );
    }

    // One header parse per source, before any of them is decoded: what rate
    // each file was shot at against the timeline's ([`Rate`]), which is how the
    // clip's timeline frames below become frames of the file, and what its
    // samples *mean* ([`ColorDescription`]), which is what decides whether they
    // are remapped on the way in. Real time for a still, a song and a
    // single-rate project, where every conversion below is the identity.
    let rates: Vec<(
        Rate,
        ColorDescription,
        Option<f32>,
        Rotation,
        crate::demux::PixelAspect,
    )> = sources
        .iter()
        .map(|source| source_rate(&source.path, meta.frame_rate))
        .collect();
    // ...and, for a source on an HDR curve, the tone map that brings its samples
    // down to the SDR this file is written in ([`crate::tonemap`]). Once per
    // source stream: the table costs a couple of milliseconds to build and a
    // span reopens its file, so building it here rather than in the loop below
    // is the difference between paying that once and paying it per cut. An SDR
    // source -- the whole of an ordinary project -- builds no table at all.
    //
    // Through the project's own preset ([`tonemap::Preset`]) and each source's
    // own declared peak, which is the pair playback's decode funnel builds its
    // tables with: preview and export are the same rendition of the same film
    // because they are told the same two things, not because two numbers happen
    // to agree.
    let preset = project.tone();
    let tone: Vec<Option<ToneMapper>> = rates
        .iter()
        .map(|(_, color, peak, _, _)| match color.transfer {
            // A preview file keeps the film's own curve and is tone-mapped when
            // it is *shown* ([`ExportSettings::keep_source_colour`]), which is
            // both cheaper and the only way the preset can still be changed
            // after the stand-in was made.
            _ if settings.keep_source_colour => None,
            // An HDR export keeps the HDR sources' own curve: no tone map to SDR
            // ([`ExportSettings::hdr`], [`hdr_colour_of`] refuses a curve change).
            _ if hdr_colour.is_some() => None,
            Transfer::Sdr => None,
            Transfer::Pq => Some(ToneMapper::new(tonemap::Transfer::Pq, preset, *peak)),
            Transfer::Hlg => Some(ToneMapper::new(tonemap::Transfer::Hlg, preset, *peak)),
        })
        .collect();
    // ...and the one space they are all written in, the same rule a reader with
    // no tags to read would apply to this file's height -- or, for a preview
    // file, the source's own, which is what makes every remap below the
    // identity and the file's `colr` box its film's.
    // The software seat's depth ([`wide_ten_bit`]): a 10-bit timeline is
    // decoded and encoded at 10 bits wherever its pictures pass through untouched.
    for (_, color, ..) in &rates {
        let clash = hdr_colour.is_some_and(|out| color.transfer != Transfer::Sdr && color.transfer != out.transfer);
        if clash {
            let out = hdr_colour.expect("a clash needs an HDR output");
            return Err(format!(
                "a source is {} and this export is {}: converting one HDR curve to the other is not done here, so pick the export's transfer to match, or SDR to tone-map",
                transfer_name(color.transfer),
                transfer_name(out.transfer)
            )
            .into());
        }
    }
    let ten_bit = wide_ten_bit(settings, || {
        sources
            .iter()
            .any(|s| Demuxer::open(&s.path).is_ok_and(|(_, d)| d.bit_depth() >= 10))
    })?;
    // The colour a 10-bit SDR export declares: its sources' own matrix, where every
    // clip is SDR in one matrix ([`source_sdr_colour`]). Every other export keeps the
    // output rule, and so does the encoder's declared colour when there is no such one.
    let source_sdr = match ten_bit && !settings.keep_source_colour {
        // An HDR export declares its own colour, whatever the sources are.
        true if hdr_colour.is_some() => hdr_colour,
        true => source_sdr_colour(&rates.iter().map(|(_, c, ..)| *c).collect::<Vec<_>>()),
        false => None,
    };
    let rule = ColorDescription::output_sized(meta.width, meta.height);
    let out_color = match settings.keep_source_colour {
        true => meta.color,
        false => source_sdr.unwrap_or(rule),
    };
    let container_colour = source_sdr.map_or(source_colour, Tags::of);

    // The captions drawn into each picture, if the export burns any in ([`Burn`]).
    let mut burn = Burn::new(project, settings, meta.frame_rate, out_color.full_range)?;
    let mut encoder = Enc::open(meta, settings, ten_bit, source_sdr.unwrap_or(rule))?;
    // What this file is really being written by, for a progress line to name.
    // Published *after* the seat is open, so a hardware encoder the driver
    // refused reads as the software one that took over rather than as the hope
    // it replaced -- and the sound says whether it was copied or encoded, which
    // only `copy_audio` knows and, on the overlapped path, does not know yet.
    let seat = encoder.label();
    publish_seats(
        shared,
        seat,
        project,
        settings,
        sound,
        opus,
        audio_job.running(),
    );
    // When the picture started, so the stage line below can say where under it
    // the sound landed -- the one number that says whether the two passes
    // really overlapped, in the same voice as `encode_audio`'s own timers.
    let picture_started = std::time::Instant::now();
    // The sound, once the pass beside this one has finished with it: joined
    // here the moment it lands rather than only after the picture, so
    // `feed_late_audio` below can hand it to the muxer while pictures are
    // still being coded and let `Mp4Muxer`'s own hold interleave the two
    // ([`crate::mux::Mp4Muxer`]).
    let mut late: Option<ExportAudio> = None;
    // Set once `late`'s track has been handed to the muxer -- so the final
    // join below, which still exists for a mix that never lands mid-picture,
    // knows not to declare the track or write its packets a second time.
    let mut audio_fed = false;
    let mut muxer = None;
    // A ProRes file is declared up front: the encoder knows its own sample entry.
    if let Some(params) = encoder.prores_params() {
        muxer = Some(Muxer::Mov(crate::prores_out::MovWriter::create(
            out,
            params,
            crate::mux::frame_timing(meta.frame_rate)?,
        )?));
    }
    let mut done = 0u32;
    // ...and how many of the edit's frames a file could not give, summed over
    // the spans that ran out under their clip ([`Shared::truncated`]).
    let mut skipped = 0u32;
    let black = Black::new(meta);
    // Software decoders that spans leave behind for the next span of the same
    // file to continue (see [`Parked`]).
    let parked = Arc::new(Parked::default());
    // Spans, not clips: a gap in the video is part of the timeline and gets
    // encoded too, as black frames. The picture count is therefore
    // `media_frames` however the lanes are arranged -- [`export_spans`] is that
    // walk cut to it, so `done` ends on the `total` above instead of running
    // past it under a trailing caption -- and *which* lane a span comes from is
    // `composite_spans_from`'s answer, the same one playback shows, so an
    // export is what was watched.
    for span in export_spans(project, settings.range) {
        // Every clip reopens its own source file at its own in point; the
        // encoder is *not* reopened, so the export is one continuous stream
        // whose GOP boundaries need not line up with the cuts -- nor with the
        // file boundaries, which are just cuts that change the path.
        // Whether this span's tail dissolves into its successor -- decided
        // before A's own decoder is opened, because it is what decides
        // whether A may take the DMA fast path at all: the blend below needs
        // A's pixels on the CPU, so a dissolving span never asks for a GPU
        // buffer, not even outside its own window.
        //
        // A layered span (two or more clips on the stack, [`layered_at`]) takes
        // no decoder of its own: its pictures are composed by a [`LayerStack`]
        // opened beside it, placed and faded per clip, so it never dissolves,
        // never takes the DMA path and never runs the single-clip placement.
        let keyed_color = color_keyed_alone_at(project, span.start).filter(|_| ten_bit);
        let layered_here = layered_at(project, span.start) && keyed_color.is_none();
        let dissolve = dissolve_window(project, &span);
        let (mut pictures, rate, in_frame, color, mapper, rotation, pixel_aspect) =
            match span.from.filter(|_| !layered_here) {
            Some((source, in_frame)) => {
                let entry = sources
                    .get(source)
                    .ok_or_else(|| format!("clip names source {source} of {}", sources.len()))?;
                let (rate, color, _peak, rotation, pixel_aspect) = rates[source];
                // A field-mode clip of an interlaced file counts fields, two a frame.
                let deint = span_deint(project, span.start, &entry.path);
                let rate = crate::deinterlace::counted_rate(deint, rate);
                let mapper = tone[source].as_ref();
                // Nothing on the way from this span's decoder to the encoder
                // touches a sample: no tone map, no matrix remap, no grade --
                // which is what lets the *decoded buffer itself* be encoded,
                // never read back and never written out again. The canvas is
                // the fourth thing that would touch it, and the decoder answers
                // that one: a picture of another size than this file's is one
                // the plugin hands back as pixels ([`HwSession::next_frame_dma`]).
                let untouched = mapper.is_none()
                    && keyed_color.is_none()
                    && burn.is_none()
                    && remap_into(Some(color), false, out_color.matrix).is_none()
                    && project
                        .composite_color_at(span.start)
                        .is_none_or(|params| params.is_identity())
                    && project
                        .composite_transform_at(span.start)
                        .is_none_or(|params| params.is_identity())
                    // A file whose display matrix asks for a turn is *not*
                    // untouched, whatever else the timeline says: its pictures
                    // have to be turned, which is a read-back no GPU buffer
                    // survives. The sample aspect is the same refusal one
                    // stage later: its pictures have to be stretched to the
                    // square-pixel raster before an encoder sees them, which
                    // is the same kind of touch.
                    && rotation.is_none()
                    && pixel_aspect.is_square()
                    && dissolve.is_none()
                    && !picture_faded_in(project, &span);
                let want = untouched.then(|| encoder.dma_want(meta)).flatten();
                // Opened at the file's own frame, which is the only place the
                // file's numbering is used -- the span's are the timeline's.
                let flags = project.composite_visualizer_at(span.start);
                let pictures = if crate::is_audio(&entry.path) || flags & 1 != 0 {
                    ClipDecoder::visualizer(
                        &entry.path,
                        entry.audio_stream,
                        rate.source_at(in_frame),
                        (meta.width, meta.height),
                        meta.frame_rate,
                        flags,
                        project.composite_viz_paint_at(span.start),
                    )?
                } else if span.reverse && !crate::is_image(&entry.path) {
                    // Played backwards: from the file frame the span starts on, down
                    // to the lowest it reads. The loop below counts the pictures it
                    // takes off the decoder, which is how it reads a forward one.
                    let lo = span.source_window().map_or(0, |(_, lo, _)| rate.source_at(lo));
                    ClipDecoder::reversed(&entry.path, rate.source_at(in_frame), lo, deint)
                } else {
                    ClipDecoder::open_with(&entry.path, rate.source_at(in_frame), want, Some(&parked), ten_bit, deint)?
                };
                (
                    Some(pictures),
                    rate,
                    in_frame,
                    Some(color),
                    mapper,
                    rotation,
                    pixel_aspect,
                )
            }
            // A gap's black is 16/128/128, which is black in every matrix here
            // and on every curve: nothing to remap and nothing to tone-map,
            // which is what `None` says. A gap is no file either, so nothing
            // turns it.
            None => (
                None,
                Rate::REAL_TIME,
                0,
                None,
                None,
                Rotation::None,
                crate::demux::PixelAspect::SQUARE,
            ),
        };
        // Mixed spaces on one timeline: a clip coded against another matrix than
        // the one this file declares is rewritten into it, *after* the grade --
        // playback grades in the source's own space and converts for the screen
        // ([`crate::decode`]), so a grade applied to remapped samples is a
        // different picture than the one the canvas showed. Same-space clips --
        // the whole of an ordinary project -- take `None` and not a byte is
        // touched.
        //
        // ...and what the samples mean when that remap sees them is not always
        // what the file said, which is [`remap_into`]'s whole question.
        let remap = remap_into(color, mapper.is_some(), out_color.matrix);
        // What this span is graded by -- the same answer playback's decoder
        // carries, so the export is the picture that was watched. `None` for a
        // gap and for an ungraded clip, and that is the path where not a byte
        // is touched. Scratch outside the frame loop: it is refilled per frame
        // and its allocation survives the whole span.
        // A layered span's clips carry their own grades and transforms, in the
        // [`LayerStack`] that composes them; the top clip's do not apply here.
        let grade = match layered_here {
            true => None,
            false => project.composite_color_at(span.start).copied(),
        };
        let transform = match layered_here {
            true => TransformParams::default(),
            false => project
                .composite_transform_at(span.start)
                .copied()
                .unwrap_or_default(),
        };
        // The composing stack for a layered span, open for the whole span.
        let mut layers = match layered_here {
            true => Some(LayerStack::open(
                project,
                meta,
                &stack_at(project, span.start),
                out_color.matrix,
                None,
                Some((&shared.cancel, &shared.measuring)),
            )?),
            false => None,
        };
        // A 10-bit export composes a layered picture at 10 bits too ([`LayerStack::picture16`]).
        let layered_wide = ten_bit && layers.is_some();
        let mut graded = (Vec::new(), Vec::new(), Vec::new());
        let mut transformed = (Vec::new(), Vec::new(), Vec::new());
        // The 10-bit scratch of A's placement, and the 10-bit copies of an 8-bit
        // picture (or of B's) that composes at 10 bits ([`widen_into`]).
        let mut scratch16 = Scratch16::default();
        let mut widened_a: Planes16 = Planes16::default();
        let mut widened_b: Planes16 = Planes16::default();
        // A lone clip's effects stack on this path draws on copies of its placed
        // picture, at whichever depth it was placed ([`color_keyed_alone_at`]).
        let fx_clip = keyed_color.filter(|clip| crate::effects::active(&clip.effects));
        let (mut fx8, mut fx16): (Planes, Planes16) = Default::default();
        let mut fx_scratch8 = crate::effects::Scratch::<u8>::default();
        let mut fx_scratch16 = crate::effects::Scratch::<u16>::default();
        // ...and one more for the file's own display-matrix turn, which happens
        // before all of those (see [`place_picture`]): empty for every source
        // that states no turn. `stretched` is its twin for the pixel aspect,
        // which happens right after it.
        let mut rotated = (Vec::new(), Vec::new(), Vec::new());
        let mut stretched = (Vec::new(), Vec::new(), Vec::new());
        // ...and the canvas it is placed on, which is where a source of another
        // resolution becomes a picture at the project's. The same `Composer`
        // playback composes with, given the same policy, so an export is what
        // was watched down to the letterbox; a clip already the project's size
        // passes through it untouched.
        let mut canvas = Composer::new(
            meta.width,
            meta.height,
            project.composite_fit_at(span.start),
        );
        // The 10-bit canvas: its own composer, because the 8-bit placement above
        // borrows `canvas` in the same frame. It places the same fit.
        let mut canvas16 = Composer::new(
            meta.width,
            meta.height,
            project.composite_fit_at(span.start),
        );
        // Scratch for the 8-bit composition of a 10-bit picture ([`narrowed`]),
        // one for A's picture and one for B's.
        let mut narrow_a: [Vec<u8>; 3] = Default::default();
        let mut narrow_b: [Vec<u8>; 3] = Default::default();
        // B's own decoder, opened beside A's: the two streams come off their
        // files together for exactly the frames the tail below blends, and
        // nothing else in this span ever touches it.
        let mut b = dissolve
            .map(
                |(b_source, b_in_frame, window, b_start)| -> crate::Result<_> {
                    let entry = sources.get(b_source).ok_or_else(|| {
                        format!("clip names source {b_source} of {}", sources.len())
                    })?;
                    let (b_rate, b_color, _peak, b_rotation, b_pixel_aspect) = rates[b_source];
                    // The blended tail of a dissolve takes one picture per timeline
                    // frame, so it never counts fields: `Field` is its `Frame`.
                    let b_deint = frame_counting(span_deint(project, b_start, &entry.path));
                    let b_rate = crate::deinterlace::counted_rate(b_deint, b_rate);
                    let b_mapper = tone[b_source].as_ref();
                    let b_remap = remap_into(Some(b_color), b_mapper.is_some(), out_color.matrix);
                    Ok(Dissolve {
                        window,
                        kind: crate::transitions::kind_at(project, span.start),
                        tail_start: span.len - window,
                        decoder: if crate::is_audio(&entry.path)
                            || project.composite_visualizer_at(b_start) & 1 != 0
                        {
                            ClipDecoder::visualizer(
                                &entry.path,
                                entry.audio_stream,
                                b_rate.source_at(b_in_frame),
                                (meta.width, meta.height),
                                meta.frame_rate,
                                project.composite_visualizer_at(b_start),
                                project.composite_viz_paint_at(b_start),
                            )?
                        } else {
                            ClipDecoder::open_with(&entry.path, b_rate.source_at(b_in_frame), None, Some(&parked), ten_bit, b_deint)?
                        },
                        mapper: b_mapper,
                        remap: b_remap,
                        grade: project.composite_color_at(b_start).copied(),
                        transform: project
                            .composite_transform_at(b_start)
                            .copied()
                            .unwrap_or_default(),
                        rotation: b_rotation,
                        pixel_aspect: b_pixel_aspect,
                        canvas: Composer::new(
                            meta.width,
                            meta.height,
                            project.composite_fit_at(b_start),
                        ),
                        rotated: (Vec::new(), Vec::new(), Vec::new()),
                        stretched: (Vec::new(), Vec::new(), Vec::new()),
                        graded: (Vec::new(), Vec::new(), Vec::new()),
                        transformed: (Vec::new(), Vec::new(), Vec::new()),
                        scratch16: Scratch16::default(),
                    })
                },
            )
            .transpose()?;
        // Timeline frames, taken a *source* frame at a time: at a rate other
        // than real time the two are not the same count, and the span says which
        // source frame each timeline frame shows ([`Speed::source_at`]) -- the
        // very frame playback is holding there, because the two conversions are
        // one floor/ceil pair (`speed_maps_both_ways`). A speed-up decodes the
        // frames it skips and drops them, because the pictures after them
        // reference those; a slow-down encodes the picture it is holding again
        // rather than decoding it twice. At real time `repeats` is 1 and `want`
        // advances by one, which is the loop this always ran.
        let mut done_here = 0u32;
        // Source frames already taken out of this span's decoder.
        let mut taken = 0u32;
        // The whole of the mapping, in the order it composes: the speed says
        // which of the clip's (timeline-rate) frames a timeline frame shows,
        // and the rate says which frame of the *file* that is -- counted from
        // the one the decoder was opened at. At real time and at the timeline's
        // own rate this is `Speed::source_at` and nothing else, which is the
        // walk a single-rate project has always taken.
        let opened_at = rate.source_at(in_frame);
        let source_at =
            |offset: u32| rate.source_at(in_frame + span.source_offset(offset)) - opened_at;
        while done_here < span.len {
            cancelled(shared)?;
            // The sound's failure is the export's, and it is heard the moment
            // it happens rather than after the last frame: a mix this engine
            // cannot decode used to refuse in seconds, before any picture was
            // encoded, and an overlapped pass that reported only at the end
            // would spend the whole encode first. Also where its measured
            // codec reaches the line on screen.
            if audio_job.is_finished() {
                late = join_audio(audio_job.take().expect("just seen to be there"))?;
                sound = late.as_ref().map(|track| track.copied);
                opus = late
                    .as_ref()
                    .is_some_and(|track| track.opus_pre_skip.is_some());
                publish_seats(shared, seat, project, settings, sound, opus, false);
                if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!(
                    "export audio: done {:.1} s into the picture",
                    picture_started.elapsed().as_secs_f64()
                ); }
                feed_late_audio(&mut muxer, &late, &mut audio_fed)?;
            }
            let want = source_at(done_here);
            // A colour-keyed clip on the 10-bit path: this picture's grade is the
            // keys' value at its first timeline frame (a slowed picture repeated
            // over several frames keeps it).
            let grade = match keyed_color {
                Some(clip) => Some(crate::project::clip_color_at(
                    clip,
                    grade.unwrap_or_default(),
                    span.start + done_here,
                )),
                None => grade,
            };
            // How many timeline frames from here show that same picture: one at
            // real time, more when the clip is slowed or its file is slower
            // than the timeline. Encoded again rather than decoded twice.
            // A layered span composes every timeline frame, so it never repeats one.
            // ...nor does a lone clip whose effects move from frame to frame (a
            // shake); a static stack is drawn once and its picture held like any other.
            let repeats = match layered_here || fx_clip.is_some_and(|c| crate::effects::animated(&c.effects)) {
                true => 1,
                false => (done_here..span.len)
                    .take_while(|&offset| source_at(offset) == want)
                    .count() as u32,
            };
            // The composed picture of a layered span, owned for this frame.
            let mut composed: Option<Planes> = None;
            let mut composed16: Option<Planes16> = None;
            let picture = match (&mut layers, &mut pictures) {
                (Some(stack), _) if layered_wide => match stack.picture16(span.start + done_here)? {
                    Some(planes) => {
                        let planes = composed16.insert(planes);
                        Some(Frame::Wide(
                            &planes.0[..],
                            &planes.1[..],
                            &planes.2[..],
                            meta.width,
                            meta.height,
                        ))
                    }
                    None => None,
                },
                (Some(stack), _) => match stack.picture(span.start + done_here)? {
                    Some(planes) => {
                        let planes = composed.insert(planes);
                        Some(Frame::Pixels(
                            &planes.0[..],
                            &planes.1[..],
                            &planes.2[..],
                            meta.width,
                            meta.height,
                        ))
                    }
                    None => None,
                },
                (None, Some(pictures)) => {
                    let mut ran_out = false;
                    while taken < want && !ran_out {
                        ran_out = pictures.next()?.is_none();
                        taken += 1;
                    }
                    match ran_out {
                        true => None,
                        false => {
                            taken += 1;
                            pictures.next()?
                        }
                    }
                }
                (None, None) => {
                    let (y, u, v, width, height) = black.picture();
                    Some(Frame::Pixels(y, u, v, width, height))
                }
            };
            let Some(frame) = picture else {
                // The file ran out before its clip did: this span's tail never
                // gets a picture -- and *only* this span's tail, because the
                // span loop this break sits in carries on with the spans after
                // it and writes every picture they have. Counted, not
                // swallowed: the bar reaches its end whatever happens, so
                // without this a truncated file reads as a whole one. The same
                // sentence `planned_subtitles` gives cues that hang past the
                // last picture, said after the fact here because how many
                // pictures a *file* holds is only known by opening it.
                //
                // `span.len > done_here` holds by the loop's own condition, and
                // the sum over every break is exactly `total - done` at the end
                // of the walk: the spans are disjoint and cover the whole edit.
                skipped += span.len - done_here;
                break; // source ran out early; the clip list outlives the file
            };
            // A picture still on the GPU has nothing waiting for it here: the
            // span was opened that way only where none of what follows would
            // have touched a sample of it, so it is placed by nobody and goes
            // into the encoder as the buffer the decoder wrote.
            // A 10-bit picture goes to the encoder whole only where nothing on the
            // way touches a sample: no layer, dissolve, tone map, remap, grade,
            // transform, turn, stretch, caption or fade. Anything else composes at
            // 8 bits as it always did, narrowed here.
            let untouched = ten_bit
                && !layered_here
                && b.is_none()
                && mapper.is_none()
                && remap.is_none()
                && grade.is_none()
                && transform == TransformParams::default()
                && matches!(rotation, Rotation::None)
                && pixel_aspect.is_square()
                && burn
                    .as_ref()
                    .is_none_or(|b| b.cues.is_empty() && b.titles.is_empty())
                && (done_here..done_here + repeats)
                    .all(|off| picture_fade_at(project, span.start + off).is_none());
            // A 10-bit export composes at 10 bits wherever the picture is touched at
            // all. The exceptions stay at 8 bits: a tone map (HDR), a turn or a
            // stretch, and a layered picture (composed per clip).
            let wide16 = ten_bit
                && (layered_wide || !layered_here)
                && !untouched;
            let frame = match frame {
                Frame::Wide(_, _, _, width, height)
                    if !wide16 && !(untouched && width == meta.width && height == meta.height) =>
                {
                    narrowed(frame, &mut narrow_a)
                }
                other => other,
            };
            // The picture at 10 bits, composed: a 10-bit one as it is, an 8-bit one widened.
            let placed16 = match (wide16, &frame) {
                (true, Frame::Wide(y, u, v, width, height)) => Some(place_picture16(
                    y,
                    u,
                    v,
                    *width,
                    *height,
                    rotation,
                    pixel_aspect,
                    grade,
                    transform,
                    remap.filter(|_| !layered_here),
                    mapper,
                    &mut scratch16,
                    &mut canvas16,
                )),
                (true, Frame::Pixels(y, u, v, width, height)) => {
                    widen_into(y, &mut widened_a.0);
                    widen_into(u, &mut widened_a.1);
                    widen_into(v, &mut widened_a.2);
                    Some(place_picture16(
                        &widened_a.0[..],
                        &widened_a.1[..],
                        &widened_a.2[..],
                        *width,
                        *height,
                        rotation,
                        pixel_aspect,
                        grade,
                        transform,
                        remap.filter(|_| !layered_here),
                        mapper,
                        &mut scratch16,
                        &mut canvas16,
                    ))
                }
                _ => None,
            };
            let placed = match frame {
                Frame::Dma(_) => None,
                // Encoded whole below, at 10 bits.
                Frame::Wide(..) => None,
                // Composed at 10 bits above.
                Frame::Pixels(..) if wide16 => None,
                // A layered picture is composed already, per clip: nothing here
                // places it again.
                Frame::Pixels(y, u, v, width, height) if layered_here => {
                    Some((y, u, v, width, height))
                }
                Frame::Pixels(y, u, v, width, height) => Some(place_picture(
                    y,
                    u,
                    v,
                    width,
                    height,
                    rotation,
                    pixel_aspect,
                    grade,
                    transform,
                    remap,
                    mapper,
                    &mut rotated,
                    &mut stretched,
                    &mut graded,
                    &mut transformed,
                    &mut canvas,
                )),
            };
            // A lone clip's effects, drawn on its placed picture and laid over black.
            let (placed16, placed) = match fx_clip {
                None => (placed16, placed),
                Some(clip) => {
                    let secs = f64::from(span.start + done_here) / meta.frame_rate;
                    let src = crate::project::key_sample_pos(clip, span.start + done_here);
                    let placed16 = placed16.map(|(y, u, v, w, h)| {
                        fx16 = (y.to_vec(), u.to_vec(), v.to_vec());
                        effected(&clip.effects, &mut fx16, w, h, secs, src, out_color.matrix, &mut fx_scratch16);
                        (&fx16.0[..], &fx16.1[..], &fx16.2[..], w, h)
                    });
                    let placed = placed.map(|(y, u, v, w, h)| {
                        fx8 = (y.to_vec(), u.to_vec(), v.to_vec());
                        effected(&clip.effects, &mut fx8, w, h, secs, src, out_color.matrix, &mut fx_scratch8);
                        (&fx8.0[..], &fx8.1[..], &fx8.2[..], w, h)
                    });
                    (placed16, placed)
                }
            };
            // Once per timeline frame this source frame covers: one at real time
            // and faster, more when the clip is slowed -- the picture is already
            // graded and placed, so a held frame costs an encode and no decode.
            for r in 0..repeats {
                // Inside the window every *output* frame gets its own B
                // picture -- decoded fresh here rather than once for the
                // whole held A frame -- which is what makes a slowed A's
                // tail still ramp one weight per frame shown rather than one
                // per source frame.
                let in_window = b.as_ref().is_some_and(|d| done_here + r >= d.tail_start);
                let coded = match (&mut b, in_window) {
                    (Some(d), true) if placed16.is_some() => {
                        let (ay, au, av, aw, ah) = placed16.expect("checked by the guard");
                        let t = dissolve_weight(done_here + r - d.tail_start, d.window);
                        let blended: Option<Planes16> = match d.decoder.next()? {
                            // B's file ran out inside the window: A's own picture carries the frame.
                            None => None,
                            Some(b) => {
                                // B at 10 bits: a 10-bit picture as it is, an 8-bit one widened.
                                let (by, bu, bv, bw, bh): (&[u16], &[u16], &[u16], u32, u32) = match b {
                                    Frame::Wide(y, u, v, w, h) => (y, u, v, w, h),
                                    Frame::Pixels(y, u, v, w, h) => {
                                        widen_into(y, &mut widened_b.0);
                                        widen_into(u, &mut widened_b.1);
                                        widen_into(v, &mut widened_b.2);
                                        (&widened_b.0[..], &widened_b.1[..], &widened_b.2[..], w, h)
                                    }
                                    Frame::Dma(_) => unreachable!("b's decoder is never a DMA buffer"),
                                };
                                let (y, u, v, _, _) = place_picture16(
                                    by,
                                    bu,
                                    bv,
                                    bw,
                                    bh,
                                    d.rotation,
                                    d.pixel_aspect,
                                    d.grade,
                                    d.transform,
                                    d.remap,
                                    d.mapper,
                                    &mut d.scratch16,
                                    &mut d.canvas,
                                );
                                Some(crate::transitions::blend_planes(
                                    d.kind,
                                    (ay, au, av),
                                    (y, u, v),
                                    t,
                                    aw as usize,
                                    ah as usize,
                                ))
                            }
                        };
                        let frame_at = span.start + done_here + r;
                        let (y, u, v) = match &blended {
                            Some(p) => borrow_planes(p),
                            None => (ay, au, av),
                        };
                        let faded = picture_fade_at(project, frame_at).map(|k| fade_planes((y, u, v), k));
                        let (y, u, v) = faded.as_ref().map_or((y, u, v), borrow_planes);
                        let painted = burned16(&mut burn, frame_at, aw, ah, (y, u, v));
                        let (y, u, v) = painted.as_ref().map_or((y, u, v), borrow_planes);
                        encoder.encode_wide(y, u, v, aw, ah)?
                    }
                    (Some(d), true) => {
                        let (ay, au, av, aw, ah) =
                            placed.expect("a dissolving span always decodes to pixels");
                        match d.decoder.next()?.map(|f| narrowed(f, &mut narrow_b)) {
                            // B's file ran out inside the window: A's own
                            // picture carries the frame rather than the
                            // export losing it.
                            None => {
                                let frame_at = span.start + done_here + r;
                                let faded = picture_fade_at(project, frame_at)
                                    .map(|k| fade_planes((ay, au, av), k));
                                let (ay, au, av) =
                                    faded.as_ref().map_or((ay, au, av), borrow_planes);
                                let painted = burned(&mut burn, frame_at, aw, ah, (ay, au, av));
                                let (ay, au, av) = painted.as_ref().map_or((ay, au, av), borrow_planes);
                                encoder.encode(ay, au, av, aw, ah, settings.intra_only)?
                            }
                            Some(Frame::Dma(_) | Frame::Wide(..)) => {
                                unreachable!("b's decoder was opened with no DMA want and is narrowed")
                            }
                            Some(Frame::Pixels(by, bu, bv, bw, bh)) => {
                                let (by, bu, bv, _, _) = place_picture(
                                    by,
                                    bu,
                                    bv,
                                    bw,
                                    bh,
                                    d.rotation,
                                    d.pixel_aspect,
                                    d.grade,
                                    d.transform,
                                    d.remap,
                                    d.mapper,
                                    &mut d.rotated,
                                    &mut d.stretched,
                                    &mut d.graded,
                                    &mut d.transformed,
                                    &mut d.canvas,
                                );
                                let idx_in_window = done_here + r - d.tail_start;
                                let t = dissolve_weight(idx_in_window, d.window);
                                let (y, u, v) = crate::transitions::blend_planes(
                                    d.kind,
                                    (ay, au, av),
                                    (by, bu, bv),
                                    t,
                                    aw as usize,
                                    ah as usize,
                                );
                                let frame_at = span.start + done_here + r;
                                let faded = picture_fade_at(project, frame_at)
                                    .map(|k| fade_planes((&y[..], &u[..], &v[..]), k));
                                let (y, u, v) =
                                    faded.as_ref().map_or((&y[..], &u[..], &v[..]), borrow_planes);
                                let painted =
                                    burned(&mut burn, frame_at, aw, ah, (y, u, v));
                                let (y, u, v) =
                                    painted.as_ref().map_or((y, u, v), borrow_planes);
                                encoder.encode(y, u, v, aw, ah, settings.intra_only)?
                            }
                        }
                    }
                    _ => match (&frame, placed16) {
                        (_, Some((y, u, v, width, height))) => {
                            let frame_at = span.start + done_here + r;
                            // A layered picture's fades were applied per clip in its stack.
                            let faded = picture_fade_at(project, frame_at)
                                .filter(|_| !layered_here)
                                .map(|k| fade_planes((y, u, v), k));
                            let (y, u, v) = faded.as_ref().map_or((y, u, v), borrow_planes);
                            let painted = burned16(&mut burn, frame_at, width, height, (y, u, v));
                            let (y, u, v) = painted.as_ref().map_or((y, u, v), borrow_planes);
                            encoder.encode_wide(y, u, v, width, height)?
                        }
                        (Frame::Dma(dma), None) => encoder.encode_dma(dma, settings.intra_only)?,
                        (Frame::Wide(y, u, v, width, height), None) => {
                            encoder.encode_wide(y, u, v, *width, *height)?
                        }
                        (Frame::Pixels(..), None) => {
                            let (y, u, v, width, height) =
                                placed.expect("a picture with pixels was placed");
                            let frame_at = span.start + done_here + r;
                            // A layered picture's fades were applied per clip in
                            // its stack; the top clip's does not fade the frame.
                            let faded = picture_fade_at(project, frame_at)
                                .filter(|_| !layered_here)
                                .map(|k| fade_planes((y, u, v), k));
                            let (y, u, v) = faded.as_ref().map_or((y, u, v), borrow_planes);
                            let painted = burned(&mut burn, frame_at, width, height, (y, u, v));
                            let (y, u, v) = painted.as_ref().map_or((y, u, v), borrow_planes);
                            encoder.encode(y, u, v, width, height, settings.intra_only)?
                        }
                    },
                };
                if let Some((au, key)) = coded {
                    write_video(
                        &mut muxer,
                        out,
                        meta,
                        settings,
                        container_colour,
                        source_language.as_deref(),
                        audio_params.as_ref(),
                        &mut packets,
                        &mut subs,
                        au,
                        key,
                        ten_bit,
                    )?;
                    // Retried here rather than only where the mix lands: the
                    // very first coded picture is what creates `muxer`, and a
                    // mix that lands before it (a short film, or a slow first
                    // frame) has nothing to hand it to until now.
                    feed_late_audio(&mut muxer, &late, &mut audio_fed)?;
                    inject_export_test_panic(out);
                }
                done += 1;
                // Forwards only: an audio pass still running beside this one
                // publishes its own share of [`AUDIO_BAND`], and a bar that
                // went backwards every time it did would be worse than one
                // that stood still.
                shared
                    .progress
                    .fetch_max(picture_progress(done, total), Ordering::Relaxed);
            }
            done_here += repeats;
        }
    }
    // The whole walk is over, so what it could not write is known to the frame
    // and is published once. `fetch_max` and not `fetch_add`, because a
    // hardware seat that died has the *whole* walk run again from its first
    // frame ([`start`]): the same project and the same picture counts, so a
    // second run must not double a number that is already on the line.
    shared.truncated.fetch_max(skipped, Ordering::Relaxed);
    while let Some((au, key)) = encoder.drain()? {
        write_video(
            &mut muxer,
            out,
            meta,
            settings,
            container_colour,
            source_language.as_deref(),
            audio_params.as_ref(),
            &mut packets,
            &mut subs,
            au,
            key,
            ten_bit,
        )?;
    }
    // Progress reads 100% from here on, but nothing is published yet: draining,
    // the audio pass and `finish` are all still cancellable.
    cancelled(shared)?;

    let Some(muxer) = muxer else {
        return Err("export produced no coded pictures".into());
    };
    // The mp4's audio and subtitle tracks after its picture -- the Matroska one
    // interleaved its own as it went and left `packets` and `subs` empty behind
    // it. The text last, so its samples are written knowing how long the picture
    // turned out to be.
    let muxer = match muxer {
        Muxer::Mp4(mut mp4) => {
            // The sound, collected where this file first needs it: it has been
            // mixing and encoding under the whole picture, and an mp4 declares
            // its audio track here rather than at the open precisely so that
            // could happen ([`Mp4Muxer::add_audio_track`]).
            let track = match audio_job.take() {
                Some(job) => {
                    let track = join_audio(job)?;
                    // The picture finished first and this waited for the
                    // sound: the pair of stage lines is what says which of the
                    // two an export is really bounded by.
                    if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!(
                        "export audio: waited for after {:.1} s of picture",
                        picture_started.elapsed().as_secs_f64()
                    ); }
                    // ...and the line on screen finished with it. The loop above
                    // publishes the measured codec the moment the pass lands
                    // under the picture, but a picture that outran the mix never
                    // reaches that branch -- and "working out the sound…" would
                    // then be the last thing the card ever said about a track it
                    // had long since written.
                    publish_seats(
                        shared,
                        seat,
                        project,
                        settings,
                        track.as_ref().map(|track| track.copied),
                        track
                            .as_ref()
                            .is_some_and(|track| track.opus_pre_skip.is_some()),
                        false,
                    );
                    track
                }
                // `late` was already handed to `mp4` inside the picture loop
                // ([`feed_late_audio`]) unless the mix never landed there, in
                // which case it is still `None` and there was nothing to feed.
                None if audio_fed => None,
                None => late,
            };
            if let Some(track) = &track {
                mp4.add_audio_track(&params_of(track))?;
            }
            for packet in track.into_iter().flat_map(|track| track.packets) {
                mp4.write_audio_packet(&packet.bytes)?;
            }
            mp4.set_chapters(&settings.chapters)?;
            mp4.write_subtitles(&subs)?;
            Muxer::Mp4(mp4)
        }
        Muxer::Mov(mut mov) => {
            // The sound, PCM at the asked width, laid to the picture's length.
            if let Some(job) = pcm_job.take()
                && let Some((mut samples, rate, channels)) = join_pcm(job)?
            {
                let want = (f64::from(done) / meta.frame_rate * f64::from(rate)).round() as usize
                    * usize::from(channels.max(1));
                samples.resize(want, 0);
                mov.write_sound(&samples, rate, channels, prores_audio_bits(settings))?;
            }
            Muxer::Mov(mov)
        }
        muxer => muxer,
    };
    cancelled(shared)?;
    muxer.finish()?;
    shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed);
    Ok(())
}

/// The cues a burn of track `pick` draws, on this timeline's clock: what the
/// screen shows. Once a track is placed on a subtitle lane, its placements are
/// the answer -- a cut sentence is gone, a trimmed or moved one sits where it
/// was put, and a hidden lane draws nothing -- with each cue's words read from
/// its palette track, so a retyped cue burns retyped. A track no lane places is
/// drawn as the export writes it whole ([`timeline_cues`]), whatever else is
/// placed, so a burn and a soft track of the same pick never disagree.
///
/// `Err` for a pick that is not a track of the project.
pub fn burn_cues(project: &Project, pick: usize, fps: f64) -> crate::Result<Vec<Cue>> {
    let track = project
        .subtitles()
        .get(pick)
        .ok_or_else(|| format!("burn_subtitles: no subtitle track {pick} in this project"))?;
    if !placed_tracks(project).contains(&pick) {
        return Ok(timeline_cues(project, track, fps));
    }
    let mut cues: Vec<Cue> = project
        .subtitle_lanes()
        .into_iter()
        .flat_map(|lane| project.sub_lane_cue_spans(lane, fps))
        .filter(|s| s.track == pick)
        .filter_map(|s| {
            let c = track.cues.get(s.cue)?;
            Some(Cue {
                start_us: s.start_us,
                end_us: s.end_us,
                text: c.text.clone(),
                image: c.image.clone(),
                words: crate::subtitle::map_words(&c.words, s.window, (s.start_us, s.end_us)),
                karaoke: crate::subtitle::map_karaoke(&c.karaoke, s.window, (s.start_us, s.end_us)),
            })
        })
        .collect();
    cues.sort_by_key(|c| c.start_us);
    Ok(cues)
}

/// The index of the cue on screen at timeline frame `frame`, if one is: a cue
/// is shown from the instant of its start up to, not including, its end, read
/// off the frame's own start time the way [`timeline_cues`] times the frames.
/// Cues with no text (a picture track's, or a blank line) show nothing.
pub fn cue_on_screen(cues: &[Cue], frame: u32, fps: f64) -> Option<usize> {
    let t = (f64::from(frame) / fps * 1e6).round() as i64;
    cues.iter().position(|c| {
        c.image.is_none() && c.start_us <= t && t < c.end_us && !c.text.trim().is_empty()
    })
}

/// The indices of every cue on screen at timeline frame `frame`, in the order
/// `cues` lists them (start order from [`burn_cues`]). The same test as
/// [`cue_on_screen`], for a burn that draws all the cues up at once.
pub fn cues_on_screen(cues: &[Cue], frame: u32, fps: f64) -> Vec<usize> {
    let t = (f64::from(frame) / fps * 1e6).round() as i64;
    cues.iter()
        .enumerate()
        .filter(|(_, c)| {
            c.image.is_none() && c.start_us <= t && t < c.end_us && !c.text.trim().is_empty()
        })
        .map(|(i, _)| i)
        .collect()
}

/// Three planes of one picture, owned: what a burned-in frame is encoded from.
type Planes<T = u8> = (Vec<T>, Vec<T>, Vec<T>);

/// A picture at 10 bits a sample ([`Enc::encode_wide`]).
type Planes16 = Planes<u16>;

fn borrow_planes<T>(p: &Planes<T>) -> (&[T], &[T], &[T]) {
    (&p.0, &p.1, &p.2)
}

/// What one export draws into its pictures: the titles, then the captions
/// ([`ExportSettings::burn_subtitles`]). The overlay of the cue last drawn is
/// kept, so a caption on screen for a second is laid out once rather than once
/// a frame, and each title's overlay is kept the same way.
struct Burn {
    cues: Vec<Cue>,
    style: TextStyle,
    fps: f64,
    full_range: bool,
    /// The cues drawn last, by index, with the size, the animation state
    /// ([`crate::captionanim::blocks_key`]) and the stacked overlay.
    last: Option<(Vec<usize>, u32, u32, u64, Overlay)>,
    titles: Vec<Title>,
    /// One per title, the overlay it was last drawn as, with what made it ([`TitleKey`]).
    title_last: Vec<Option<(TitleKey, Overlay)>>,
}

/// What a title's overlay is made from: a frame's size, how much of its text shows and its
/// motion. A title that does not move keeps the same key, so it is laid out once.
#[derive(Clone, Debug, PartialEq)]
struct TitleKey {
    width: u32,
    height: u32,
    size: u32,
    dx: i32,
    dy: i32,
    /// The letters drawn, or `None` for all of them.
    shown: Option<usize>,
}

/// What a title draws at one frame, as a key: its size, where it is moved and how much of its
/// text shows, for a `width` x `height` picture. Preview and export both draw a title from this
/// same motion ([`Title::motion_at`]), so the animation reads the same in both.
fn title_key(title: &Title, motion: &Motion, width: u32, height: u32) -> TitleKey {
    let size = ((title.style.size as f32 * motion.scale).round() as u32)
        .clamp(crate::text::SIZE_RANGE.0, crate::text::SIZE_RANGE.1);
    TitleKey {
        width,
        height,
        size,
        dx: (motion.dx * width as f32).round() as i32,
        dy: (motion.dy * height as f32).round() as i32,
        shown: motion.revealed(&title.text).map(|s| s.ink),
    }
}

/// The overlay a title's key stands for: the whole text laid out in the key's size and moved,
/// with the letters past its reveal left out of the drawing but not out of the layout.
fn title_overlay(title: &Title, key: &TitleKey) -> Overlay {
    let style = TextStyle { size: key.size, ..title.style.clone() };
    let lines: Vec<String> = title.text.split('\n').map(str::to_string).collect();
    let mut ov = match key.shown {
        None => overlay(&style, key.width, key.height, &lines),
        Some(ink) => crate::text::overlay_revealed(&style, key.width, key.height, &lines, ink),
    };
    if (key.dx, key.dy) != (0, 0) {
        ov = ov.moved(key.dx, key.dy);
    }
    ov
}

impl Burn {
    /// What the settings and the timeline ask drawn in, or `None` for a picture
    /// left alone. A pick that is not a track of the project is named, not ignored.
    fn new(
        project: &Project,
        settings: &ExportSettings,
        fps: f64,
        full_range: bool,
    ) -> crate::Result<Option<Self>> {
        let titles = project.titles().to_vec();
        let (cues, style) = match &settings.burn_subtitles {
            Some((pick, style)) => {
                (burn_cues(project, *pick, fps)?, style.clamped())
            }
            None => (Vec::new(), TextStyle::default()),
        };
        if cues.is_empty() && titles.is_empty() && settings.burn_subtitles.is_none() {
            return Ok(None);
        }
        Ok(Some(Self {
            cues,
            style,
            fps,
            full_range,
            last: None,
            title_last: vec![None; titles.len()],
            titles,
        }))
    }

    /// The picture at timeline frame `frame` with the titles on screen and the
    /// caption drawn in, or `None` where nothing is on screen and the planes go
    /// on untouched.
    fn paint(
        &mut self,
        frame: u32,
        width: u32,
        height: u32,
        planes: (&[u8], &[u8], &[u8]),
    ) -> Option<Planes> {
        let mut out: Option<Planes> = None;
        for (i, t) in self.titles.iter().enumerate() {
            let Some(motion) = t.motion_at(frame) else {
                continue;
            };
            let alpha = motion.alpha;
            if alpha == 0 {
                continue;
            }
            let key = title_key(t, &motion, width, height);
            if !matches!(&self.title_last[i], Some((k, _)) if *k == key) {
                let ov = title_overlay(t, &key);
                self.title_last[i] = Some((key, ov));
            }
            let Some((_, ov)) = &self.title_last[i] else {
                continue;
            };
            let mut planes = out
                .take()
                .unwrap_or_else(|| (planes.0.to_vec(), planes.1.to_vec(), planes.2.to_vec()));
            if alpha == 255 {
                ov.blend_i420(&mut planes.0, &mut planes.1, &mut planes.2, self.full_range);
            } else {
                ov.faded(alpha).blend_i420(
                    &mut planes.0,
                    &mut planes.1,
                    &mut planes.2,
                    self.full_range,
                );
            }
            out = Some(planes);
        }
        let on = cues_on_screen(&self.cues, frame, self.fps);
        if !on.is_empty() {
            // A static style is keyed by the cues alone, so a cache hit builds nothing.
            // A karaoke cue moves with the clock whatever the style, so it is planned too.
            let lit = on.iter().any(|&i| self.cues[i].real_karaoke().is_some());
            let planned = (!self.style.caption.is_plain() || lit)
                .then(|| crate::captionanim::plan(&self.style, &self.cues, &on, frame, self.fps));
            let key = planned.as_deref().map_or(0, crate::captionanim::blocks_key);
            let hit = matches!(&self.last, Some((i, w, h, k, _))
                if i == &on && (*w, *h) == (width, height) && *k == key);
            if !hit {
                let blocks = planned.unwrap_or_else(|| {
                    crate::captionanim::plan(&self.style, &self.cues, &on, frame, self.fps)
                });
                let ov = overlay_blocks(&self.style, width, height, &blocks);
                self.last = Some((on, width, height, key, ov));
            }
            if let Some((_, _, _, _, ov)) = self.last.as_ref() {
                let mut planes = out
                    .take()
                    .unwrap_or_else(|| (planes.0.to_vec(), planes.1.to_vec(), planes.2.to_vec()));
                ov.blend_i420(&mut planes.0, &mut planes.1, &mut planes.2, self.full_range);
                out = Some(planes);
            }
        }
        out
    }
}

/// [`Burn::paint`] through an optional burn: the picture as it is when there is
/// no burn, and as it is with the caption drawn in when there is one.
fn burned(
    burn: &mut Option<Burn>,
    frame: u32,
    width: u32,
    height: u32,
    planes: (&[u8], &[u8], &[u8]),
) -> Option<Planes> {
    burn.as_mut()?.paint(frame, width, height, planes)
}

/// A lone clip's effects stack drawn on its placed `w` x `h` picture in place, and
/// the result laid over black where the stack keys, masks or blends it -- what the
/// layered path draws for the same clip with nothing beneath ([`LayerStack::picture`]).
#[allow(clippy::too_many_arguments)]
fn effected<T: Sample>(
    effects: &[crate::effects::Effect],
    planes: &mut Planes<T>,
    w: u32,
    h: u32,
    secs: f64,
    src: f64,
    matrix: Matrix,
    scratch: &mut crate::effects::Scratch<T>,
) {
    let (w, h) = (w as usize, h as usize);
    let out = crate::effects::apply(effects, (&mut planes.0, &mut planes.1, &mut planes.2), w, h, secs, src, matrix, scratch);
    crate::effects::over_black((&mut planes.0, &mut planes.1, &mut planes.2), &out, matrix, w, h);
}

/// One 8-bit code at 10 bits, as the encoder upshifts it (`v << 2 | v >> 6`), so a
/// widened black is the black of the 10-bit span.
fn widen_code(v: u8) -> u16 {
    (u16::from(v) << 2) | (u16::from(v) >> 6)
}

/// [`widen_code`] over a plane, into `out`.
fn widen_into(src: &[u8], out: &mut Vec<u16>) {
    out.clear();
    out.extend(src.iter().map(|&v| widen_code(v)));
}

/// [`burned`] at 10 bits. A caption or a title is an 8-bit overlay, so the picture is
/// narrowed for it; where the overlay changed a code, that code is written back
/// widened, and every other code keeps its 10-bit value.
fn burned16(
    burn: &mut Option<Burn>,
    frame: u32,
    width: u32,
    height: u32,
    planes: (&[u16], &[u16], &[u16]),
) -> Option<Planes16> {
    let burn = burn.as_mut()?;
    let narrow = |s: &[u16]| -> Vec<u8> { s.iter().map(|&c| (c >> 2) as u8).collect() };
    let (ny, nu, nv) = (narrow(planes.0), narrow(planes.1), narrow(planes.2));
    let painted = burn.paint(frame, width, height, (&ny[..], &nu[..], &nv[..]))?;
    let merge = |orig: &[u16], before: &[u8], after: &[u8]| -> Vec<u16> {
        orig.iter()
            .zip(before.iter().zip(after))
            .map(|(&o, (&b, &a))| if a == b { o } else { widen_code(a) })
            .collect()
    };
    Some((
        merge(planes.0, &ny, &painted.0[..]),
        merge(planes.1, &nu, &painted.1[..]),
        merge(planes.2, &nv, &painted.2[..]),
    ))
}

/// The 10-bit placement's scratch: the turn, the pixel-aspect stretch, the grade and the
/// transform each want a buffer the planes come back borrowed from.
#[derive(Default)]
struct Scratch16 {
    turned: Planes16,
    stretched: Planes16,
    graded: Planes16,
    transformed: Planes16,
}

/// [`place_picture`] at 10 bits: the pixel aspect, the display turn, the tone map, the grade,
/// the matrix and range remap and the transform, all on 10-bit samples, in the 8-bit order.
#[expect(clippy::too_many_arguments, reason = "one span's whole 10-bit picture path")]
fn place_picture16<'a>(
    y: &'a [u16],
    u: &'a [u16],
    v: &'a [u16],
    width: u32,
    height: u32,
    rotation: Rotation,
    pixel_aspect: crate::demux::PixelAspect,
    grade: Option<crate::color::ColorParams>,
    transform: TransformParams,
    remap: Option<Remap>,
    mapper: Option<&ToneMapper>,
    scratch: &'a mut Scratch16,
    canvas: &'a mut Composer,
) -> (&'a [u16], &'a [u16], &'a [u16], u32, u32) {
    let Scratch16 { turned, stretched, graded, transformed } = scratch;
    let (y, u, v, width, height) = if pixel_aspect.is_square() {
        (y, u, v, width, height)
    } else {
        let display_width = pixel_aspect.width_factor(width);
        let (dcw, dch) = (display_width.div_ceil(2) as usize, height.div_ceil(2) as usize);
        stretched.0.resize(display_width as usize * height as usize, 0);
        stretched.1.resize(dcw * dch, 0);
        stretched.2.resize(dcw * dch, 0);
        crate::scale::scale_i420(
            y,
            u,
            v,
            width as usize,
            height as usize,
            &mut stretched.0,
            &mut stretched.1,
            &mut stretched.2,
            display_width as usize,
            height as usize,
        );
        (&stretched.0[..], &stretched.1[..], &stretched.2[..], display_width, height)
    };
    let (y, u, v, width, height) = match rotation.is_none() {
        true => (y, u, v, width, height),
        false => {
            let (w, h) = crate::scale::rotate_i420_90s_into(y, u, v, width, height, rotation.steps(), turned);
            (&turned.0[..], &turned.1[..], &turned.2[..], w, h)
        }
    };
    let params = grade.filter(|p| !p.is_identity());
    let (y, u, v) = match (params, remap, mapper) {
        (None, None, None) => (y, u, v),
        (params, remap, mapper) => {
            let (gy, gu, gv) = graded;
            gy.clear();
            gy.extend_from_slice(y);
            gu.clear();
            gu.extend_from_slice(u);
            gv.clear();
            gv.extend_from_slice(v);
            if let Some(mapper) = mapper {
                mapper.map16(gy, gu, gv, width as usize, height as usize);
            }
            if let Some(params) = params {
                crate::color::apply_yuv_wide(&params, gy, gu, gv, width as usize, remap.is_some_and(|r| r.limit));
            }
            if let Some(remap) = remap {
                if remap.limit {
                    crate::colorspace::limit_range::<u16>(gy, gu, gv);
                }
                crate::colorspace::remap::<u16>(remap.from, remap.to, gy, gu, gv, width as usize);
            }
            (&gy[..], &gu[..], &gv[..])
        }
    };
    if transform.is_identity() {
        canvas.place_wide(y, u, v, width, height)
    } else {
        let (py, pu, pv, w, h) = canvas.place_transformed_wide(y, u, v, width, height, &transform);
        *transformed = (py, pu, pv);
        (&transformed.0[..], &transformed.1[..], &transformed.2[..], w, h)
    }
}

/// One decoded picture turned, graded, tone-mapped, remapped and placed on the
/// canvas -- everything between a source's own samples and the ones an encoder
/// is fed, which is exactly the work a zero-copy span has none of.
///
/// `rotation` first, before the grade: it is the file's own display matrix, the
/// turn a player would make before showing the picture at all, and everything
/// after this -- the grade, the letterbox, the encoder -- is about the picture
/// as it is *meant* to be seen. `turned`, `graded` and `canvas` are the span's
/// scratch: the planes come back borrowed from one of them, or from the picture
/// itself where not a byte was touched.
#[expect(clippy::too_many_arguments, reason = "one span's whole picture path")]
fn place_picture<'a>(
    y: &'a [u8],
    u: &'a [u8],
    v: &'a [u8],
    width: u32,
    height: u32,
    rotation: Rotation,
    pixel_aspect: crate::demux::PixelAspect,
    grade: Option<crate::color::ColorParams>,
    transform: TransformParams,
    remap: Option<Remap>,
    mapper: Option<&ToneMapper>,
    turned: &'a mut (Vec<u8>, Vec<u8>, Vec<u8>),
    stretched: &'a mut (Vec<u8>, Vec<u8>, Vec<u8>),
    graded: &'a mut (Vec<u8>, Vec<u8>, Vec<u8>),
    transformed: &'a mut (Vec<u8>, Vec<u8>, Vec<u8>),
    canvas: &'a mut Composer,
) -> (&'a [u8], &'a [u8], &'a [u8], u32, u32) {
    // The pixel aspect, applied exactly once -- the same single conversion to
    // the square-pixel raster the decode funnel makes, in the same order
    // (stretch the coded width, *then* turn, [`crate::decode::Render::frame`]'s
    // comment carries the 1440x1440 failure the other order produces). A
    // square-pixel file -- every file whose samples are one to one -- borrows
    // the planes through.
    let (y, u, v, width, height) = if pixel_aspect.is_square() {
        (y, u, v, width, height)
    } else {
        let display_width = pixel_aspect.width_factor(width);
        let (dcw, dch) = (
            display_width.div_ceil(2) as usize,
            height.div_ceil(2) as usize,
        );
        stretched
            .0
            .resize(display_width as usize * height as usize, 0);
        stretched.1.resize(dcw * dch, 0);
        stretched.2.resize(dcw * dch, 0);
        crate::scale::scale_i420(
            y,
            u,
            v,
            width as usize,
            height as usize,
            &mut stretched.0,
            &mut stretched.1,
            &mut stretched.2,
            display_width as usize,
            height as usize,
        );
        (
            &stretched.0[..],
            &stretched.1[..],
            &stretched.2[..],
            display_width,
            height,
        )
    };
    // The file's own turn, taken exactly as playback's decode funnel takes it
    // ([`crate::decode`]): a phone's portrait video is landscape in the file,
    // and the encoder must be fed what the preview showed. A file that states
    // no turn skips this entirely -- the planes go on borrowed.
    let (y, u, v, width, height) = match rotation.is_none() {
        true => (y, u, v, width, height),
        false => {
            let (w, h) = crate::scale::rotate_i420_90s_into(
                y,
                u,
                v,
                width,
                height,
                rotation.steps(),
                turned,
            );
            (&turned.0[..], &turned.1[..], &turned.2[..], w, h)
        }
    };
    // The planes are borrowed from the decoder (and `Black::picture` hands the
    // same slice as both u and v), so a grade cannot be applied in place: it
    // goes onto a copy, which the encoder then reads instead.
    let params = grade.filter(|p| !p.is_identity());
    let (y, u, v) = match (params, remap, mapper) {
        (None, None, None) => (y, u, v),
        (params, remap, mapper) => {
            let (gy, gu, gv) = graded;
            gy.clear();
            gy.extend_from_slice(y);
            gu.clear();
            gu.extend_from_slice(u);
            gv.clear();
            gv.extend_from_slice(v);
            // Tone-map first, so the grade is applied to the SDR picture the
            // canvas showed and not to HDR codes; then remap the graded result.
            // The order playback renders in, and the only order in which
            // preview and export agree.
            if let Some(mapper) = mapper {
                mapper.map(gy, gu, gv, width as usize, height as usize);
            }
            if let Some(params) = params {
                crate::color::apply_yuv(&params, gy, gu, gv, width as usize, remap.is_some_and(|r| r.limit));
            }
            if let Some(remap) = remap {
                // Range before matrix: the matrix arithmetic is written for limited
                // codes, so a full-range picture is brought into that span first.
                if remap.limit {
                    crate::colorspace::limit_range(gy, gu, gv);
                }
                crate::colorspace::remap(remap.from, remap.to, gy, gu, gv, width as usize);
            }
            (&gy[..], &gu[..], &gv[..])
        }
    };
    // Grade first, place second: the grade is the clip's own pixels and the
    // bars around them are not the clip (see `scale::Composer`).
    if transform.is_identity() {
        canvas.place(y, u, v, width, height)
    } else {
        let (py, pu, pv, w, h) = canvas.place_transformed(y, u, v, width, height, &transform);
        *transformed = (py, pu, pv);
        (
            &transformed.0[..],
            &transformed.1[..],
            &transformed.2[..],
            w,
            h,
        )
    }
}

/// How a span's samples are rewritten on the way into a file whose own matrix is
/// `to` and whose range is limited ([`ColorDescription::output`]): the matrix they
/// are coded against, and whether they span the full byte and so need scaling
/// into the limited span first ([`crate::colorspace::limit_range`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Remap {
    from: Matrix,
    to: Matrix,
    limit: bool,
}

/// What [`place_picture`] rewrites a span's samples by -- `None` when they are
/// already coded against the output's matrix and span its range, and for a gap,
/// whose 16/128/128 black is black in every matrix here.
///
/// `toned` says the tone map ran on this span: it hands back BT.709 SDR whatever
/// the source was coded as, so an HDR clip is never *also* given the matrix
/// treatment its BT.2020 tag would ask for -- one conversion or the other, never
/// both on one frame -- while a tone-mapped clip on a canvas below 720 lines
/// still takes its 709 -> 601 step. It is also limited range already, so only an
/// untouched full-range source is scaled here.
fn remap_into(color: Option<ColorDescription>, toned: bool, out: Matrix) -> Option<Remap> {
    let from = match toned {
        true => Matrix::Bt709,
        false => color?.matrix,
    };
    let limit = !toned && color?.full_range;
    (from != out || limit).then_some(Remap {
        from,
        to: out,
        limit,
    })
}

/// The picture at timeline `frame`, composed the way [`run`] composes it: the
/// same span and source frame, placed by [`place_picture`] (turn, pixel aspect,
/// grade, transform, tone map, fit), dissolved into its successor over the
/// tail window, and written as the BGRA picture the encoder would have been
/// given. A gap is black. Visualizer and still clips take the same decoders
/// export does.
///
/// The output is the project's resolution scaled to fit a `width` x `height`
/// box at the project's aspect (so a 16:9 project asked for 640x480 comes back
/// 640x360). Each call opens its own decoder for the one source frame it needs,
/// so it costs a decoder start; it is for the frame an agent asks for, not a
/// sequence.
pub fn compose_at(
    project: &Project,
    meta: &VideoMeta,
    frame: u32,
    width: u32,
    height: u32,
) -> crate::Result<crate::decode::Frame> {
    compose_at_with(project, meta, frame, width, height, None)
}

/// [`compose_at`] with every clip's grade taken off (its colour, its keys and
/// its LUT): the picture as shot, which is the preview's "before" side and the
/// frame Auto reads. A copy of the timeline is graded-off, so `project` is left
/// as it is; nothing here is exported. Sequences a compound clip plays keep
/// their own grades.
pub fn compose_ungraded_at(project: &Project, meta: &VideoMeta, frame: u32, width: u32, height: u32) -> crate::Result<crate::decode::Frame> {
    let mut plain = project.clone();
    for lane in plain.lanes() {
        for idx in 0..plain.lane(lane).len() {
            if !plain.color_keys_of(lane, idx).is_empty() {
                plain.set_color_keys(lane, idx, Vec::new());
            }
            plain.set_color(lane, idx, None);
        }
    }
    compose_at(&plain, meta, frame, width, height)
}

/// [`compose_at`] with captions drawn in: `burn` is the subtitle track's index
/// in [`Project::subtitles`] and the style, as [`ExportSettings::burn_subtitles`]
/// takes them, and the cue on screen at `frame` is drawn over the output at its
/// own size. `None` is exactly [`compose_at`].
pub fn compose_at_with(
    project: &Project,
    meta: &VideoMeta,
    frame: u32,
    width: u32,
    height: u32,
    burn: Option<(usize, TextStyle)>,
) -> crate::Result<crate::decode::Frame> {
    let end = project.media_frames();
    if frame >= end {
        return Err(format!("frame {frame} is past the end of the timeline ({end} frames)").into());
    }
    if width == 0 || height == 0 {
        return Err("compose_at: the output size must be non-zero".into());
    }
    let out = ColorDescription::output_sized(meta.width, meta.height);
    let span = export_spans(project, None)
        .into_iter()
        .find(|s| s.start <= frame && frame - s.start < s.len);
    let layered = layered_at(project, frame);
    let (y, u, v) = match span {
        // Two or more clips cover the frame: each is decoded at this frame and
        // composed bottom to top, exactly as the export composes it.
        _ if layered => {
            let stack = stack_at(project, frame);
            let mut layers = LayerStack::open(project, meta, &stack, out.matrix, None, None)?;
            layers
                .picture(frame)?
                .ok_or_else(|| -> crate::Error {
                    format!("compose_at: a clip ends before frame {frame}").into()
                })?
        }
        Some(s) if s.from.is_some() => {
            let (source, in_frame) = s.from.expect("checked above");
            let offset = frame - s.start;
            let mut a = placed_info(project, meta, source)?;
            let deint = span_deint(project, s.start, &a.path);
            a.rate = crate::deinterlace::counted_rate(deint, a.rate);
            let source_frame = a.rate.source_at(s.source_at(in_frame, offset));
            let (ay, au, av) = picture_at(project, meta, &a, source_frame, s.start, out.matrix, deint)?;
            match dissolve_window(project, &s) {
                Some((b_source, b_in, window, b_start))
                    if offset >= s.len.saturating_sub(window) =>
                {
                    let k = offset - (s.len - window);
                    let mut b = placed_info(project, meta, b_source)?;
                    let b_deint = frame_counting(span_deint(project, b_start, &b.path));
                    b.rate = crate::deinterlace::counted_rate(b_deint, b.rate);
                    let b_frame = b.rate.source_at(b_in) + k;
                    let (by, bu, bv) = picture_at(project, meta, &b, b_frame, b_start, out.matrix, b_deint)?;
                    crate::transitions::blend_planes(
                        crate::transitions::kind_at(project, s.start),
                        (&ay, &au, &av),
                        (&by, &bu, &bv),
                        dissolve_weight(k, window),
                        meta.width as usize,
                        meta.height as usize,
                    )
                }
                _ => (ay, au, av),
            }
        }
        _ => {
            let (w, h) = (meta.width as usize, meta.height as usize);
            (
                vec![16; w * h],
                vec![128; w.div_ceil(2) * h.div_ceil(2)],
                vec![128; w.div_ceil(2) * h.div_ceil(2)],
            )
        }
    };
    // A layered picture's fades were applied per clip inside its stack.
    let (y, u, v) = match picture_fade_at(project, frame).filter(|_| !layered) {
        Some(k) => fade_planes((&y, &u, &v), k),
        None => (y, u, v),
    };
    let bgra =
        crate::convert::i420_to_bgra(&out, &y, &u, &v, meta.width as usize, meta.height as usize);
    let (tw, th) = fit_box(meta.width, meta.height, width, height);
    let mut bgra = if (tw, th) == (meta.width, meta.height) {
        bgra
    } else {
        resize_bgra(&bgra, meta.width, meta.height, tw, th)
    };
    // The titles, then the caption, drawn at the size the caller gets, so their
    // size reads the same whatever the output box was asked for.
    for title in project.titles() {
        let Some(motion) = title.motion_at(frame) else {
            continue;
        };
        let alpha = motion.alpha;
        if alpha == 0 {
            continue;
        }
        let ov = title_overlay(title, &title_key(title, &motion, tw, th));
        if alpha == 255 {
            ov.blend_bgra(&mut bgra);
        } else {
            ov.faded(alpha).blend_bgra(&mut bgra);
        }
    }
    if let Some((pick, style)) = burn {
        let cues = burn_cues(project, pick, meta.frame_rate)?;
        let on = cues_on_screen(&cues, frame, meta.frame_rate);
        if !on.is_empty() {
            let style = style.clamped();
            let blocks = crate::captionanim::plan(&style, &cues, &on, frame, meta.frame_rate);
            overlay_blocks(&style, tw, th, &blocks).blend_bgra(&mut bgra);
        }
    }
    Ok(crate::decode::Frame {
        index: frame,
        width: tw,
        height: th,
        bgra,
    })
}

/// A source as [`run`] reads it for placement: its path and audio stream, its
/// rate against the timeline, its colour, the tone map its curve asks for, and
/// its display-matrix turn and pixel aspect.
struct Placed {
    path: PathBuf,
    stream: usize,
    rate: Rate,
    color: ColorDescription,
    mapper: Option<ToneMapper>,
    rotation: Rotation,
    pixel_aspect: crate::demux::PixelAspect,
}

/// The deinterlace setting of the clip the composite shows at `frame`, as it
/// applies to `path` (`Frame` for anything but interlaced H.264).
fn span_deint(project: &Project, frame: u32, path: &Path) -> Deinterlace {
    let mode = project
        .composite_clip_at(frame)
        .map_or(Deinterlace::Frame, |(lane, idx)| project.deinterlace_of(lane, idx));
    crate::deinterlace::effective(mode, path)
}

/// `Field` counts two pictures a frame, which a picture-per-timeline-frame reader
/// cannot follow: it takes `Frame` (the same first field at the file's own rate).
fn frame_counting(mode: Deinterlace) -> Deinterlace {
    if mode == Deinterlace::Field { Deinterlace::Frame } else { mode }
}

fn placed_info(project: &Project, meta: &VideoMeta, source: usize) -> crate::Result<Placed> {
    let sources = project.sources();
    let entry = sources
        .get(source)
        .ok_or_else(|| format!("clip names source {source} of {}", sources.len()))?;
    let (rate, color, peak, rotation, pixel_aspect) = source_rate(&entry.path, meta.frame_rate);
    let mapper = match color.transfer {
        Transfer::Sdr => None,
        Transfer::Pq => Some(ToneMapper::new(tonemap::Transfer::Pq, project.tone(), peak)),
        Transfer::Hlg => Some(ToneMapper::new(
            tonemap::Transfer::Hlg,
            project.tone(),
            peak,
        )),
    };
    Ok(Placed {
        path: entry.path.clone(),
        stream: entry.audio_stream,
        rate,
        color,
        mapper,
        rotation,
        pixel_aspect,
    })
}

/// One source picture at `source_frame`, placed as [`run`] places it, as owned
/// planes at the project's size. `key` is the timeline frame whose grade,
/// transform, fit and visualizer state apply.
fn picture_at(
    project: &Project,
    meta: &VideoMeta,
    info: &Placed,
    source_frame: u32,
    key: u32,
    out: Matrix,
    deint: Deinterlace,
) -> crate::Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let flags = project.composite_visualizer_at(key);
    let mut decoder = if crate::is_audio(&info.path) || flags & 1 != 0 {
        ClipDecoder::visualizer(
            &info.path,
            info.stream,
            source_frame,
            (meta.width, meta.height),
            meta.frame_rate,
            flags,
            project.composite_viz_paint_at(key),
        )?
    } else {
        ClipDecoder::open_with(&info.path, source_frame, None, None, false, deint)?
    };
    let Some(picture) = decoder.next()? else {
        return Err(format!("{} ends before frame {source_frame}", info.path.display()).into());
    };
    let Frame::Pixels(y, u, v, width, height) = picture else {
        return Err("a single-frame decode produced no pixels".into());
    };
    let remap = remap_into(Some(info.color), info.mapper.is_some(), out);
    let grade = project.composite_color_at(key).copied();
    let transform = project
        .composite_transform_at(key)
        .copied()
        .unwrap_or_default();
    let mut canvas = Composer::new(meta.width, meta.height, project.composite_fit_at(key));
    let (mut rotated, mut stretched, mut graded, mut transformed) = Default::default();
    let (py, pu, pv, _, _) = place_picture(
        y,
        u,
        v,
        width,
        height,
        info.rotation,
        info.pixel_aspect,
        grade,
        transform,
        remap,
        info.mapper.as_ref(),
        &mut rotated,
        &mut stretched,
        &mut graded,
        &mut transformed,
        &mut canvas,
    );
    Ok((py.to_vec(), pu.to_vec(), pv.to_vec()))
}

/// The largest size with the project's `w:h` aspect that fits in `bw` x `bh`.
fn fit_box(w: u32, h: u32, bw: u32, bh: u32) -> (u32, u32) {
    let (w, h, bw, bh) = (w as u64, h as u64, bw as u64, bh as u64);
    let th = (bw * h / w).max(1);
    if th <= bh {
        (bw as u32, th as u32)
    } else {
        let tw = (bh * w / h).max(1);
        (tw as u32, bh as u32)
    }
}

/// Box-filter resize of a tightly packed BGRA picture. Each destination pixel
/// averages the source pixels it covers, so an upscale is nearest-neighbour
/// and a downscale is an area average; no float, so it is byte-stable.
fn resize_bgra(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    fn cover(i: usize, src: usize, dst: usize) -> (usize, usize) {
        let a = i * src / dst;
        let b = ((i + 1) * src / dst).max(a + 1).min(src);
        (a, b)
    }
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        let (y0, y1) = cover(dy, sh, dh);
        for dx in 0..dw {
            let (x0, x1) = cover(dx, sw, dw);
            let mut acc = [0u32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = (y * sw + x) * 4;
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += src[p + c] as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let q = (dy * dw + dx) * 4;
            for (c, a) in acc.iter().enumerate() {
                out[q + c] = ((a + n / 2) / n) as u8;
            }
        }
    }
    out
}

/// What rate `path` was shot at against a timeline at `timeline_fps`
/// ([`Rate`]). [`Rate::REAL_TIME`] for the files that have no rate of their own
/// -- a still, a song -- and for one that will not open here: the decoder is
/// opened a few lines later and fails the export by name, which is a better
/// error than this one could raise.
fn source_rate(
    path: &Path,
    timeline_fps: f64,
) -> (
    Rate,
    ColorDescription,
    Option<f32>,
    Rotation,
    crate::demux::PixelAspect,
) {
    if crate::is_image(path) || crate::is_audio(path) {
        // A still is BT.601 by construction whatever it was authored as:
        // `decode::rgb_to_i420` is the one matrix that turns its pixels into
        // planes. A song has no picture and the answer is never read. Neither
        // has a display matrix to be turned by, nor an aspect to be widened
        // by: both are authored square. A still's *EXIF* turn is not in this
        // arm either, and for a better reason -- `decode::Still::open` already
        // made it before the planes were written, so `place_picture` is handed
        // the picture the preview showed and must not be told to turn it again.
        return (
            Rate::REAL_TIME,
            ColorDescription::default(),
            None,
            Rotation::None,
            crate::demux::PixelAspect::SQUARE,
        );
    }
    match Demuxer::open(path) {
        // ...and one whose rate cannot be named against the timeline's, which
        // `matches_timeline` refuses at import, so nothing on a timeline is on
        // this arm either.
        //
        // The declared peak comes off the same open the space does, which is
        // what keeps export on the number playback's decode funnel read: one
        // header parse, both answers.
        Ok((meta, demuxer)) => (
            Rate::from_fps(meta.frame_rate, timeline_fps).unwrap_or(Rate::REAL_TIME),
            meta.color,
            demuxer.light().peak(),
            // The file's own turn, so the encoder is fed the picture the
            // preview shows: the same header read, one more answer.
            meta.rotation,
            // ...and its own sample aspect, for the same reason on the same
            // read: the encoder is fed the shape the preview showed.
            meta.pixel_aspect,
        ),
        // Unreadable here means unreadable below too, where the span dies with a
        // real message; the file's own space is the least of that.
        Err(_) => (
            Rate::REAL_TIME,
            ColorDescription::default(),
            None,
            Rotation::None,
            crate::demux::PixelAspect::SQUARE,
        ),
    }
}

/// The audio lanes alone, summed, as a WAV or a FLAC.
///
/// Nothing here re-derives the edit: the play lists are
/// [`Project::audio_segments_from`] and they are handed to the very
/// [`AudioSession::open_mixed_streams`] playback feeds from, so what is written
/// is what was heard -- every lane, mixed the same way, gaps included, which
/// arrive as real silence from that choke point rather than as a hole this would
/// have to invent.
///
/// The length is the *timeline's*: each segment resolves its window to whole
/// samples on its own, so the sum can miss the timeline by a sample or two, and
/// a source that runs out early would leave the file short. Both are settled by
/// one `resize` at the end -- padding with silence, trimming what overshot --
/// so an exported file is exactly as long as what was edited.
/// The timeline frames a picture export covers: its range, clipped to the media, or the whole media.
fn picture_span(project: &Project, settings: &ExportSettings) -> (u32, u32) {
    let media = project.media_frames();
    match settings.range {
        Some((from, to)) => {
            let from = from.min(media);
            (from, to.min(media).max(from))
        }
        None => (0, media),
    }
}

/// The picture at timeline frame `frame` as a PNG file's bytes, at the project's own size. What
/// the still formats write: [`compose_at_with`] draws it as the export would, titles and burnt
/// captions included.
fn still_png(project: &Project, meta: &VideoMeta, frame: u32, settings: &ExportSettings) -> crate::Result<Vec<u8>> {
    let picture = compose_at_with(project, meta, frame, meta.width, meta.height, settings.burn_subtitles.clone())?;
    crate::decode::to_png_bytes(&picture)
}

/// A numbered PNG run ([`Format::PngSequence`]) into the folder `out`, one file per frame,
/// named after the folder: `name_00001.png`. A run that fails part way leaves no files behind.
fn run_png_sequence(
    project: &Project,
    meta: &VideoMeta,
    out: &Path,
    shared: &Arc<Shared>,
    settings: &ExportSettings,
) -> crate::Result<()> {
    let (from, to) = picture_span(project, settings);
    let total = to - from;
    std::fs::create_dir_all(out)?;
    let name = out.file_name().and_then(|n| n.to_str()).unwrap_or("frame").to_string();
    let mut written: Vec<PathBuf> = Vec::new();
    let result = (|| -> crate::Result<()> {
        for k in 0..total {
            cancelled(shared)?;
            let path = out.join(format!("{name}_{:05}.png", k + 1));
            std::fs::write(&path, still_png(project, meta, from + k, settings)?)?;
            written.push(path);
            shared.progress.fetch_max(picture_progress(k + 1, total), Ordering::Relaxed);
        }
        Ok(())
    })();
    if result.is_err() {
        for path in &written {
            let _ = std::fs::remove_file(path);
        }
    }
    result
}

/// The poster frame ([`Format::Still`]): the picture at the start of the range, written to `part`
/// for the caller to publish.
fn run_still(project: &Project, meta: &VideoMeta, part: &Path, settings: &ExportSettings) -> crate::Result<()> {
    let frame = settings.range.map_or(0, |(from, _)| from);
    std::fs::write(part, still_png(project, meta, frame, settings)?)?;
    Ok(())
}

/// The animated GIF ([`Format::Gif`]): the range sampled at [`GifSettings::fps`], each picture
/// composed at [`GifSettings::width`], handed to the GIF writer one frame at a time.
fn run_gif(
    project: &Project,
    meta: &VideoMeta,
    part: &Path,
    shared: &Arc<Shared>,
    settings: &ExportSettings,
) -> crate::Result<()> {
    let gif = settings.gif;
    // The encoder holds every frame until `finish`, so a GIF is bounded in what it can hold.
    if gif.width == 0 || gif.width > GIF_MAX_WIDTH || gif.fps == 0 || gif.fps > 60 {
        return Err(format!("a GIF is 1 to {GIF_MAX_WIDTH} pixels wide and 1 to 60 frames a second").into());
    }
    let (from, to) = picture_span(project, settings);
    if f64::from(to - from) / meta.frame_rate > GIF_MAX_SECS {
        return Err(format!("a GIF is at most {GIF_MAX_SECS} s long: export a shorter range").into());
    }
    let (width, height) = fit_box(meta.width, meta.height, gif.width, GIF_MAX_WIDTH);
    let mut writer = GifWriter::open(width, height, gif.loop_forever)?;
    let fps = f64::from(gif.fps);
    let frames = ((f64::from(to - from) * fps) / meta.frame_rate).ceil() as u32;
    if frames == 0 {
        return Err("the range holds no frames".into());
    }
    for i in 0..frames {
        cancelled(shared)?;
        // The timeline frame this GIF frame shows, and how long it holds: the hundredths of a
        // second between this frame's start and the next, so the rounding never drifts.
        let at = ((f64::from(i) * meta.frame_rate) / fps) as u32;
        let shown = from + at.min(to - from - 1);
        let next = (f64::from(i + 1) * 100.0 / fps).round() as u32;
        let delay = (next - (f64::from(i) * 100.0 / fps).round() as u32).max(1) as u16;
        let picture = compose_at_with(project, meta, shown, width, height, settings.burn_subtitles.clone())?;
        let rgba: Vec<u8> = picture
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|px| [px[2], px[1], px[0], 255])
            .collect();
        writer.push(&rgba, delay)?;
        shared.progress.fetch_max(picture_progress(i + 1, frames), Ordering::Relaxed);
    }
    std::fs::write(part, writer.finish()?)?;
    Ok(())
}

/// The widest GIF, and the longest: the encoder keeps every frame until it is finished.
const GIF_MAX_WIDTH: u32 = 720;
const GIF_MAX_SECS: f64 = 30.0;

/// The GIF writer, over `ec_image::gif_encode`: 256 colours with dithering (video gradients band
/// badly without it), looping forever or playing once.
struct GifWriter(ec_image::gif_encode::Encoder);

impl GifWriter {
    fn open(width: u32, height: u32, loop_forever: bool) -> crate::Result<Self> {
        let opts = ec_image::gif_encode::GifOptions {
            loop_count: if loop_forever { Some(0) } else { None },
            max_colors: 256,
            dither: true,
        };
        let encoder = ec_image::gif_encode::Encoder::new(width as u16, height as u16, opts)
            .map_err(|e| format!("GIF: {e}"))?;
        Ok(Self(encoder))
    }

    fn push(&mut self, rgba: &[u8], delay_cs: u16) -> crate::Result<()> {
        self.0.push_rgba8(rgba, delay_cs).map_err(|e| format!("GIF: {e}").into())
    }

    fn finish(self) -> crate::Result<Vec<u8>> {
        self.0.finish().map_err(|e| format!("GIF: {e}").into())
    }
}

fn run_audio(
    project: &Project,
    meta: &VideoMeta,
    out: &Path,
    shared: &Shared,
    settings: &ExportSettings,
    sample_rate: Option<u32>,
) -> crate::Result<()> {
    let format = settings.format;
    let (samples, audio) = mix_pcm(project, meta, shared, settings, sample_rate, 16)?
        .ok_or("this timeline has no audio to export")?;
    // Written in one go and still cancellable up to here, exactly as the mp4
    // path is up to its `finish`: the `.part` is what either gets renamed or
    // deleted.
    cancelled(shared)?;
    match format {
        Format::Wav => write_wav(out, &samples, &audio)?,
        Format::Flac => write_flac(out, &samples, &audio)?,
        Format::Mp3 => write_mp3(out, &samples, &audio, audio_kbps_of(settings))?,
        Format::Ogg => write_ogg(out, &samples, &audio)?,
        Format::Opus => write_opus(out, &samples, &audio, project, settings, shared)?,
        _ => unreachable!("the picture formats are `run`"),
    }
    shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed);
    Ok(())
}

/// The sample width of a ProRes file's PCM sound: 24 unless
/// [`ExportSettings::audio_bit_depth`] says 16.
fn prores_audio_bits(settings: &ExportSettings) -> u8 {
    match settings.audio_bit_depth {
        Some(16) => 16,
        _ => 24,
    }
}

/// The ProRes file's sound pass: the mix as `bits`-wide samples, `(samples, rate,
/// channels)`, or `None` for a timeline with no sound. Runs beside the picture.
fn mix_pcm16(
    project: &Project,
    meta: &VideoMeta,
    shared: &Shared,
    settings: &ExportSettings,
    sample_rate: Option<u32>,
    bits: u8,
) -> crate::Result<Option<(Vec<i32>, u32, u16)>> {
    Ok(mix_pcm(project, meta, shared, settings, sample_rate, bits)?
        .map(|(samples, audio)| (samples, audio.sample_rate, audio.channels)))
}

/// A running ProRes sound pass, joined when dropped for the reason [`AudioPass`] is.
struct PcmPass(Option<PcmJob>);

/// The ProRes sound pass's thread: `(samples, rate, channels)` or `None` for no sound.
type PcmJob = thread::JoinHandle<crate::Result<Option<(Vec<i32>, u32, u16)>>>;

impl PcmPass {
    fn take(&mut self) -> Option<PcmJob> {
        self.0.take()
    }
}

impl Drop for PcmPass {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            let _ = job.join();
        }
    }
}

fn join_pcm(job: PcmJob) -> crate::Result<Option<(Vec<i32>, u32, u16)>> {
    job.join()
        .map_err(|_| crate::Error::from("the sound pass stopped without a picture or a reason"))?
}

/// The audio lanes mixed to 16-bit samples (in an `i32`), the length of the
/// range: [`run_audio`]'s body up to the write, so a WAV and a ProRes file's PCM
/// track are the same mix. `None` where the timeline has no sound.
fn mix_pcm(
    project: &Project,
    meta: &VideoMeta,
    shared: &Shared,
    settings: &ExportSettings,
    sample_rate: Option<u32>,
    bits: u8,
) -> crate::Result<Option<(Vec<i32>, AudioMeta)>> {
    let format = settings.format;
    // Full scale of the sample width asked for: 32767 or 8388607. The f32 mix is
    // scaled straight to it, so a 24-bit file is not a widened 16-bit one.
    let full = ((1i64 << (bits - 1)) - 1) as f32;
    let sources = project.audio_sources();
    let range_start = settings.range.map_or(0, |(s, _)| s);
    let pre = warm_up(project, range_start, meta.frame_rate);
    let offset = range_start - pre;
    let segs = project.audio_segments_from(offset, meta.frame_rate);
    // The equalizers with them, so what is written is what is heard down to the
    // filter: the worker applies them per segment before the lanes are summed
    // ([`AudioSession::open_mixed_streams_eq`]), which is the same choke point
    // playback's feeder reads from -- there is no second place here that could
    // apply them differently.
    let eqs = project.audio_eqs_from(offset, meta.frame_rate);
    // ...and the rates with them, for exactly the same reason: a speeded clip is
    // resampled inside the worker playback feeds from, so a WAV of a timeline at
    // 2x is half as long and holds the samples that were heard -- not a
    // second pass here that could disagree with what the ear got.
    let speeds = project.audio_speeds_from(offset, meta.frame_rate);
    // ...and the fades with them, the same choke-point reason again: a clip's
    // envelope runs inside the worker playback feeds from too.
    let fades = project.audio_fades_from(offset, meta.frame_rate);
    // ...and the mix over both: each lane's volume into the sum, the master
    // limiter out of it. The same opener playback's feeder reads from, so a
    // file is written at the levels it was heard at -- there is no second
    // place here that could mix it differently.
    let Some((audio, chunks)) = AudioSession::open_mixed_streams_master_at_fade(
        &sources,
        &segs,
        &eqs,
        &speeds,
        &fades,
        meta.frame_rate,
        &project.audio_gains(),
        &project.audio_ducks(),
        &project.audio_voices(),
        project.limiter(),
        sample_rate,
    )?
    else {
        return Ok(None);
    };
    // The picture path's line, for a file that is sound alone: there is sound
    // by the line above, so this names the encoder writing it. A picture file
    // publishes its own.
    if !format.has_video() {
        *shared.encoders.lock().unwrap() =
            Some(audio_label(project, format, true, None, settings.range.is_some()).to_string());
    }
    let channels = usize::from(audio.channels);
    let out_frames = settings.range.map_or(project.media_frames(), |(s, e)| {
        e.min(project.media_frames()).saturating_sub(s)
    });
    let frames =
        (f64::from(out_frames) / meta.frame_rate * f64::from(audio.sample_rate)).round() as u64;
    let total = frames as usize * channels;
    // corner-cut: the whole export sits in memory (4 bytes a sample, so ~23 MB
    // per minute of 48 kHz stereo). `ec-flac` takes it that way and
    // the mp4 path already collects its copied AAC track the same. Upgrade path
    // is ec-riff's incremental writer, fed one block at a time.
    let skip = warm_up_samples(range_start, pre, meta.frame_rate, audio.sample_rate, channels);
    let mut samples: Vec<i32> = Vec::with_capacity(total + skip);
    // Only for a loudness target: the mix at f32, measured and levelled once whole.
    let mut wide: Vec<f32> = Vec::new();
    for chunk in chunks {
        cancelled(shared)?;
        if shared.loudness.is_some() {
            wide.extend_from_slice(&chunk.samples);
            // The same range-only progress as the plain path below.
            let done = wide.len().saturating_sub(skip).min(total) * PROGRESS_SCALE as usize / total.max(1);
            if !format.has_video() { shared.progress.store(done as u32, Ordering::Relaxed); }
            if wide.len() >= skip + total {
                break;
            }
            continue;
        }
        // Already equalized: the chunks come out of the per-lane workers, which
        // filter each segment before the mix (`open_mixed_streams_eq` above).
        samples.extend(
            chunk
                .samples
                .iter()
                .map(|s| (s.clamp(-1.0, 1.0) * full).round() as i32),
        );
        // Only the range counts towards the bar: not the pre-roll, not the mix past the end.
        let done = samples.len().saturating_sub(skip).min(total) * PROGRESS_SCALE as usize / total.max(1);
        if !format.has_video() { shared.progress.store(done as u32, Ordering::Relaxed); }
        if samples.len() >= skip + total {
            break;
        }
    }
    if let Some(target) = shared.loudness {
        // The voice warm-up is dropped first, so the gain is measured over the range alone.
        wide.drain(..skip.min(wide.len()));
        wide.resize(total, 0.0);
        level_to_target(&mut wide, audio.sample_rate, audio.channels.max(1), target);
        samples.extend(
            wide.iter().map(|s| (s.clamp(-1.0, 1.0) * full).round() as i32),
        );
        if !format.has_video() { shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed); }
    } else {
        // The pre-roll is in the samples in 16-bit form too, so it goes before the length is set.
        samples.drain(..skip.min(samples.len()));
    }
    samples.resize(total, 0);
    Ok(Some((samples, audio)))
}

/// The sound alone as AAC in an mp4 box: the `.m4a` file, and the `.mp4` of a
/// timeline with no picture. The track is the one a picture export carries --
/// copied packet for packet where a copy says what the timeline says
/// ([`copy_audio`]), encoded where it cannot -- so the two files hold the same
/// samples and differ in the brand alone ([`Mp4Muxer::create_audio_only`]).
fn run_aac(
    project: &Project,
    meta: &VideoMeta,
    out: &Path,
    shared: &Shared,
    settings: &ExportSettings,
    sample_rate: Option<u32>,
) -> crate::Result<()> {
    let track = copy_audio(
        project,
        meta,
        audio_kbps_of(settings),
        false,
        false,
        shared,
        sample_rate,
        settings.range,
    )?
    .ok_or("this timeline has no audio to export")?;
    let track = ExportAudio {
        language: audio_language(project),
        ..track
    };
    *shared.encoders.lock().unwrap() = Some(
        audio_label(
            project,
            settings.format,
            true,
            Some(track.copied),
            settings.range.is_some(),
        )
        .to_string(),
    );
    let mut mp4 = Mp4Muxer::create_audio_only(
        out,
        &params_of(&track),
        settings.format == Format::M4a,
    )?;
    if let Some(frames) = track.pcm_frames {
        mp4.set_audio_frames(frames)?;
    }
    let count = track.packets.len().max(1);
    for (i, packet) in track.packets.iter().enumerate() {
        cancelled(shared)?;
        mp4.write_audio_packet(&packet.bytes)?;
        shared
            .progress
            .fetch_max((i * PROGRESS_SCALE as usize / count) as u32, Ordering::Relaxed);
    }
    cancelled(shared)?;
    mp4.set_chapters(&settings.chapters)?;
    mp4.finish()?;
    shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed);
    Ok(())
}

/// 16-bit PCM at the timeline's own rate and layout: no resampling anywhere in
/// this engine, so what the decoder handed over is what the header says.
fn write_wav(out: &Path, samples: &[i32], audio: &AudioMeta) -> crate::Result<()> {
    // Every sample is in hand, so the frame count is known before the header
    // is written: a long enough export gets RF64 instead of a RIFF that would
    // overflow its 32-bit sizes.
    let channels = u64::from(audio.channels.max(1));
    let options = ec_riff::WavOptions {
        expected_frames: samples.len() as u64 / channels,
        ..Default::default()
    };
    let mut writer = ec_riff::WavWriter::create_with_options(
        out,
        ec_riff::WavSpec {
            channels: audio.channels,
            sample_rate: audio.sample_rate,
            bits_per_sample: 16,
            sample_format: ec_riff::SampleType::Int,
        },
        options,
    )?;
    for &sample in samples {
        writer.write_sample(sample as i16)?;
    }
    // Not `drop`: this is what rewrites the RIFF sizes in the header, and its
    // failure is the difference between a finished file and a truncated one.
    writer.finalize()?;
    Ok(())
}

/// The same samples as MPEG-1 Layer III, `ec-mp3` doing the encoding -- the
/// family's own, taking the seat at the same contract (CBR in, frames out,
/// `Mp3EncoderConfig`'s fields are the old seat's names), so the swap cost the
/// call sites nothing.
///
/// CBR at the caller's rate, [`DEFAULT_AUDIO_KBPS`] where there was none. Every
/// offered rate is already a legal Layer III value, so the encoder's snap is a
/// backstop and not a silent substitution; a sample rate MPEG has no frame for
/// (anything but the nine MPEG rates 8-48 kHz) is refused there by name rather
/// than written as something else.
///
/// corner-cut: CBR only -- the card offers rates and not a quality index. Upgrade
/// path is `Mp3EncoderConfig::vbr_quality` behind a setting of its own.
fn write_mp3(out: &Path, samples: &[i32], audio: &AudioMeta, kbps: u32) -> crate::Result<()> {
    let mut encoder = ec_mp3::encode::Mp3Encoder::new(ec_mp3::encode::Mp3EncoderConfig {
        bitrate_kbps: kbps,
        vbr_quality: None,
    });
    // The samples are the 16-bit ones a WAV of this timeline holds, so the two
    // files are one mix -- and `push_pcm_s16` divides by 32768 exactly as this
    // engine's own decoders do.
    let pcm: Vec<i16> = samples.iter().map(|&s| s as i16).collect();
    encoder
        .push_pcm_s16(&pcm, audio.channels, audio.sample_rate)
        .map_err(|e| format!("mp3 encode: {e}"))?;
    encoder.finish();
    let mut file = BufWriter::new(File::create(out)?);
    // `next_packet` after `finish` yields frames until `Eof`, the Xing/Info
    // header first: the loop ends on the only error it can see.
    while let Ok(frame) = encoder.next_packet() {
        file.write_all(&frame)?;
    }
    file.flush()?;
    Ok(())
}

/// Where the Vorbis rate loop starts, on `ec-vorbis`'s normalised `[0, 1]`
/// scale, beside [`VORBIS_TARGET_BPS`] -- the two numbers every `.ogg` this
/// program writes is coded at.
///
/// A measured operating point, not a taste (2026-09-13 bench, every output
/// decoded back with **symphonia** -- an independent decoder, and the very one
/// [`crate::audio`] reopens an export with). The incumbent seat's own 0.85 --
/// its top quality that still decoded every fixture to exactly its own length
/// -- measured 124-176 kbps across this suite's fixtures with correlation
/// 0.9947-0.9957 against the source. The quality knob alone on the new seat
/// saturates well under that band (0.98 lands ~87 kbps on a 30-second music
/// clip), so the seat is driven the way its own rate gates are measured: the
/// loop starts at 0.85 and is held to a 128 kbps target, which landed
/// 124-134 kbps actual on the same inputs and beat the incumbent at its own
/// operating point on every one of them -- correlation 0.9968 against 0.9947
/// on the music clip, +3.1 dB PSNR -- at the same size of file. Against
/// ffmpeg's own libvorbis at `-q 5` (125.8 kbps actual on the same clip) it
/// reads 0.9968 against 0.9973: par at the same rate.
const VORBIS_QUALITY: f32 = 0.85;

/// The rate [`VORBIS_QUALITY`]'s loop is held to, in bits per second. Measured
/// landing within +1..+5% of target across the bench's inputs; the Sound row
/// stays refused for this format because this target is the encoder's measured
/// operating point, not a number a card may offer.
const VORBIS_TARGET_BPS: i32 = 128_000;

/// The same samples as Vorbis I in an Ogg container: `ec-vorbis` encodes,
/// `ec-ogg` pages. Pure Rust on both halves, like every other encoder here.
///
/// The old seat's two corrections are gone with it, and the going is measured
/// rather than assumed (2026-09-13, every output decoded back with symphonia).
/// This encoder numbers its packets the spec's way -- the first audio packet
/// at granule 0, every packet after at the absolute sample position its block
/// ends on -- and clamps its final granule to the sample count it was fed, so
/// a decoder trims the block grid's tail on its own. Written straight through,
/// every file decodes to exactly the timeline's frame count with the
/// correlation peak at lag zero, which is what the one-hop pre-roll and the
/// two granule corrections this function used to carry around the old encoder
/// were for.
///
/// **Stereo, always.** The format row promises stereo, so a mono mix is
/// written as dual mono, both channels bit-identical (measured: `max|L-R|` is
/// exactly 0; the encoder's own channel coupling codes the empty side channel
/// for almost nothing). The encoder would serve a mono stream, but the row
/// says stereo and folding the file back down must give exactly what was
/// mixed. Wider than stereo cannot reach here -- [`crate::audio`] folds 5.1
/// to stereo when the source is opened -- but it is refused by name rather
/// than left to the library's own message.
///
/// corner-cut: one operating point, [`VORBIS_QUALITY`] +
/// [`VORBIS_TARGET_BPS`], with no user control -- the Sound row is refused for
/// this format. Upgrade path is a quality picker of its own, worth building
/// the day the card has a control that speaks in quality rather than kbps.
fn write_ogg(out: &Path, samples: &[i32], audio: &AudioMeta) -> crate::Result<()> {
    if audio.channels > 2 {
        return Err(format!(
            "Ogg Vorbis is written in stereo here and this mix has {} channels",
            audio.channels
        )
        .into());
    }
    // The mp3 path's conversion, so the two files are one mix -- the same
    // /32768 scaling this engine's own decoders do -- plus (for a mono mix)
    // the dual-mono widening: the same sample in both channels is the same
    // signal, not a widening of it, so what a player folds back down is what
    // was mixed.
    let pcm: Vec<f32> = match audio.channels < 2 {
        true => samples
            .iter()
            .flat_map(|&s| [f32::from(s as i16) / 32768.0; 2])
            .collect(),
        false => samples
            .iter()
            .map(|&s| f32::from(s as i16) / 32768.0)
            .collect(),
    };
    let frames = samples.len() / usize::from(audio.channels).max(1);

    let mut encoder = ec_vorbis::VorbisEncoder::new(ec_vorbis::EncoderConfig {
        sample_rate: audio.sample_rate,
        channels: 2,
        bitrate_bps: VORBIS_TARGET_BPS,
        quality: VORBIS_QUALITY,
    })
    .map_err(|e| format!("vorbis encode: {e}"))?;
    encoder
        .push_interleaved(&pcm)
        .map_err(|e| format!("vorbis encode: {e}"))?;
    encoder.finish();
    let mut packets = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(packet) => packets.push(packet),
            Err(ec_core::Error::Eof) => break,
            Err(ec_core::Error::NeedMore) => continue,
            Err(e) => return Err(format!("vorbis encode: {e}").into()),
        }
    }
    // The three setup headers travel with the encoder, not in the packet
    // stream; with no audio packets there is no stream to page.
    if packets.is_empty() {
        return Err("vorbis encode: no audio packets is not a stream".into());
    }
    let headers = encoder.headers();
    let extradata =
        ec_ogg::xiph_lace(&headers).ok_or("vorbis encode: the three headers would not lace")?;

    let time_base =
        TimeBase::try_new(1, i64::from(audio.sample_rate)).map_err(|e| format!("ogg mux: {e}"))?;
    let mut params = CodecParameters::new(CodecId::Vorbis);
    params.extradata = Some(Buf::from(extradata));
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: audio.sample_rate,
        layout: ChannelLayout::Stereo,
        ..AudioParameters::default()
    });
    let mut stream = StreamInfo::new(0, time_base, params);
    stream.duration = Some(frames as i64);
    stream.start_time = Some(0);
    let mut muxer = ec_ogg::OggMuxer::new(File::create(out)?);
    // Without this every audio packet of a short export lands on one page, and
    // the muxer's own note records what that costs: a first-audio-page-is-also-
    // EOS stream decodes short by half a small block in a reference decoder.
    muxer.set_page_target_bytes(Some(OGG_PAGE_BYTES));
    muxer
        .add_stream(stream)
        .map_err(|e| format!("ogg mux: {e}"))?;
    for packet in &packets {
        // The granule is the encoder's own: the absolute sample position this
        // packet ends at, clamped to the mix's length on the last one. It goes
        // in as side data, which is where `ec-ogg` reads it, because a packet's
        // `pts + duration` would state the position one block late.
        let mut out_packet =
            Packet::new(0, time_base, packet.data.clone()).with_duration(packet.samples);
        out_packet
            .side_data
            .push(ec_ogg::granule_side_data(packet.granule));
        muxer
            .write_packet(&out_packet)
            .map_err(|e| format!("ogg mux: {e}"))?;
    }
    muxer.finish().map_err(|e| format!("ogg mux: {e}"))?;
    Ok(())
}

/// The same samples as Opus in Ogg ([`crate::opus_write`]), at the Sound row's
/// rate clamped into Opus's own range (6-510 kbps, [`crate::opus_write::DEFAULT_KBPS`]
/// where none was asked for), with the language and chapters in `OpusTags`.
/// Cancellable per packet, like the mix before it.
fn write_opus(
    out: &Path,
    samples: &[i32],
    audio: &AudioMeta,
    project: &Project,
    settings: &ExportSettings,
    shared: &Shared,
) -> crate::Result<()> {
    if audio.sample_rate != OPUS_RATE {
        return Err(format!("Opus is coded at 48 kHz and this mix came out at {} Hz", audio.sample_rate).into());
    }
    let pcm: Vec<f32> = samples.iter().map(|&s| f32::from(s as i16) / 32768.0).collect();
    let language = audio_language(project);
    let tags = crate::opus_write::Tags { language: language.as_deref(), chapters: &settings.chapters };
    crate::opus_write::write(
        out,
        &pcm,
        usize::from(audio.channels.max(1)),
        audio.sample_rate,
        settings.audio_kbps.unwrap_or(crate::opus_write::DEFAULT_KBPS),
        &tags,
        &mut |_| cancelled(shared),
    )
}

/// The page size [`write_ogg`] asks the Ogg muxer to aim at, in bytes. RFC 3533
/// describes pages as "usually 4-8 kB", which is also the muxer's own suggested
/// general-purpose value.
const OGG_PAGE_BYTES: usize = 4096;

/// The same samples, losslessly compressed, through [`crate::flac_write`]. The
/// export is always 16-bit: the samples were scaled to `i16` above.
fn write_flac(out: &Path, samples: &[i32], audio: &AudioMeta) -> crate::Result<()> {
    crate::flac_write::write(out, samples, usize::from(audio.channels), 16, audio.sample_rate)
}

/// The I420 planes of one black picture, allocated once for a whole export: a
/// gap in the video lane is encoded, not skipped, or every frame after it would
/// arrive early. Limited-range black is `Y=16, U=V=128`, the same convention
/// [`crate::convert`] decodes with.
struct Black {
    y: Vec<u8>,
    uv: Vec<u8>,
    width: u32,
    height: u32,
}

impl Black {
    fn new(meta: &VideoMeta) -> Self {
        let (w, h) = (meta.width as usize, meta.height as usize);
        Self {
            y: vec![16; w * h],
            uv: vec![128; w.div_ceil(2) * h.div_ceil(2)],
            width: meta.width,
            height: meta.height,
        }
    }

    fn picture(&self) -> (&[u8], &[u8], &[u8], u32, u32) {
        (&self.y, &self.uv, &self.uv, self.width, self.height)
    }
}

/// `Err` once a cancel has been asked for. Called at every point where the work
/// left is more than an instant, so an `esc` at 99.9% still stops the export
/// instead of quietly completing it.
fn cancelled(shared: &Shared) -> crate::Result<()> {
    if shared.cancel.load(Ordering::Relaxed) {
        return Err("export cancelled".into());
    }
    Ok(())
}

/// The two containers an export writes, so the picture loop above is one loop:
/// H.264 goes into mp4, AV1 into Matroska, and nothing else is offered.
enum Muxer {
    Mp4(Mp4Muxer),
    Mkv(MkvMuxer),
    /// ProRes in QuickTime ([`crate::prores_out::MovWriter`]).
    Mov(crate::prores_out::MovWriter),
}

impl Muxer {
    fn finish(self) -> crate::Result<()> {
        match self {
            Self::Mp4(mp4) => mp4.finish(),
            Self::Mkv(mkv) => mkv.finish(),
            Self::Mov(mov) => mov.finish(),
        }
    }
}

/// Test-only panic point, so the worker's own `catch_unwind` branch in
/// [`start`] is reachable without a codec parser that actually panics: the
/// variable names the one `.part` whose export dies, and no export but that
/// one ever matches it, so a concurrent export in the same process is safe.
fn inject_export_test_panic(out: &Path) {
    if crate::envcfg::var_os("VE_EXPORT_TEST_PANIC").is_some_and(|target| target == out.as_os_str()) {
        panic!("export test panic: the .part is on disk");
    }
}

/// Writes one access unit, creating the file on the first one -- the parameter
/// sets for `avcC` (and the sequence header for `CodecPrivate`) only exist once
/// the encoder has coded something. Units with no coded slice are skipped: a
/// software encoder may hand back an empty buffer while it buffers, and the
/// muxer rejects a sample that would carry no picture.
#[allow(
    clippy::too_many_arguments,
    reason = "the export's per-access-unit sink: its inputs are passed positionally at its two call sites, and a struct would only rename them"
)]
fn write_video(
    muxer: &mut Option<Muxer>,
    out: &Path,
    meta: &VideoMeta,
    settings: &ExportSettings,
    source_colour: Tags,
    source_language: Option<&str>,
    audio: Option<&AudioParams>,
    packets: &mut Option<Vec<crate::AacPacket>>,
    subs: &mut Vec<SubParams>,
    au: &[u8],
    key: bool,
    ten_bit: bool,
) -> crate::Result<()> {
    if settings.format.is_prores() {
        // The encoder's own frame, one sample per picture; the track is declared
        // by the first one, the sound is written after the last.
        let Some(Muxer::Mov(mov)) = muxer else {
            unreachable!("the ProRes seat opens its own muxer")
        };
        return mov.write_frame(au);
    }
    if settings.format.is_hevc() {
        // The parameter sets are in the access unit -- every intra AU carries
        // VPS, SPS and PPS -- and the record built from them is what *both*
        // containers declare the track with: an mp4's `hvcC` box and a
        // Matroska `CodecPrivate` are the same bytes, so the two files are one
        // stream written twice rather than two.
        let sample =
            crate::mux::annex_b_to_hvcc(au).ok_or("a coded HEVC unit with no slice in it")?;
        if settings.format == Format::Hevc {
            let muxer = match muxer {
                Some(Muxer::Mkv(mkv)) => mkv,
                Some(Muxer::Mp4(_)) => unreachable!("the format picks the muxer once"),
                none => {
                    let hvcc = hevc_params(au, ten_bit)?;
                    let sound = audio.zip(packets.take());
                    let Muxer::Mkv(mkv) = none.insert(Muxer::Mkv(MkvMuxer::create_hevc(
                        out,
                        &HevcParams {
                            width: meta.width,
                            height: meta.height,
                            frame_rate: meta.frame_rate,
                            hvcc: &hvcc,
                            language: source_language.map(str::to_owned),
                            colour: (settings.hdr.transfer != HdrTransfer::Sdr).then_some(source_colour),
                            light: (settings.hdr.transfer != HdrTransfer::Sdr)
                                .then(|| crate::mux::hdr_light(&settings.hdr)),
                        },
                        sound,
                        std::mem::take(subs),
                    )?)) else {
                        unreachable!("just inserted a Matroska muxer")
                    };
                    mkv.set_chapters(&settings.chapters)?;
                    mkv
                }
            };
            return muxer.write_frame(&sample, key);
        }
        let muxer = match muxer {
            Some(Muxer::Mp4(mp4)) => mp4,
            Some(Muxer::Mkv(_)) => unreachable!("the format picks the muxer once"),
            none => {
                let hvcc = hevc_params(au, ten_bit)?;
                let Muxer::Mp4(mp4) = none.insert(Muxer::Mp4(Mp4Muxer::create_hevc(
                    out,
                    &HevcParams {
                        width: meta.width,
                        height: meta.height,
                        frame_rate: meta.frame_rate,
                        hvcc: &hvcc,
                        // mp4 does not state a picture's language yet.
                        language: None,
                        colour: None,
                        light: None,
                    },
                    audio,
                )?)) else {
                    unreachable!("just inserted an mp4 muxer")
                };
                // The same `colr` rule as AV1's: the colour a 10-bit SDR export
                // declares, which the bitstream's VUI states as well.
                if source_colour != Tags::default() {
                    mp4.set_colour(source_colour);
                }
                if settings.hdr.transfer != HdrTransfer::Sdr {
                    mp4.set_hdr(&settings.hdr);
                }
                mp4
            }
        };
        return muxer.write_coded_sample(&sample, key);
    }
    if settings.format.is_av1() {
        // Every AV1 stream opens on a keyframe, and a keyframe carries the
        // sequence header the track has to declare: an encoder that handed back
        // neither has produced nothing a decoder can start. The same header goes
        // into a `CodecPrivate` and into an `av1C` -- one record, two containers.
        fn params<'a>(
            meta: &VideoMeta,
            au: &'a [u8],
            language: Option<&str>,
            ten_bit: bool,
            colour: Tags,
        ) -> crate::Result<Av1Params<'a>> {
            Ok(Av1Params {
                width: meta.width,
                height: meta.height,
                frame_rate: meta.frame_rate,
                config: crate::mux::av1_sequence_header(au)
                    .ok_or("the first coded picture carries no AV1 sequence header")?,
                language: language.map(str::to_owned),
                ten_bit,
                colour: (colour != Tags::default()).then_some(colour),
            })
        }
        if matches!(settings.format, Format::Av1 | Format::Webm) {
            let muxer = match muxer {
                Some(Muxer::Mkv(mkv)) => mkv,
                Some(Muxer::Mp4(_)) => unreachable!("the format picks the muxer once"),
                none => {
                    // The sound with it, all of it: the Matroska muxer writes the
                    // packets into the clusters of the pictures they play under.
                    let sound = audio.zip(packets.take());
                    let created = match settings.format {
                        // WebM has no subtitle track to take ([`export_subtitles`]).
                        Format::Webm => MkvMuxer::create_webm(out, &params(meta, au, source_language, ten_bit, source_colour)?, sound)?,
                        _ => {
                            MkvMuxer::create(
                                out,
                                &params(meta, au, source_language, ten_bit, source_colour)?,
                                sound,
                                std::mem::take(subs),
                            )?
                        }
                    };
                    let Muxer::Mkv(mkv) = none.insert(Muxer::Mkv(created)) else {
                        unreachable!("just inserted a Matroska muxer")
                    };
                    mkv.set_chapters(&settings.chapters)?;
                    mkv
                }
            };
            return muxer.write_frame(au, key);
        }
        let muxer = match muxer {
            Some(Muxer::Mp4(mp4)) => mp4,
            Some(Muxer::Mkv(_)) => unreachable!("the format picks the muxer once"),
            none => {
                let Muxer::Mp4(mp4) = none.insert(Muxer::Mp4(Mp4Muxer::create_av1(
                    out,
                    &params(meta, au, None, ten_bit, source_colour)?,
                    audio,
                )?)) else {
                    unreachable!("just inserted an mp4 muxer")
                };
                // The `colr` box states the colour a 10-bit SDR export declares; an
                // export with no colour of its own keeps the output rule's box.
                if source_colour != Tags::default() {
                    mp4.set_colour(source_colour);
                }
                mp4
            }
        };
        return muxer.write_coded_sample(au, key);
    }
    if !crate::mux::has_coded_slice(au) {
        return Ok(());
    }
    let muxer = match muxer {
        Some(Muxer::Mp4(mp4)) => mp4,
        Some(Muxer::Mkv(_)) => unreachable!("the format picks the muxer once"),
        none => {
            let (sps, pps) = crate::mux::parameter_sets(au)
                .ok_or("the first coded picture carries no SPS/PPS")?;
            let Muxer::Mp4(mp4) = none.insert(Muxer::Mp4(Mp4Muxer::create(
                out,
                &VideoParams {
                    width: meta.width,
                    height: meta.height,
                    frame_rate: meta.frame_rate,
                    sps,
                    pps,
                },
                audio,
            )?)) else {
                unreachable!("just inserted an mp4 muxer")
            };
            // A file written in its source's own space says so, rather than
            // claiming the space its height would imply
            // ([`ExportSettings::keep_source_colour`]).
            if settings.keep_source_colour || source_colour != Tags::default() {
                mp4.set_colour(source_colour);
            }
            mp4
        }
    };
    muxer.write_video_au(au)
}

/// The `hvcC` record of the first coded unit, or the refusal by name. Split out
/// because both HEVC containers ask it in the same words.
fn hevc_params(au: &[u8], ten_bit: bool) -> crate::Result<Vec<u8>> {
    crate::mux::hvcc_record_at(au, ten_bit)
        .ok_or_else(|| crate::Error::from("the first coded picture carries no HEVC VPS/SPS/PPS"))
}

fn bitrate_for(meta: &VideoMeta) -> u64 {
    let raw = f64::from(meta.width) * f64::from(meta.height) * meta.frame_rate * BITS_PER_PIXEL;
    (raw as u64).clamp(MIN_BITRATE, MAX_BITRATE)
}

fn forced(var: &str) -> bool {
    crate::envcfg::var_os(var).is_some_and(|v| v == "1")
}

/// Hardware where it is available, software everywhere else -- chosen once for
/// the whole export, so the stream never changes encoder mid-file. Both codecs
/// have both seats: an AV1 export runs on the plugin's VA-API encoder where the
/// GPU has one and on `rav1e` where it has not, which is the same pair H.264 has
/// had and the same silent fallback.
enum Enc {
    Hw(HwEncoder),
    Sw {
        encoder: Encoder,
        /// The access unit currently lent out: the encoder hands back an owned
        /// `Vec` where the seat's contract lends a slice, so one lives here
        /// between the two.
        au: Vec<u8>,
    },
    /// AV1 on the GPU, through the same plugin the H.264 seat uses.
    Av1Hw(HwEncoder),
    /// HEVC on the GPU, through that same plugin -- and intra-only there too,
    /// so this and [`Enc::Hevc`] write the same kind of file.
    HevcHw(HwEncoder),
    /// AV1 software at 10 bits: `rav1e`'s 16-bit context. A picture that passes
    /// through the span untouched arrives as 16-bit planes off the 10-bit decode
    /// ([`Enc::encode_wide`]); an 8-bit picture (one composed on the way) is shifted
    /// up to the same scale in [`Enc::encode`].
    Av1Sw16 {
        context: rav1e::Context<u16>,
        ready: std::collections::VecDeque<(Vec<u8>, bool)>,
        au: Vec<u8>,
        flushed: bool,
        scratch: [Vec<u16>; 3],
    },
    Av1Sw {
        context: rav1e::Context<u8>,
        /// Temporal units the encoder has finished but the caller has not
        /// collected: `rav1e` reorders and may hand back several at once, while
        /// this interface is one picture in, one unit out (the plugin's own
        /// `ready` queue exists for the same reason).
        ready: std::collections::VecDeque<(Vec<u8>, bool)>,
        /// The unit currently lent out, for the same reason `Sw` owns one.
        au: Vec<u8>,
        flushed: bool,
    },
    /// HEVC, intra-only and software: `ec-h265` codes every picture an IDR
    /// carrying its own VPS/SPS/PPS. A batch of frames codes on as many lane
    /// encoders as there are pictures, and each encoder's wavefront splits
    /// whatever cores the lanes leave over -- the same shape as the seat it
    /// replaced, the cores spent on frames and inside one picture at the same
    /// time. Asked for by [`ExportSettings::intra_only`].
    Hevc(HevcEnc),
    /// HEVC, I+P GOPs, software: `ec-h265`'s sequence coder on a worker thread.
    /// The default HEVC seat ([`Enc::open_hevc_gop`]).
    HevcGop(HevcGop),
    /// ProRes, software: `ec-prores` codes one whole frame per call (its own
    /// worker threads inside), so one picture in is one sample out. The 4:2:0
    /// pictures of the walk are widened to 10 bits and chroma-upsampled to the
    /// profile's layout ([`crate::prores_out`]).
    ProRes {
        encoder: ec_prores::ProResEncoder,
        /// The sample currently lent out.
        au: Vec<u8>,
        /// 4:4:4 profiles (4444, 4444 XQ) code full chroma.
        full_chroma: bool,
    },
}

/// The intra HEVC seat. Frames arrive one at a time (the export walk is one
/// picture per timeline frame) and are held until there is a lane for each,
/// then coded in parallel and queued in **display order**. `ec-h265`'s
/// wavefront spreads one picture across the workers it is given and the lanes
/// spread the frames across the machine, so decode and render keep filling
/// the next batch while this one codes -- a seat that coded one picture at a
/// time would serialize the whole walk behind its encoder (measured: 7.4 fps
/// against 11.8 at 1080p on the same box, which is the regression this
/// batching exists to prevent).
///
/// corner-cut: a batch of display-size planes sits in memory (~3 MB a frame
/// at 1080p, so ~37 MB at 12 lanes) and the tail of a timeline is coded on
/// fewer lanes than the middle. Upgrade path is a pipeline that keeps every
/// lane fed from a decoder running ahead of the encoder rather than a batch
/// barrier.
struct HevcEnc {
    /// Display-size I420 pictures waiting for a lane, in display order. The
    /// encoder pads to its own coded grid, so nothing is pre-padded here.
    pending: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>,
    /// Coded access units not yet collected, in the same order.
    ready: std::collections::VecDeque<Vec<u8>>,
    /// The unit currently lent out, for the reason `Sw` owns one: the encoder
    /// hands back an owned `Vec`, the seat's contract lends a slice.
    au: Vec<u8>,
    /// One encoder per lane. `ec-h265` is stateless from picture to picture
    /// (every picture is an IDR), so a lane coding frame *n* needs nothing
    /// the other lanes know.
    lanes: Vec<HevcEncoder>,
    /// The display width the planes arrive at; the height lives in each
    /// encoder's config, where the coded grid is derived from it.
    width: usize,
    /// Whether the lanes code at 10 bits: `pending` then holds little-endian
    /// 16-bit samples, which [`HevcEnc::code_batch`] reads back.
    ten_bit: bool,
}

impl Enc {
    /// Which seat this *is*, said the way [`video_label`] says it -- so the
    /// name a running export shows is the name the card showed, or the honest
    /// difference where the probe and the open disagreed.
    fn label(&self) -> &'static str {
        match self {
            Self::Hw(_) => video_label(Format::Mp4, true, false),
            Self::Sw { .. } => video_label(Format::Mp4, false, false),
            Self::Av1Hw(_) => video_label(Format::Av1, true, false),
            Self::Av1Sw { .. } | Self::Av1Sw16 { .. } => video_label(Format::Av1, false, false),
            Self::HevcHw(_) => video_label(Format::Hevc, true, true),
            Self::Hevc(_) => video_label(Format::Hevc, false, true),
            Self::HevcGop(_) => video_label(Format::Hevc, false, false),
            Self::ProRes { .. } => video_label(Format::ProRes, false, true),
        }
    }

    /// The ProRes seat's codec parameters (the sample entry's fourcc rides in
    /// the extradata); `None` for every other seat.
    fn prores_params(&self) -> Option<&ec_core::registry::CodecParameters> {
        match self {
            Self::ProRes { encoder, .. } => Some(ec_core::registry::Encoder::codec_parameters(encoder)),
            _ => None,
        }
    }

    /// `ten_bit` is [`wide_ten_bit`]'s answer; it means something to AV1 and HEVC only.
    /// `colour` is what the file declares, in the bitstream as well as the container.
    fn open(
        meta: &VideoMeta,
        settings: &ExportSettings,
        ten_bit: bool,
        colour: ColorDescription,
    ) -> crate::Result<Self> {
        // Which *codec* the picture is, not which container it goes in: both AV1
        // formats run the same encoder, and so do both HEVC ones.
        if settings.format.is_av1() {
            return Self::open_av1(meta, settings, ten_bit, colour);
        }
        if settings.format.is_hevc() {
            return Self::open_hevc(meta, settings, ten_bit, colour);
        }
        if settings.format.is_prores() {
            return Self::open_prores(meta, settings, colour);
        }
        // A caller's number goes through the same clamp as the computed one: a
        // zero bitrate switches the software encoder's lookahead on, which would
        // break the one-picture-per-call contract `encode` documents below.
        let bitrate = bitrate_of(meta, settings);
        // The plugin wants an exact rational, and the muxer already picks one
        // that is exact at every rate we can read -- `fps * 1000 / 1000` would
        // hand 24000/1001 over as a rounded 23.976, which is the same
        // truncation the container timing had.
        crate::mux::frame_timing(meta.frame_rate)?;
        if let Some(hw) = hw_seat(meta, settings)? {
            if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export encoder: hardware (VA-API plugin)"); }
            return Ok(Self::Hw(hw));
        }
        // H.264 codes 16x16 macroblocks, and ec-h264's floor is the
        // macroblock itself -- the old encoder took any positive even size.
        // Named here, where a card can show it, rather than let the encoder's
        // error surface from inside an export already under way.
        if meta.width < 16 || meta.height < 16 {
            return Err(format!(
                "{}x{} cannot be written as H.264: the picture is smaller \
                 than one 16x16 macroblock",
                meta.width, meta.height
            )
            .into());
        }
        // 4:2:0 addresses its conformance window in *chroma* samples, so an odd
        // picture cannot be cropped back to itself -- and an odd dimension has
        // no chroma plane of its own to begin with. Named rather than padded to
        // something a decoder would then show a column of: the same refusal the
        // HEVC seat makes, and x264's and ffmpeg's own for 4:2:0 H.264.
        if !meta.width.is_multiple_of(2) || !meta.height.is_multiple_of(2) {
            return Err(format!(
                "{}x{} cannot be written as H.264: 4:2:0 needs even dimensions",
                meta.width, meta.height
            )
            .into());
        }
        let mut cfg = EncoderConfig::new(meta.width, meta.height);
        cfg.framerate = meta.frame_rate as f32;
        cfg.bitrate = bitrate.min(u32::MAX as u64) as u32;
        // Two seconds between key frames, and no B-frames on either path: the
        // muxer times everything by duration alone, which reordering would
        // break. A proxy asks for one frame a GOP, which is every frame its own
        // starting point ([`ExportSettings::intra_only`]).
        cfg.gop_size = match settings.intra_only {
            true => 1,
            false => (meta.frame_rate * 2.0).round().max(1.0) as u32,
        };
        // Fast, the rung a fallback is allowed to cost -- the cheaper arm of
        // the preset ladder, the one the old seat's own S1 also picked when its
        // Balanced rung halved the rate for the same bitrate.
        cfg.preset = Preset::Fast;
        // 8x8 transform off. At this rung it is half the pure-encode cost
        // (17.2 ms/frame against 35.1 at 720p across all cores, measured at
        // the swap) for at most 0.7 dB BD-PSNR -- against an old seat that ran
        // Constrained Baseline and never had the transform in the first
        // place. The export stays Main-profile CABAC, which is where the
        // quality over the old seat lives; a constant-QP mode would be the
        // place to revisit the rung whole.
        cfg.transform_8x8 = false;
        let encoder = Encoder::new(cfg).map_err(|e| format!("software encoder: {e}"))?;
        Ok(Self::Sw {
            encoder,
            au: Vec::new(),
        })
    }

    /// The AV1 pair. The hardware seat is the plugin's, opened by a symbol of
    /// its own (`vh_enc_av1_open`) so a plugin built before AV1 encode existed
    /// simply has none and this falls through -- the same "no" a GPU without an
    /// AV1 encode entrypoint gives, and the same silent fallback either way.
    ///
    /// It is **asked for by name**, which is the one place this pair does not
    /// mirror the H.264 one: the vendored cros-codecs AV1 encoder hung the GPU
    /// on this project's own radeonsi box -- `engine_hw: operation failed`,
    /// then an amdgpu hard recovery and a lost context, measured 2026-08-10
    /// exporting the 720p fixture. A software encoder that takes half a minute
    /// is a worse export than a hardware one; a driver reset is not an export
    /// at all. So [`EncoderSeat::Auto`] takes `rav1e` here and only
    /// [`EncoderSeat::Hardware`] -- a person picking the GPU for AV1 on the
    /// card, under a row that names the risk -- enters the plugin's seat. It
    /// was an env pin (`VE_HW_AV1=1`) until the card could ask, which is a
    /// switch nobody exporting a film would ever find.
    ///
    /// corner-cut: the upgrade path is a driver this was reproduced against (or a
    /// cros-codecs release that fixes it) plus a probe encode of one frame at
    /// open, after which `Auto` can prefer hardware the way H.264 does.
    fn open_av1(
        meta: &VideoMeta,
        settings: &ExportSettings,
        ten_bit: bool,
        colour: ColorDescription,
    ) -> crate::Result<Self> {
        let bitrate = bitrate_of(meta, settings);
        let (fps_num, fps_den) = crate::mux::frame_timing(meta.frame_rate)?;
        // Two seconds between keyframes, as the H.264 seat does: a seek may only
        // land on one, and this is what a cluster of the Matroska file is.
        let gop = (meta.frame_rate * 2.0).round().max(1.0) as u64;
        // The 10-bit seat is software: the GPU encode is 8-bit only.
        let hw = match ten_bit {
            true => None,
            false => hw_seat(meta, settings)?,
        };
        if let Some(hw) = hw {
            if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export encoder: hardware AV1 (VA-API plugin)"); }
            return Ok(Self::Av1Hw(hw));
        }
        if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export encoder: software AV1 (rav1e)"); }
        // Seconds per frame, which is what `frame_timing` already has as an
        // exact rational -- 1001/24000 rather than a rounded 23.976.
        let mut cfg = rav1e::EncoderConfig {
            width: meta.width as usize,
            height: meta.height as usize,
            bit_depth: if ten_bit { 10 } else { 8 },
            chroma_sampling: rav1e::prelude::ChromaSampling::Cs420,
            time_base: rav1e::prelude::Rational::new(u64::from(fps_den), u64::from(fps_num)),
            ..Default::default()
        };
        cfg.min_key_frame_interval = gop;
        cfg.max_key_frame_interval = gop;
        cfg.bitrate = bitrate.min(i32::MAX as u64) as i32;
        // The fastest preset, with the settings that matter for speed at that
        // preset. Tiles spread one frame over the cores (one tile ran on a single
        // core); low latency drops the lookahead; CDEF and loop restoration are
        // off for speed (a 10 s 1080p export measured the same PSNR against the
        // source either way). Measured: 170 s before, about 17 s after, on a box
        // with load ~15. The `asm` feature in Cargo.toml is the other half.
        cfg.speed_settings = rav1e::prelude::SpeedSettings::from_preset(AV1_SPEED);
        cfg.tiles = AV1_TILES;
        cfg.low_latency = true;
        cfg.speed_settings.cdef = false;
        cfg.speed_settings.lrf = false;
        cfg.speed_settings.multiref = false;
        // What the sequence header says the samples mean, for the reason
        // [`video_signal_type`] gives: a container tag is the second answer a
        // decoder looks at, not the first. Written as rav1e's own enums rather
        // than the H.273 numbers -- the crate has no `from` for the code points,
        // and the output rule only ever picks one of these two spaces.
        let out = colour;
        cfg.pixel_range = rav1e::prelude::PixelRange::Limited;
        cfg.color_description = Some(match out.matrix {
            Matrix::Bt709 => rav1e::prelude::ColorDescription {
                color_primaries: rav1e::prelude::ColorPrimaries::BT709,
                transfer_characteristics: rav1e::prelude::TransferCharacteristics::BT709,
                matrix_coefficients: rav1e::prelude::MatrixCoefficients::BT709,
            },
            // A 10-bit SDR export of a BT.2020 source: the source's own space.
            Matrix::Bt2020Ncl => rav1e::prelude::ColorDescription {
                color_primaries: rav1e::prelude::ColorPrimaries::BT2020,
                transfer_characteristics: rav1e::prelude::TransferCharacteristics::BT2020_10Bit,
                matrix_coefficients: rav1e::prelude::MatrixCoefficients::BT2020NCL,
            },
            _ => rav1e::prelude::ColorDescription {
                color_primaries: rav1e::prelude::ColorPrimaries::BT601,
                transfer_characteristics: rav1e::prelude::TransferCharacteristics::BT601,
                matrix_coefficients: rav1e::prelude::MatrixCoefficients::BT601,
            },
        });
        let config = rav1e::Config::new()
            .with_encoder_config(cfg)
            .with_threads(std::thread::available_parallelism().map_or(1, |n| n.get()));
        if ten_bit {
            let context = config
                .new_context::<u16>()
                .map_err(|e| format!("software AV1 encoder: {e}"))?;
            return Ok(Self::Av1Sw16 {
                context,
                ready: std::collections::VecDeque::new(),
                au: Vec::new(),
                flushed: false,
                scratch: Default::default(),
            });
        }
        let context = config
            .new_context::<u8>()
            .map_err(|e| format!("software AV1 encoder: {e}"))?;
        Ok(Self::Av1Sw {
            context,
            ready: std::collections::VecDeque::new(),
            au: Vec::new(),
            flushed: false,
        })
    }

    /// The HEVC pair: the plugin's VA-API seat where the GPU has one, and the
    /// software intra encoder everywhere else -- below the driver's 384x384
    /// floor, on a plugin too old to carry the symbol, on a GPU with no HEVC
    /// encode entrypoint. Silent either way, because both write the same kind
    /// of file.
    ///
    /// The default is I+P GOPs on the software seat ([`Enc::open_hevc_gop`]).
    /// With [`ExportSettings::intra_only`] it is intra: every picture an IDR,
    /// on the hardware seat where there is one and on the lanes below where
    /// there is not -- an intraframe master in the shape of ProRes or DNxHD,
    /// large files, every frame a cut point.
    ///
    /// The picture is coded at its own size and padded to whole minimum coding
    /// blocks *by the encoder*, which crops the padding back off with the SPS
    /// conformance window, so 1920x1080 is a legal export rather than a
    /// refusal.
    fn open_hevc(
        meta: &VideoMeta,
        settings: &ExportSettings,
        ten_bit: bool,
        colour: ColorDescription,
    ) -> crate::Result<Self> {
        // The muxers time by it and the mp4 one wants an exact rational: asked
        // here so an impossible rate fails before a frame is coded. The same
        // rational goes into the SPS's VUI, whose timing is also where the
        // encoder picks the level from.
        let (fps_num, fps_den) = crate::mux::frame_timing(meta.frame_rate)?;
        // 4:2:0 addresses its conformance window in *chroma* samples, so an odd
        // picture cannot be cropped back to itself -- and an odd dimension has
        // no chroma plane of its own to begin with. Named rather than padded to
        // something a decoder would then show a column of.
        if !meta.width.is_multiple_of(2) || !meta.height.is_multiple_of(2) {
            return Err(format!(
                "{}x{} cannot be written as HEVC: 4:2:0 needs even dimensions",
                meta.width, meta.height
            )
            .into());
        }
        // The GPU seat is 8-bit only, so a 10-bit export is software.
        let hw = match ten_bit {
            true => None,
            false => hw_seat(meta, settings)?,
        };
        if let Some(hw) = hw {
            if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export encoder: hardware HEVC intra (VA-API plugin)"); }
            return Ok(Self::HevcHw(hw));
        }
        if !settings.intra_only {
            if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export encoder: software HEVC I+P (ec-h265)"); }
            return Self::open_hevc_gop(meta, settings, (fps_num, fps_den), ten_bit, colour);
        }
        if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export encoder: software HEVC intra (ec-h265)"); }
        // The cores are split between the lanes and the wavefront inside one:
        // twelve lanes of one worker each on this box's twelve threads,
        // fewer lanes on a smaller machine with a wider wavefront each.
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let lane_count = cores.clamp(1, HEVC_LANES);
        let per_lane = cores.div_ceil(lane_count);
        let mut lanes = Vec::with_capacity(lane_count);
        for _ in 0..lane_count {
            let mut cfg = HevcEncoderConfig::new(meta.width, meta.height);
            cfg.bit_depth = if ten_bit { 10 } else { 8 };
            // The bitrate row a user picked, mapped through the same measured
            // anchor as before ([`hevc_qp`]) and held: a *quality* dial, not a
            // rate controller. The encoder's own per-picture target-bits mode
            // would chase the number with a correction pass -- named headroom,
            // not behaviour this seat takes silently.
            cfg.rate_control = RateControl::ConstantQp(hevc_qp(meta, settings));
            cfg.timing = Some((fps_den, fps_num));
            cfg.video_signal_type = Some(video_signal_type(colour));
            apply_hdr(&mut cfg, settings);
            cfg.speed = settings.hevc_speed.unwrap_or_default().preset();
            cfg.threads = per_lane;
            lanes.push(HevcEncoder::new(cfg).map_err(|e| format!("HEVC software encoder: {e}"))?);
        }
        Ok(Self::Hevc(HevcEnc {
            pending: Vec::new(),
            ready: std::collections::VecDeque::new(),
            au: Vec::new(),
            lanes,
            width: meta.width as usize,
            ten_bit,
        }))
    }

    /// The ProRes seat: the profile's encoder at the picture's size, tagged with
    /// the colour the file declares. Progressive, no alpha.
    fn open_prores(meta: &VideoMeta, settings: &ExportSettings, colour: ColorDescription) -> crate::Result<Self> {
        if !meta.width.is_multiple_of(2) || !meta.height.is_multiple_of(2) {
            return Err(format!(
                "{}x{} cannot be written as ProRes from a 4:2:0 picture: it needs even dimensions",
                meta.width, meta.height
            )
            .into());
        }
        let profile = settings.prores_profile;
        let mut cfg = ec_prores::EncoderConfig::new(profile.codec(), meta.width, meta.height);
        let (primaries, transfer, matrix) = colour.codes();
        cfg.color = ec_core::frame::ColorInfo {
            primaries: primaries as u8,
            transfer: transfer as u8,
            matrix: matrix as u8,
            full_range: false,
        };
        if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!("export encoder: software ProRes (ec-prores)"); }
        let encoder = ec_prores::ProResEncoder::new(cfg).map_err(|e| format!("ProRes encoder: {e}"))?;
        Ok(Self::ProRes { encoder, au: Vec::new(), full_chroma: profile.is_444() })
    }

    /// One 10-bit picture of 16-bit planes, at 10-bit scale, in and at most one
    /// access unit out. Asked only of the 10-bit seat, which [`wide_ten_bit`] opens
    /// with the same answer the export loop checks.
    fn encode_wide(
        &mut self,
        y: &[u16],
        u: &[u16],
        v: &[u16],
        width: u32,
        height: u32,
    ) -> crate::Result<Option<(&[u8], bool)>> {
        match self {
            Self::Av1Sw16 { context, ready, au, .. } => {
                submit_av1_16(context, ready, au, [y, u, v], width, height)
            }
            Self::HevcGop(gop) if gop.ten_bit => {
                gop.push_wide(y, u, v, width, height)?;
                Ok(pop_gop(gop))
            }
            Self::ProRes { encoder, au, full_chroma } => {
                let frame =
                    crate::prores_out::upsampled_frame(y, u, v, width as usize, height as usize, *full_chroma)?;
                *au = encoder.encode(&frame).map_err(|e| format!("ProRes encode: {e}"))?;
                Ok(Some((&au[..], true)))
            }
            Self::Hevc(hevc) if hevc.ten_bit => {
                hevc.pending.push((le_bytes(y), le_bytes(u), le_bytes(v)));
                if hevc.pending.len() >= hevc.lanes.len() {
                    hevc.code_batch()?;
                }
                Ok(pop_hevc(hevc))
            }
            _ => Err("a 10-bit picture needs a 10-bit seat".into()),
        }
    }

    /// Whether this is a software HEVC seat coding at 10 bits.
    fn hevc_ten_bit(&self) -> bool {
        match self {
            Self::HevcGop(gop) => gop.ten_bit,
            Self::Hevc(hevc) => hevc.ten_bit,
            _ => false,
        }
    }

    /// One picture in, at most one access unit out, and whether that unit is one
    /// a decoder may be started from -- which only the Matroska muxer asks, the
    /// mp4 one reading its own sync flag off the IDR slice.
    ///
    /// The H.264 seat keeps the contract structurally: `ec-h264` buffers
    /// nothing -- no lookahead, and B pictures coded as none -- so one call is
    /// one access unit by construction. The HEVC seat's lanes hold a small
    /// batch of pictures until there is a lane for each, so it says "at most
    /// one" and means it; the old H.264 seat only kept the contract because
    /// its GOP-buffering lookahead needed a zero bitrate and this path was
    /// always CBR.
    fn encode(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        width: u32,
        height: u32,
        intra: bool,
    ) -> crate::Result<Option<(&[u8], bool)>> {
        // A composed 8-bit picture on the 10-bit HEVC seat: shifted up to the
        // seat's scale, the same shift the AV1 10-bit seat makes.
        if self.hevc_ten_bit() {
            let up = |p: &[u8]| p.iter().map(|&v| u16::from(v) << 2 | u16::from(v) >> 6).collect::<Vec<u16>>();
            let (y, u, v) = (up(y), up(u), up(v));
            return self.encode_wide(&y, &u, &v, width, height);
        }
        if let Self::ProRes { encoder, au, full_chroma } = self {
            // A composed 8-bit picture: widened to 10 bits and upsampled in one pass.
            let frame =
                crate::prores_out::upsampled_frame_8(y, u, v, width as usize, height as usize, *full_chroma)?;
            *au = encoder.encode(&frame).map_err(|e| format!("ProRes encode: {e}"))?;
            return Ok(Some((&au[..], true)));
        }
        match self {
            Self::Av1Hw(hw) => {
                let au = hw.encode(y, u, v, width, height, false)?;
                // What the *bitstream* says, not what was asked for: an AV1
                // keyframe carries the sequence header, which is the same mark
                // `demux` reads a sync point off. A driver that emitted one
                // unasked is then still a seek target, and one that skipped a
                // requested keyframe cannot be mistaken for a decoder's start.
                Ok(au.map(|au| (au, crate::mux::av1_sequence_header(au).is_some())))
            }
            Self::Av1Sw16 {
                context,
                ready,
                au,
                scratch,
                ..
            } => {
                // An 8-bit picture on the 10-bit seat: each sample scaled up to
                // the same range (255 maps to 1023, not 1020).
                let (w, h) = (width as usize, height as usize);
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                for (dst, (src, n)) in scratch.iter_mut().zip([(y, w * h), (u, cw * ch), (v, cw * ch)]) {
                    dst.clear();
                    dst.extend(src[..n].iter().map(|&s| {
                        let s = u16::from(s);
                        (s << 2) | (s >> 6)
                    }));
                }
                let [sy, su, sv] = &*scratch;
                submit_av1_16(context, ready, au, [&sy[..], &su[..], &sv[..]], width, height)
            }
            Self::Av1Sw {
                context, ready, au, ..
            } => {
                let mut frame = context.new_frame();
                let (w, h) = (width as usize, height as usize);
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                frame.planes[0].copy_from_raw_u8(&y[..w * h], w, 1);
                frame.planes[1].copy_from_raw_u8(&u[..cw * ch], cw, 1);
                frame.planes[2].copy_from_raw_u8(&v[..cw * ch], cw, 1);
                if let Err(e) = context.send_frame(frame) {
                    return Err(format!("software AV1 encode: {e}").into());
                }
                collect_av1(context, ready)?;
                Ok(pop_av1(ready, au))
            }
            // Every unit this seat hands back is an IDR carrying its own
            // parameter sets (the plugin codes HEVC intra-only), so every one of
            // them is a sync point -- said as a constant rather than parsed back
            // out of the bitstream, because it is this encoder's configuration
            // and not a driver's choice.
            Self::HevcHw(hw) => Ok(hw
                .encode(y, u, v, width, height, true)?
                .map(|au| (au, true))),
            Self::Hevc(hevc) => {
                // The display-size planes are kept as they came: `ec-h265`
                // pads to its own coded grid and crops back through the SPS
                // conformance window.
                hevc.pending.push((y.to_vec(), u.to_vec(), v.to_vec()));
                if hevc.pending.len() >= hevc.lanes.len() {
                    hevc.code_batch()?;
                }
                // Every intra AU is an IDR, so every one of them is a key
                // frame -- which is what an intraframe master means.
                Ok(pop_hevc(hevc))
            }
            Self::HevcGop(gop) => {
                gop.push(y, u, v, width, height)?;
                Ok(pop_gop(gop))
            }
            // A key frame is asked for on every picture where the file is an
            // intraframe one, and the flag says what was asked: the mp4 muxer
            // reads its own sync marks off the IDR slices either way, so this
            // is what a Matroska one would be told.
            Self::Hw(hw) => Ok(hw
                .encode(y, u, v, width, height, intra)?
                .map(|au| (au, intra))),
            Self::ProRes { .. } => unreachable!("the ProRes seat is handled before this match"),
            Self::Sw { encoder, au } => {
                // The key flag is what the bitstream says -- an IDR where the
                // GOP asked for one, every picture on an intraframe export --
                // which is what the hardware seat's `intra` carries too. The
                // planes go in as borrows; the old seat was handed a copy.
                let view = PictureView::i420(width, height, y, u, v);
                let coded = encoder
                    .encode(&view)
                    .map_err(|e| format!("software encode: {e}"))?;
                *au = coded.au;
                Ok(Some((&au[..], coded.key_frame)).filter(|(au, _)| !au.is_empty()))
            }
        }
    }

    /// What a decoded buffer must look like to reach this seat without ever
    /// being read back -- `None` for every seat that reads bytes, which is both
    /// software encoders and the intra HEVC one.
    ///
    /// The AV1 hardware seat is left out on purpose: it is entered only when a
    /// person picks it by name because the vendored encoder reset this project's GPU
    /// ([`Enc::open_av1`]), and a path that cannot be measured is not one to
    /// widen.
    /// `&mut` because an isolated seat has to *ask* the process it lives in
    /// ([`crate::hwproc`]), which is a round trip on its socket.
    fn dma_want(&mut self, meta: &VideoMeta) -> Option<crate::hw::DmaWant> {
        match self {
            Self::Hw(hw) | Self::HevcHw(hw) => hw.dma_want(meta.width, meta.height),
            _ => None,
        }
    }

    /// One picture the decoder left on the GPU in, at most one access unit out.
    /// Same answer as [`Enc::encode`], including which units a decoder may be
    /// started from, because it is the same encoder -- only the picture took
    /// another way in.
    ///
    /// `intra` therefore carries here too, and not for symmetry's sake: a proxy
    /// of a file already small enough is coded at that file's own size
    /// ([`crate::proxy::size_for`]), which is the one case where an intraframe
    /// export's decoder and encoder agree on a surface and the buffer goes
    /// straight across. An access unit marked as no sync point there would give
    /// a stand-in whose whole reason to exist is that every frame is one.
    fn encode_dma(&mut self, dma: &DmaFrame, intra: bool) -> crate::Result<Option<(&[u8], bool)>> {
        match self {
            Self::Hw(hw) => Ok(hw.encode_dma(dma, intra)?.map(|au| (au, intra))),
            // Every picture an IDR here, exactly as in [`Enc::encode`].
            Self::HevcHw(hw) => Ok(hw.encode_dma(dma, true)?.map(|au| (au, true))),
            // Unreachable through [`Enc::dma_want`], which is the only thing
            // that puts a span's decoder on this path.
            _ => Err("this encoder takes pictures as bytes, not as buffers".into()),
        }
    }

    /// End of stream; call until it returns `None`.
    fn drain(&mut self) -> crate::Result<Option<(&[u8], bool)>> {
        match self {
            Self::Av1Hw(hw) => {
                let au = hw.drain()?;
                Ok(au.map(|au| (au, crate::mux::av1_sequence_header(au).is_some())))
            }
            Self::Av1Sw16 {
                context,
                ready,
                au,
                flushed,
                ..
            } => {
                if !*flushed {
                    *flushed = true;
                    context.flush();
                    collect_av1(context, ready)?;
                }
                Ok(pop_av1(ready, au))
            }
            Self::Av1Sw {
                context,
                ready,
                au,
                flushed,
            } => {
                if !*flushed {
                    *flushed = true;
                    context.flush();
                    collect_av1(context, ready)?;
                }
                Ok(pop_av1(ready, au))
            }
            Self::HevcHw(hw) => Ok(hw.drain()?.map(|au| (au, true))),
            // The tail batch: fewer frames than lanes, coded on as many
            // encoders as there are frames left.
            Self::Hevc(hevc) => {
                if !hevc.pending.is_empty() {
                    hevc.code_batch()?;
                }
                Ok(pop_hevc(hevc))
            }
            Self::HevcGop(gop) => {
                gop.finish()?;
                Ok(pop_gop(gop))
            }
            Self::Hw(hw) => Ok(hw.drain()?.map(|au| (au, false))),
            // Nothing is held back: every picture `encode` took already came
            // back with its access unit, so end of stream is the empty answer.
            Self::Sw { .. } | Self::ProRes { .. } => Ok(None),
        }
    }
}

impl HevcEnc {
    /// The whole pending batch, one frame per lane, collected back in the
    /// order it went out -- `std::thread::scope`, because the encode borrows
    /// the planes and the lanes rather than copying them again. A panic inside
    /// a lane comes back as an error rather than unwinding the export thread.
    fn code_batch(&mut self) -> crate::Result<()> {
        let width = self.width;
        let chroma = width / 2;
        let pending = std::mem::take(&mut self.pending);
        let ten_bit = self.ten_bit;
        let coded: Vec<crate::Result<Vec<u8>>> = std::thread::scope(|scope| {
            let lanes: Vec<_> = self
                .lanes
                .iter()
                .zip(pending.iter())
                .map(|(encoder, (y, cb, cr))| {
                    scope.spawn(move || {
                        let coded = if ten_bit {
                            let samples = |p: &[u8]| -> Vec<u16> {
                                p.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
                            };
                            let (y, cb, cr) = (samples(y), samples(cb), samples(cr));
                            encoder.encode_idr_samples(&y, width, &cb, chroma, &cr, chroma)
                        } else {
                            encoder.encode_idr_planes(y, width, cb, chroma, cr, chroma)
                        };
                        coded
                            .map(|coded| coded.au)
                            .map_err(|e| crate::Error::from(format!("software HEVC encode: {e}")))
                    })
                })
                .collect();
            lanes
                .into_iter()
                .map(|lane| {
                    lane.join()
                        .unwrap_or_else(|_| Err("an HEVC encode lane panicked".into()))
                })
                .collect()
        });
        for au in coded {
            self.ready.push_back(au?);
        }
        Ok(())
    }
}

/// The oldest coded intra AU, lent out under the same "valid until the next
/// call" contract [`pop_av1`] has. Every one of them is an IDR, so the key
/// flag is not a guess.
fn pop_hevc(hevc: &mut HevcEnc) -> Option<(&[u8], bool)> {
    hevc.au = hevc.ready.pop_front()?;
    Some((&hevc.au[..], true))
}

/// The longest GOP the HEVC seat asks for, in pictures. `ec-h265` codes GOPs
/// concurrently only up to this length, and the sequence coder's own limit is
/// 256; a 60 fps export is 60 pictures and a faster one takes the serial path.
const HEVC_GOP_MAX_KEYINT: usize = 64;

impl Enc {
    /// The HEVC I+P seat: `ec-h265`'s sequence coder, an IDR once a second at
    /// the picture's own rate with P pictures between, on a worker thread.
    ///
    /// The export walk pushes one picture at a time and the encoder pulls its
    /// frames from an iterator, so the two meet on a bounded channel: the walk
    /// is never more than one GOP ahead of the worker. The encoder codes whole
    /// GOPs concurrently on [`default_threads`](ec_h265::encoder::default_threads)
    /// workers, which is at most six, and hands the pictures back in order.
    ///
    /// Rate: an explicit bitrate row goes to bitrate control at that rate, the
    /// same number the H.264 seat codes at. No bitrate is the automatic quality
    /// row, constant QP at [`HEVC_GOP_QP`]: a quality default of its own, not
    /// the intra seat's [`hevc_qp`], whose anchor is measured on intra pictures.
    fn open_hevc_gop(
        meta: &VideoMeta,
        settings: &ExportSettings,
        (fps_num, fps_den): (u32, u32),
        ten_bit: bool,
        colour: ColorDescription,
    ) -> crate::Result<Self> {
        let keyint = hevc_gop_keyint(meta.frame_rate);
        let mut cfg = HevcEncoderConfig::new(meta.width, meta.height);
        cfg.bit_depth = if ten_bit { 10 } else { 8 };
        cfg.rate_control = match settings.bitrate {
            Some(_) => RateControl::Bitrate {
                kbps: (bitrate_of(meta, settings) / 1000).max(1) as u32,
                fps: meta.frame_rate,
            },
            None => RateControl::ConstantQp(HEVC_GOP_QP),
        };
        cfg.keyint = keyint;
        cfg.threads = ec_h265::encoder::default_threads();
        cfg.timing = Some((fps_den, fps_num));
        cfg.video_signal_type = Some(video_signal_type(colour));
        apply_hdr(&mut cfg, settings);
        cfg.speed = settings.hevc_speed.unwrap_or_default().preset();
        let encoder = HevcEncoder::new(cfg).map_err(|e| format!("HEVC software encoder: {e}"))?;
        let (frames_tx, frames_rx) = sync_channel::<VideoFrame>(keyint);
        let (au_tx, coded) = std::sync::mpsc::channel::<(Vec<u8>, bool)>();
        // The P pictures refer to a second picture parameter set, which the
        // samples do not keep: the first unit carries it, so the record the
        // container builds from that unit holds it.
        let p_pps = encoder.p_picture_parameter_set();
        let worker = thread::Builder::new()
            .name("ec-h265-gop".into())
            .spawn(move || {
                let mut picture = 0usize;
                encoder
                    .encode_sequence_with(frames_rx, |pic| {
                        // Every GOP opens on an IDR, so a picture's key flag is
                        // its place in the sequence.
                        let key = picture.is_multiple_of(keyint);
                        let au = if picture == 0 {
                            with_parameter_set(pic.au, &p_pps)
                        } else {
                            pic.au
                        };
                        picture += 1;
                        // A closed receiver is an export dropped part way: the
                        // rest is unwanted, and draining it is not an error.
                        let _ = au_tx.send((au, key));
                        Ok(())
                    })
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| format!("HEVC software encoder: {e}"))?;
        Ok(Self::HevcGop(HevcGop {
            frames: Some(frames_tx),
            worker: Some(worker),
            coded,
            ready: std::collections::VecDeque::new(),
            au: Vec::new(),
            ten_bit,
        }))
    }
}

/// The GOP the I+P seat codes at this frame rate: an IDR about a second apart.
fn hevc_gop_keyint(frame_rate: f64) -> usize {
    (frame_rate.round() as usize).clamp(2, HEVC_GOP_MAX_KEYINT)
}

/// What an HEVC export codes as, in the words an export start names: the I+P
/// GOP and its key interval and rate, or `intra-only`. `None` for any other
/// format. The rate is the bitrate the file aims at, or, with no bitrate given,
/// the quality mode the automatic row codes at: constant QP ([`HEVC_GOP_QP`]), named
/// with its number and with how to ask for a fixed rate instead.
pub fn hevc_mode_words(meta: &VideoMeta, settings: &ExportSettings) -> Option<String> {
    if !settings.format.is_hevc() {
        return None;
    }
    if settings.intra_only {
        return Some(match settings.bitrate {
            Some(_) => "intra-only".to_string(),
            None => format!(
                "intra-only, constant quality QP {} on the software seat (the GPU seat codes at the automatic rate); pass bitrate_kbps for a fixed rate",
                hevc_qp(meta, settings)
            ),
        });
    }
    let seconds = hevc_gop_keyint(meta.frame_rate) as f64 / meta.frame_rate;
    let every = if (seconds - seconds.round()).abs() < 1e-6 {
        format!("{} s", seconds.round() as u64)
    } else {
        format!("{seconds:.2} s")
    };
    let rate = match settings.bitrate {
        Some(_) => rate_words(bitrate_of(meta, settings) / 1000),
        None => format!("constant quality QP {HEVC_GOP_QP}; pass bitrate_kbps for a fixed rate"),
    };
    Some(format!("I+P GOP ({every} keyframes), {rate}"))
}

/// What an AV1 export codes as, in the words an export start names. AV1 is always
/// coded at a bitrate, so with none given the automatic target is named as one
/// (chosen from the picture size and frame rate). `None` for any other format.
pub fn av1_mode_words(meta: &VideoMeta, settings: &ExportSettings) -> Option<String> {
    if !settings.format.is_av1() {
        return None;
    }
    let rate = rate_words(bitrate_of(meta, settings) / 1000);
    Some(match settings.bitrate {
        Some(_) => format!("average bitrate {rate}"),
        None => format!("average bitrate {rate} chosen from the picture size and frame rate; pass bitrate_kbps for a fixed rate"),
    })
}

/// A rate in kilobits a second, in the words a reply uses: `4 Mb/s` on a whole megabit, `6.2 Mb/s`
/// above one, else `850 kb/s`.
fn rate_words(kbps: u64) -> String {
    if kbps.is_multiple_of(1000) {
        format!("{} Mb/s", kbps / 1000)
    } else if kbps >= 1000 {
        format!("{:.1} Mb/s", kbps as f64 / 1000.0)
    } else {
        format!("{kbps} kb/s")
    }
}

/// The words an export start names when the source's chroma is reduced to 4:2:0 on
/// the way out, e.g. `VP9 4:4:4 source: chroma reduced to 4:2:0 for export`. `codec`
/// and `chroma` are the source's; the chroma is read off the stream
/// ([`Demuxer::chroma`]). `None` where nothing is reduced: a 4:2:0 or grey source,
/// or a sound-only format, which has no picture to reduce.
pub fn chroma_reduction_words(codec: Codec, chroma: Chroma, settings: &ExportSettings) -> Option<String> {
    let format = match chroma {
        Chroma::C422 => "4:2:2",
        Chroma::C444 => "4:4:4",
        _ => return None,
    };
    if !settings.format.has_video() {
        return None;
    }
    Some(format!("{} {format} source: chroma reduced to 4:2:0 for export", codec.name()))
}

/// The I+P seat's state: the worker running the sequence coder, the channel
/// the frames go to it on, and the access units it has handed back.
struct HevcGop {
    /// The frames for the worker, in display order. `None` once the sequence
    /// has ended, which is what lets the worker finish its last GOP.
    frames: Option<SyncSender<VideoFrame>>,
    worker: Option<thread::JoinHandle<Result<(), String>>>,
    /// Access units and their key flags, in coding order, which is display order.
    coded: Receiver<(Vec<u8>, bool)>,
    /// Units collected from `coded` and not yet handed to the caller.
    ready: std::collections::VecDeque<(Vec<u8>, bool)>,
    /// The unit currently lent out, for the reason `Sw` owns one.
    au: Vec<u8>,
    /// Whether the sequence codes at 10 bits: its frames are I010 then.
    ten_bit: bool,
}

impl HevcGop {
    fn push(&mut self, y: &[u8], u: &[u8], v: &[u8], width: u32, height: u32) -> crate::Result<()> {
        let frame = i420_frame(y, u, v, width, height)?;
        self.send(frame)
    }

    /// One 10-bit picture of 16-bit samples, as the I010 frame the 10-bit sequence takes.
    fn push_wide(&mut self, y: &[u16], u: &[u16], v: &[u16], width: u32, height: u32) -> crate::Result<()> {
        let frame = i010_frame(y, u, v, width, height)?;
        self.send(frame)
    }

    fn send(&mut self, frame: VideoFrame) -> crate::Result<()> {
        if self
            .frames
            .as_ref()
            .is_some_and(|tx| tx.send(frame).is_ok())
        {
            self.collect();
            return Ok(());
        }
        // The worker has stopped on its own: its error is the answer.
        self.finish()?;
        Err("the HEVC encoder stopped before the export ended".into())
    }

    /// Ends the sequence and waits for the worker's last GOP.
    fn finish(&mut self) -> crate::Result<()> {
        self.frames = None;
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(format!("software HEVC encode: {e}").into()),
                Err(_) => return Err("the HEVC encoder thread panicked".into()),
            }
        }
        self.collect();
        Ok(())
    }

    fn collect(&mut self) {
        while let Ok(unit) = self.coded.try_recv() {
            self.ready.push_back(unit);
        }
    }
}

/// One I420 picture at the display size, as the sequence coder takes it.
fn i420_frame(y: &[u8], u: &[u8], v: &[u8], width: u32, height: u32) -> crate::Result<VideoFrame> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let planes = vec![
        ec_core::frame::Plane::new(y[..w * h].to_vec(), w),
        ec_core::frame::Plane::new(u[..cw * ch].to_vec(), cw),
        ec_core::frame::Plane::new(v[..cw * ch].to_vec(), cw),
    ];
    VideoFrame::try_new(ec_core::frame::PixelFormat::I420, width, height, planes)
        .map_err(|e| format!("HEVC frame: {e}").into())
}

/// One 10-bit I010 picture at the display size: 16-bit little-endian samples, the
/// stride in bytes, as the sequence coder takes it.
fn i010_frame(y: &[u16], u: &[u16], v: &[u16], width: u32, height: u32) -> crate::Result<VideoFrame> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let planes = vec![
        ec_core::frame::Plane::new(le_bytes(&y[..w * h]), w * 2),
        ec_core::frame::Plane::new(le_bytes(&u[..cw * ch]), cw * 2),
        ec_core::frame::Plane::new(le_bytes(&v[..cw * ch]), cw * 2),
    ];
    VideoFrame::try_new(ec_core::frame::PixelFormat::I010, width, height, planes)
        .map_err(|e| format!("HEVC frame: {e}").into())
}

/// Little-endian bytes of 16-bit samples.
fn le_bytes(samples: &[u16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// `au` with `nal` (one Annex-B unit) placed before its first coded slice, so
/// after the parameter sets that open the unit. `au` comes back as it went in
/// where it has no slice.
fn with_parameter_set(au: Vec<u8>, nal: &[u8]) -> Vec<u8> {
    let starts = (0..au.len().saturating_sub(3)).filter(|&i| au[i..i + 3] == [0, 0, 1]);
    for start in starts {
        // The NAL unit type is the six bits after the forbidden bit.
        if (au[start + 3] >> 1) & 0x3F < 32 {
            // A four-byte start code keeps its leading zero in front of the new unit.
            let at = if start > 0 && au[start - 1] == 0 {
                start - 1
            } else {
                start
            };
            let mut out = Vec::with_capacity(au.len() + nal.len());
            out.extend_from_slice(&au[..at]);
            out.extend_from_slice(nal);
            out.extend_from_slice(&au[at..]);
            return out;
        }
    }
    au
}

fn pop_gop(gop: &mut HevcGop) -> Option<(&[u8], bool)> {
    let (au, key) = gop.ready.pop_front()?;
    gop.au = au;
    Some((&gop.au[..], key))
}
/// The quantiser an intra HEVC export codes at, mapped from the *bitrate* row a
/// user picked -- the card has no QP control and this codec has no rate control,
/// so the two are joined by measurement rather than promised to each other.
///
/// The encoder codes ~0.6 bits per pixel at QP 27 ([`HEVC_BPP_AT_27`], measured
/// on the 720p and 1080p benches to within 4% of each other), and a step of 6 QP
/// is a factor of two in rate, which is the classic HEVC relation. So the QP
/// that would land on a target is `27 + 6 * log2(0.6 / target)` -- and it is
/// clamped, because this is a *quality* dial and not a rate controller: below QP
/// 22 the file grows without a picture to show for it, and past QP 40 an
/// intra-only frame goes blocky in a way no bitrate row asked for. A 1080p30
/// timeline at the automatic 6.2 Mbps therefore codes at QP 40 and comes out
/// bigger than that, which is what "intra-only, large files" on the format row
/// means: the row buys quality, not a size.
fn hevc_qp(meta: &VideoMeta, settings: &ExportSettings) -> i32 {
    let pixels = f64::from(meta.width) * f64::from(meta.height) * meta.frame_rate;
    let target = bitrate_of(meta, settings) as f64 / pixels.max(1.0);
    let qp = 27.0 + 6.0 * (HEVC_BPP_AT_27 / target.max(f64::MIN_POSITIVE)).log2();
    (qp.round() as i32).clamp(HEVC_QP_MIN, HEVC_QP_MAX)
}

/// What the *bitstream* says the samples mean, in the words a sequence header
/// spells them -- the same H.273 code points [`crate::mux`] writes into the
/// container, from the same colour the export declares there ([`source_sdr_colour`] or the
/// [`ColorDescription::output`] rule), so the two halves of a file cannot drift apart.
///
/// This exists because a container tag is not enough: libavcodec takes the
/// bitstream's answer first, and a stream that says nothing is "unspecified",
/// which renders BT.601 -- a visible shift on a 709 export that a player reads
/// from the wrong matrix. Every export is limited range, which is what
/// `video_full_range_flag = false` says here.
fn video_signal_type(colour: ColorDescription) -> HevcVideoSignalType {
    let (primaries, transfer, matrix) = colour.codes();
    HevcVideoSignalType {
        // "Unspecified", which is what every encoder here writes: the field is
        // the analogue system a picture came off, and none of these did.
        video_format: 5,
        video_full_range_flag: false,
        colour_description: Some(HevcColourDescription {
            colour_primaries: primaries as u8,
            transfer_characteristics: transfer as u8,
            matrix_coeffs: matrix as u8,
        }),
    }
}

/// The HDR extras of an HEVC export, in the bitstream: BT.2020's chroma sample
/// location (type 2, top-left) in the VUI, and the mastering display and content
/// light SEIs where [`ExportSettings::hdr`] gives them. Nothing for an SDR export,
/// whose stream stays byte-identical to what it was.
fn apply_hdr(cfg: &mut HevcEncoderConfig, settings: &ExportSettings) {
    let hdr = &settings.hdr;
    if hdr.transfer == HdrTransfer::Sdr {
        return;
    }
    cfg.chroma_sample_loc = Some((2, 2));
    cfg.mastering_display = hdr.mastering_display.map(|m| ec_h265_syntax::sei::MasteringDisplay {
        display_primaries: m.primaries,
        white_point: m.white_point,
        max_luminance: m.max_luminance,
        min_luminance: m.min_luminance,
    });
    cfg.content_light.max_cll = hdr.max_cll.map(f32::from);
    cfg.content_light.max_fall = hdr.max_fall.map(f32::from);
}

/// The colour a 10-bit export declares when every clip is SDR in one matrix: that
/// matrix, limited range, the source's own rather than the output rule's. `None`
/// for a mixed timeline, an HDR one or an empty one, which keep the rule.
fn source_sdr_colour(colours: &[ColorDescription]) -> Option<ColorDescription> {
    let first = *colours.first()?;
    let same = colours
        .iter()
        .all(|c| c.transfer == Transfer::Sdr && c.matrix == first.matrix);
    (same && first.transfer == Transfer::Sdr).then_some(ColorDescription {
        matrix: first.matrix,
        transfer: Transfer::Sdr,
        full_range: false,
    })
}

/// Whether a software export is written at 10 bits: the setting where it names a
/// depth, the source's own where it does not. AV1 and HEVC have the choice --
/// H.264 is 8-bit here -- and a depth the file cannot be is refused. The HEVC
/// hardware seat is 8-bit, so a 10-bit HEVC export falls back to software
/// ([`open_hevc`]).
fn wide_ten_bit(
    settings: &ExportSettings,
    source_ten_bit: impl FnOnce() -> bool,
) -> crate::Result<bool> {
    // ProRes is coded at 10 bits whatever the setting says (it has no 8-bit
    // sample), so the walk keeps 10-bit sources at 10 bits.
    if settings.format.is_prores() {
        return Ok(true);
    }
    if !settings.format.is_av1() && !settings.format.is_hevc() {
        return Ok(false);
    }
    // An HDR transfer is a 10-bit claim: 8 bits of PQ bands visibly.
    if settings.hdr.transfer != HdrTransfer::Sdr {
        return match settings.video_bit_depth {
            Some(8) => Err("an HDR export is 10-bit: PQ and HLG are not carried in 8 bits".into()),
            Some(10) | None => Ok(true),
            Some(bits) => Err(format!("an AV1 or HEVC export is 8 or 10 bits, not {bits}").into()),
        };
    }
    match settings.video_bit_depth {
        None => Ok(source_ten_bit()),
        Some(8) => Ok(false),
        Some(10) => Ok(true),
        Some(bits) => Err(format!("an AV1 or HEVC export is 8 or 10 bits, not {bits}").into()),
    }
}

/// The colour an HDR HEVC export declares, or `None` for an SDR export; `Err`
/// where the settings ask for HDR of something that cannot carry it.
fn hdr_colour_of(settings: &ExportSettings) -> crate::Result<Option<ColorDescription>> {
    let hdr = &settings.hdr;
    let Some(colour) = hdr.transfer.colour() else {
        if hdr.mastering_display.is_some() || hdr.max_cll.is_some() || hdr.max_fall.is_some() {
            return Err("mastering display and MaxCLL/MaxFALL need an HDR transfer (PQ or HLG)".into());
        }
        return Ok(None);
    };
    if !settings.format.is_hevc() {
        return Err(format!("{} cannot be written as HDR: only HEVC is", settings.format.name()).into());
    }
    if settings.keep_source_colour {
        return Err("an HDR export cannot also keep its source's colour".into());
    }
    Ok(Some(colour))
}

fn transfer_name(t: Transfer) -> &'static str {
    match t {
        Transfer::Sdr => "SDR",
        Transfer::Pq => "PQ",
        Transfer::Hlg => "HLG",
    }
}

/// Every temporal unit `rav1e` has finished, moved into the queue in display
/// order. `NeedMoreData` (send another picture) and `LimitReached` (the flush is
/// through) are the two ways it says there is nothing more; `Encoded` means a
/// picture was coded but not yet packetised, which is a *keep asking* -- taking
/// it for an end leaves the tail of the export inside the encoder.
fn collect_av1<T: rav1e::prelude::Pixel>(
    context: &mut rav1e::Context<T>,
    ready: &mut std::collections::VecDeque<(Vec<u8>, bool)>,
) -> crate::Result<()> {
    use rav1e::prelude::{EncoderStatus, FrameType};
    loop {
        match context.receive_packet() {
            Ok(packet) => ready.push_back((packet.data, packet.frame_type == FrameType::KEY)),
            Err(EncoderStatus::Encoded) => {}
            Err(EncoderStatus::NeedMoreData | EncoderStatus::LimitReached) => return Ok(()),
            Err(e) => return Err(format!("software AV1 encode: {e}").into()),
        }
    }
}

/// The oldest finished unit, moved into the buffer that is lent out -- the same
/// "valid until the next call" contract the plugin's slice has.
fn pop_av1<'a>(
    ready: &mut std::collections::VecDeque<(Vec<u8>, bool)>,
    au: &'a mut Vec<u8>,
) -> Option<(&'a [u8], bool)> {
    let (unit, key) = ready.pop_front()?;
    *au = unit;
    Some((&au[..], key))
}

/// One picture of 16-bit planes into the 10-bit `rav1e` context, and whatever
/// unit it finished. The planes are the picture's own size, 4:2:0.
fn submit_av1_16<'a>(
    context: &mut rav1e::Context<u16>,
    ready: &mut std::collections::VecDeque<(Vec<u8>, bool)>,
    au: &'a mut Vec<u8>,
    planes: [&[u16]; 3],
    width: u32,
    height: u32,
) -> crate::Result<Option<(&'a [u8], bool)>> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut frame = context.new_frame();
    // A plane's rows sit `stride` apart from its origin, past any padding.
    let fill = |plane: &mut rav1e::prelude::Plane<u16>, src: &[u16], w: usize, h: usize| {
        let stride = plane.cfg.stride;
        let dst = plane.data_origin_mut();
        for (row, line) in src.chunks_exact(w).take(h).enumerate() {
            dst[row * stride..row * stride + w].copy_from_slice(line);
        }
    };
    fill(&mut frame.planes[0], &planes[0][..w * h], w, h);
    fill(&mut frame.planes[1], &planes[1][..cw * ch], cw, ch);
    fill(&mut frame.planes[2], &planes[2][..cw * ch], cw, ch);
    if let Err(e) = context.send_frame(frame) {
        return Err(format!("software AV1 encode: {e}").into());
    }
    collect_av1(context, ready)?;
    Ok(pop_av1(ready, au))
}

/// A 10-bit plane narrowed to 8 bits for the composition that runs at 8 bits: the
/// same truncation [`crate::decode::narrow_av1`] makes on playback.
fn narrow_10(src: &[u16], dst: &mut Vec<u8>) {
    dst.clear();
    dst.extend(src.iter().map(|&s| (s >> 2) as u8));
}

/// The frame as an 8-bit composition takes it: a 10-bit picture narrowed into
/// `scratch`, anything else as it came.
fn narrowed<'a>(frame: Frame<'a>, scratch: &'a mut [Vec<u8>; 3]) -> Frame<'a> {
    match frame {
        Frame::Wide(y, u, v, width, height) => {
            let [sy, su, sv] = scratch;
            narrow_10(y, sy);
            narrow_10(u, su);
            narrow_10(v, sv);
            Frame::Pixels(&sy[..], &su[..], &sv[..], width, height)
        }
        other => other,
    }
}

/// I420 straight out of the decoder: the export never converts to BGRA and back,
/// which would cost two conversions and a generation of colour precision.
///
/// This mirrors `decode`'s two worker loops rather than reusing `DecodeSession`,
/// which only speaks BGRA. A hardware decode that fails *before* the first
/// picture falls back to software, exactly as playback's does -- nothing has
/// been encoded yet, so the span can be taken over cleanly. One that fails
/// after the first picture fails the export, which then deletes the
/// half-written file: a restart there would re-encode what the muxer holds.
enum ClipDecoder {
    /// A stream, decoded on a thread of its own: the encode of picture *n* and
    /// the decode of picture *n + 1* are the two halves an export used to do in
    /// turns, each idle while the other worked. The channel is two pictures
    /// deep, so the decoder runs no further ahead than that (a 1080p I420
    /// picture is 3.1 MB) and a slow encoder still stops it.
    ///
    /// The thread is also where the plugin is *opened*: a VA-API session is not
    /// ours to hand between threads ([`crate::decode::open_worker_deferred`]
    /// keeps the same rule), which is why the open's error comes back down the
    /// channel rather than out of [`ClipDecoder::open`].
    Streamed(Streamed),
    /// A still image: one picture, handed out for as long as the span runs.
    /// It is decoded here rather than in `run`'s loop for the same reason the
    /// other two are opened there -- the span's pictures come from one place,
    /// and no thread can make one picture arrive sooner.
    Still(crate::decode::Still),
    /// A clip played backwards ([`Reversed`]): its pictures, last to first.
    Reversed(Box<Reversed>),
    /// An audio file playing as a picture: each `next` paints one visualizer
    /// frame from peaks already decoded ([`crate::decode::visualizer_i420`]).
    Visualizer {
        peaks: Vec<(f32, f32)>,
        width: u32,
        height: u32,
        fps: f64,
        next: u32,
        flags: u8,
        paint: u64,
        /// This seat's own painting scratch, kept for the whole export: the
        /// same reuse the playback worker's thread does.
        scratch: crate::decode::VizScratch,
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
    },
}

/// One 8-bit picture owned: its three planes and the size.
type Owned = (Vec<u8>, Vec<u8>, Vec<u8>, u32, u32);

/// A clip's pictures in reverse ([`ClipDecoder::reversed`]).
struct Reversed {
    path: PathBuf,
    walk: crate::reverse::Walk,
    /// The block being served, oldest first: the next picture is the last one.
    block: Vec<Owned>,
    /// The picture last handed out, which `next` lends.
    current: Option<Owned>,
    /// The clip's deinterlace setting; `Field` makes the numbers above fields.
    deint: Deinterlace,
}

impl Reversed {
    fn next(&mut self) -> crate::Result<Option<Frame<'_>>> {
        if self.block.is_empty() && !self.fill()? {
            return Ok(None);
        }
        self.current = self.block.pop();
        Ok(self
            .current
            .as_ref()
            .map(|(y, u, v, w, h)| Frame::Pixels(y, u, v, *w, *h)))
    }

    /// Decodes the next block down, forwards, into `block`. `false` when the window
    /// has no frames left, or the file gave none.
    fn fill(&mut self) -> crate::Result<bool> {
        let Some((start, end)) = self.walk.next() else {
            return Ok(false);
        };
        let mut decoder = ClipDecoder::open_with(&self.path, start, None, None, false, self.deint)?;
        for _ in start..end {
            match decoder.next()? {
                Some(Frame::Pixels(y, u, v, w, h)) => {
                    if self.block.is_empty() {
                        // The pictures' real size is known now: hold as many as the
                        // budget allows from the next block on.
                        let bytes = y.len() + u.len() + v.len();
                        self.walk.set_cap(crate::reverse::block_cap(bytes));
                    }
                    self.block.push((y.to_vec(), u.to_vec(), v.to_vec(), w, h));
                }
                Some(_) => return Err("a reversed clip's decoder produced no pixels".into()),
                None => break,
            }
        }
        Ok(!self.block.is_empty())
    }
}

/// The planes of one 10-bit picture, borrowed from its seat: luma, the two
/// chroma planes, and the size.
type Wide16View<'a> = (&'a [u16], &'a [u16], &'a [u16], u32, u32);

/// The receiving half of a decode thread, plus the picture it last handed out
/// -- which is what lets `next` keep lending planes rather than moving them.
///
/// The first two fields are in this order on purpose, and it is the order
/// [`crate::decode::FrameStream`] states for the same pair: Rust drops fields as
/// they are declared, so the receiver disconnects *first* -- the only thing that
/// wakes a decoder parked in a full `send` -- and the join inside
/// [`DecodeThread::drop`] runs second. The other way round is a hang and not a
/// slow path.
struct Streamed {
    frames: Receiver<crate::Result<Option<Yuv>>>,
    /// Held only to be dropped: joining is all it does, and it must happen
    /// after the receiver above (`_lib` in `crate::hw` is kept the same way).
    /// Its result is the software decoder a stopped span leaves behind, if any.
    _thread: DecodeThread<Option<Kept>>,
    current: Option<Yuv>,
    /// Whether the thread has said "no more pictures" in as many words. What
    /// separates a stream that ended from a thread that vanished: after this,
    /// the channel is disconnected because the decoder is *done*, and a further
    /// ask is end of stream rather than a failure.
    ended: bool,
    /// Where the decoder goes when this span is done with it, if it may be kept.
    park: Option<Park>,
}

/// What a software decode thread leaves behind when it is stopped: the
/// decoder, and the pictures it had decoded past the last one its span took,
/// in order -- the ones the channel still held, then the ones it had not sent.
type Kept = (Box<SwDecoder>, VecDeque<Yuv>);

/// The way back from a span's [`Streamed`] to the [`Parked`] lot: the lot, the
/// file, and the flag that asks the decode thread to stop at its next picture.
struct Park {
    lot: Arc<Parked>,
    path: PathBuf,
    stop: Arc<AtomicBool>,
}

/// Software H.264 decoders from spans that have ended, kept so that the next
/// span of the same file can carry on from where they stopped instead of
/// seeking back to a sync sample and decoding the whole GOP in front of its in
/// point again (a long-GOP source pays up to a GOP per clip that way).
///
/// Only a decoder that is *resumable* is ever taken: see [`resumable`]. Hardware
/// sessions are never kept, since whether a span gets one is the hardware's
/// answer and not this lot's.
#[derive(Default)]
struct Parked(Mutex<VecDeque<ParkedDecoder>>);

/// Most decoders kept at once. Each holds a demuxer and its reference pictures,
/// so the lot stays small: a dissolve needs two, and a multi-source edit a few.
const PARKED_MAX: usize = 4;

/// One decoder in the lot: the file it reads, the decoder itself, and the
/// pictures it decoded that no span took (see [`Kept`]).
struct ParkedDecoder {
    path: PathBuf,
    decoder: Box<SwDecoder>,
    backlog: VecDeque<Yuv>,
}

impl Parked {
    fn keep(&self, entry: ParkedDecoder) {
        let Ok(mut lot) = self.0.lock() else { return };
        lot.push_back(entry);
        while lot.len() > PARKED_MAX {
            lot.pop_front();
        }
    }

    /// The decoder that can serve a span of `path` starting at `start`, taken
    /// out of the lot and wound forward to that picture: whatever it holds
    /// before `start` is dropped, and the rest is what the span decodes from.
    fn take(&self, path: &Path, start: u32) -> Option<ParkedDecoder> {
        let mut lot = self.0.lock().ok()?;
        let at = lot
            .iter()
            .position(|entry| entry.path == path && entry.serves(start))?;
        let mut entry = lot.remove(at)?;
        let start = i64::from(start);
        while entry.backlog.front().is_some()
            && entry.decoder.index() - (entry.backlog.len() as i64) < start
        {
            entry.backlog.pop_front();
        }
        entry.decoder.start = start as u32;
        Some(entry)
    }
}

impl ParkedDecoder {
    /// Whether this decoder can serve a span starting at `start`: asked of the
    /// file itself, where a fresh decoder would seek to.
    fn serves(&self, start: u32) -> bool {
        let Ok((_, mut demuxer)) = Demuxer::open(&self.path) else {
            return false;
        };
        let restart = demuxer.seek_to_sync_at_or_before(start);
        resumable(
            self.decoder.index(),
            self.backlog.len(),
            self.decoder.origin(),
            restart,
            i64::from(start),
        )
    }
}

/// Whether a decoder can carry on for a span starting at display frame `start`
/// instead of a fresh one opened there. `next` is the display index of the
/// picture it produces next, `backlog` how many decoded pictures sit before
/// that, unclaimed; `origin` is the sync sample it was started at, and
/// `restart` the sync sample a fresh decoder for `start` would start at.
///
/// Two things must hold. The span's first picture must still be held or not
/// yet decoded, since a decoder cannot go back. And the decoder must have been
/// started at the sample a fresh one would start at, so that it has read the
/// same access units from the same point and produces the same pictures -- a
/// later sync sample is a restart the decoder did not make, and one that is not
/// an IDR would be decoded from references the fresh one does not have.
fn resumable(next: i64, backlog: usize, origin: i64, restart: i64, start: i64) -> bool {
    next - (backlog as i64) <= start && restart == origin
}

/// A decode thread, joined when its span's decoder is dropped -- the rule
/// [`crate::decode::Worker`] is written for: a thread abandoned mid
/// `vaInitialize` outlives the process, and Mesa's `atexit` handlers then free
/// the state it is still reading, which is a SIGSEGV at exit long after the
/// export succeeded.
struct DecodeThread<T = ()>(Option<thread::JoinHandle<T>>);

impl<T> DecodeThread<T> {
    /// The thread's result, once it has finished. Joins it if it has not.
    fn finish(&mut self) -> Option<T> {
        self.0.take().and_then(|handle| handle.join().ok())
    }
}

impl<T> Drop for DecodeThread<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // The receiver is already gone (see `Streamed`'s field order), so a
            // decoder blocked on `send` has been woken and this waits only for
            // the thread to unwind its VA-API session.
            let _ = handle.join();
        }
    }
}

impl Drop for Streamed {
    fn drop(&mut self) {
        let Some(park) = self.park.take() else {
            return;
        };
        // Ask the decoder to stop, then take what it sends until it does: its
        // next send is answered by this drain and its next look at the flag
        // ends it, dropping the sender, which is what the drain waits for.
        park.stop.store(true, Ordering::Relaxed);
        let mut backlog = VecDeque::new();
        // A GPU buffer is not kept: a hardware decoder is waiting on the
        // surfaces it has lent, and nothing here would hand them back.
        while let Ok(item) = self.frames.recv() {
            if let Ok(Some(frame @ (Yuv::Pixels { .. } | Yuv::Wide { .. }))) = item {
                backlog.push_back(frame);
            }
        }
        if let Some((decoder, rest)) = self._thread.finish().flatten() {
            backlog.extend(rest);
            park.lot.keep(ParkedDecoder {
                path: park.path,
                decoder,
                backlog,
            });
        }
    }
}

/// One picture on its way from a decode thread to the encoder: its bytes, or
/// the GPU buffer they are still in.
enum Yuv {
    Pixels {
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
        width: u32,
        height: u32,
    },
    /// The same picture at 16 bits a sample, at 10-bit scale: what a 10-bit export
    /// encodes whole ([`Enc::encode_wide`]).
    Wide {
        y: Vec<u16>,
        u: Vec<u16>,
        v: Vec<u16>,
        width: u32,
        height: u32,
    },
    /// Never read back and never copied: a DRM-PRIME handle the encoder imports
    /// on the other side of the channel. Sending it is sending an integer, and
    /// what keeps the buffer from being decoded over is the channel's own depth
    /// (see [`crate::hw::HwSession::next_frame_dma`]).
    Dma(DmaFrame),
}

impl Yuv {
    /// The decoder's own buffer copied out, which is what it costs to hand a
    /// picture to another thread when the pixels have to travel.
    fn pixels(y: &[u8], u: &[u8], v: &[u8], width: u32, height: u32) -> Self {
        Self::Pixels {
            y: y.to_vec(),
            u: u.to_vec(),
            v: v.to_vec(),
            width,
            height,
        }
    }
}

/// What [`ExportHandle::stage`] calls a stabilised clip's shake being measured.
pub const MEASURING_SHAKE: &str = "Measuring shake";

/// How a layer opened for an export measures a stabilised clip with no finished
/// analysis ([`LayerFeed::open`]): under this cancel flag, reporting to this
/// stage ([`Shared::measuring`]).
type Measure<'a> = (&'a AtomicBool, &'a AtomicU32);

/// The luma of file frames `lo..hi` of `path` as a layer's decoder hands them to
/// [`LayerFeed::advance`] -- before the file's display turn and pixel stretch --
/// for the stabilise analysis, whose maps move those same planes. `each` takes
/// the frame number, the luma plane and its size, and stops the walk with `false`.
pub(crate) fn each_layer_luma(
    path: &Path,
    lo: u32,
    hi: u32,
    mut each: impl FnMut(u32, &[u8], u32, u32) -> bool,
) -> crate::Result<()> {
    let mut decoder = ClipDecoder::open(path, lo, None, None, false)?;
    for index in lo..hi {
        match decoder.next()? {
            Some(Frame::Pixels(y, _, _, w, h)) => {
                if !each(index, y, w, h) {
                    break;
                }
            }
            Some(_) => return Err("stabilise: the decoder gave no 8-bit pixels".into()),
            None => break,
        }
    }
    Ok(())
}

/// One picture as the export loop sees it: planes to grade and place, or a
/// buffer that goes straight into the encoder.
enum Frame<'a> {
    Pixels(&'a [u8], &'a [u8], &'a [u8], u32, u32),
    /// A 10-bit picture at 10-bit scale, which only a 10-bit export asks for.
    Wide(&'a [u16], &'a [u16], &'a [u16], u32, u32),
    Dma(&'a DmaFrame),
}

impl ClipDecoder {
    /// A decoder for `path` from `start_frame`. `parked` is the lot a span's
    /// software decoder may be taken from and returned to; `None` never keeps one.
    fn open(
        path: &Path,
        start_frame: u32,
        want: Option<DmaWant>,
        parked: Option<&Arc<Parked>>,
        wide: bool,
    ) -> crate::Result<Self> {
        Self::open_with(path, start_frame, want, parked, wide, Deinterlace::Frame)
    }

    /// [`open`](Self::open) for a clip with a deinterlace setting. `Field` over an
    /// interlaced file makes `start_frame` a field number and every picture a field.
    fn open_with(
        path: &Path,
        start_frame: u32,
        want: Option<DmaWant>,
        parked: Option<&Arc<Parked>>,
        wide: bool,
        deint: Deinterlace,
    ) -> crate::Result<Self> {
        // Before either decoder: an image is not a stream, so `start_frame`
        // means nothing to it -- every frame of a still span is the same
        // picture, which is what playback shows for it too.
        if crate::is_image(path) {
            return Ok(Self::Still(crate::decode::Still::open_at(path, start_frame)?));
        }
        let path = path.to_path_buf();
        let (tx, frames) = sync_channel(2);
        let stop = Arc::new(AtomicBool::new(false));
        let park = parked.map(|lot| Park {
            lot: Arc::clone(lot),
            path: path.clone(),
            stop: Arc::clone(&stop),
        });
        let thread_park = parked.map(|lot| (Arc::clone(lot), Arc::clone(&stop)));
        let handle = thread::Builder::new()
            .name("export-decode".into())
            .spawn(move || decode_stream(&path, start_frame, want, &tx, thread_park, wide, deint))?;
        Ok(Self::Streamed(Streamed {
            frames,
            _thread: DecodeThread(Some(handle)),
            current: None,
            ended: false,
            park,
        }))
    }

    fn visualizer(
        path: &Path,
        stream: usize,
        start_frame: u32,
        (width, height): (u32, u32),
        fps: f64,
        flags: u8,
        paint: u64,
    ) -> crate::Result<Self> {
        let peaks = crate::waveform::peaks(path, stream, crate::decode::VIZ_BUCKETS_PER_SEC)
            .ok()
            .flatten()
            .unwrap_or_default();
        let fps = if fps.is_finite() && fps > 0.0 {
            fps
        } else {
            30.0
        };
        Ok(Self::Visualizer {
            peaks,
            width,
            height,
            fps,
            next: start_frame,
            flags,
            paint,
            scratch: crate::decode::VizScratch::default(),
            y: Vec::new(),
            u: Vec::new(),
            v: Vec::new(),
        })
    }

    /// The pictures of `path` from file frame `top` down to `lo`, last to first:
    /// a clip played backwards. Decoded forwards a block at a time and served from
    /// the back, so the memory held is one block ([`crate::reverse`]), and always
    /// 8-bit software pictures -- the block is copied out of the decoder, which a GPU
    /// buffer or a 10-bit plane does not allow.
    fn reversed(path: &Path, top: u32, lo: u32, deint: Deinterlace) -> Self {
        let mut sync = crate::reverse::sync_frames(path);
        if deint == Deinterlace::Field {
            sync.iter_mut().for_each(|f| *f *= 2);
        }
        Self::Reversed(Box::new(Reversed {
            deint,
            path: path.to_path_buf(),
            walk: crate::reverse::Walk::new(
                sync,
                lo,
                top + 1,
                // Until a picture says how big they are: the biggest the engine takes.
                crate::reverse::block_cap(3840 * 2160 * 3 / 2),
            ),
            block: Vec::new(),
            current: None,
        }))
    }

    /// The next picture, borrowed until the call after -- as tightly packed
    /// I420, or as the GPU buffer the decoder wrote it into.
    fn next(&mut self) -> crate::Result<Option<Frame<'_>>> {
        match self {
            Self::Still(still) => {
                let (y, u, v, width, height) = still.picture();
                Ok(Some(Frame::Pixels(y, u, v, width, height)))
            }
            Self::Reversed(reversed) => reversed.next(),
            Self::Visualizer {
                peaks,
                width,
                height,
                fps,
                next,
                flags,
                paint,
                scratch,
                y,
                u,
                v,
            } => {
                let (yy, uu, vv) = crate::decode::visualizer_i420(
                    scratch,
                    peaks,
                    f64::from(*next) / *fps,
                    *width,
                    *height,
                    *flags,
                    *paint,
                );
                *y = yy;
                *u = uu;
                *v = vv;
                *next += 1;
                Ok(Some(Frame::Pixels(y, u, v, *width & !1, *height & !1)))
            }
            Self::Streamed(stream) if stream.ended => Ok(None),
            Self::Streamed(stream) => match stream.frames.recv() {
                Ok(Ok(Some(frame))) => Ok(Some(match stream.current.insert(frame) {
                    Yuv::Pixels {
                        y,
                        u,
                        v,
                        width,
                        height,
                    } => Frame::Pixels(y, u, v, *width, *height),
                    Yuv::Wide {
                        y,
                        u,
                        v,
                        width,
                        height,
                    } => Frame::Wide(y, u, v, *width, *height),
                    Yuv::Dma(dma) => Frame::Dma(dma),
                })),
                Ok(Ok(None)) => {
                    stream.ended = true;
                    Ok(None)
                }
                Ok(Err(e)) => Err(e),
                Err(_) => Err("the decoder stopped without a picture or a reason".into()),
            },
        }
    }
}

/// One span's decode, on its own thread: open the file (hardware if the plugin
/// will have it, software otherwise) and send every picture until the encoder
/// stops taking them.
///
/// Hardware that fails, before the first picture or after some, hands the rest
/// of the span to the software seat at the first picture not yet sent.
///
/// Every way this returns says which way it was, down the channel: a picture, an
/// error, or the `Ok(None)` that *is* end of stream. Closing the channel is not
/// one of them -- a silent close is a panicked thread, and the receiver treats
/// it as the failure it is rather than as a source that ran out.
///
/// With `parked`, a software decode takes a parked decoder for this file when
/// one can serve `start_frame`, and a decoder that is asked to stop (its span
/// was dropped) is handed back: see [`Streamed`]'s drop.
fn decode_stream(
    path: &Path,
    start_frame: u32,
    want: Option<DmaWant>,
    tx: &SyncSender<crate::Result<Option<Yuv>>>,
    parked: Option<(Arc<Parked>, Arc<AtomicBool>)>,
    wide: bool,
    deint: Deinterlace,
) -> Option<Kept> {
    // A 10-bit export decodes in software: the hardware seat hands 8-bit planes.
    let opened = match forced("VE_SW") || wide {
        // The decode door's own gate ([`crate::decode::open_hw`]): a VP9 4:2:2 or 4:4:4
        // stream is never offered to the GPU, which would refuse it on stderr.
        false => crate::decode::open_hw(path, start_frame).map(|hw| Pictures::Hw(Box::new(hw))),
        true => None,
    };
    // Pictures decoded ahead of the span that a parked decoder still holds.
    let (mut decoder, mut backlog) = match opened {
        Some(hw) => (hw, VecDeque::new()),
        None => match parked
            .as_ref()
            .and_then(|(lot, _)| lot.take(path, start_frame))
        {
            Some(kept) => (Pictures::Sw(kept.decoder), kept.backlog),
            None => match Pictures::software(path, start_frame, wide, deint) {
                Ok(sw) => (sw, VecDeque::new()),
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return None;
                }
            },
        },
    };
    // Pictures handed down the channel. Hardware that dies hands the rest of the
    // span to the software seat at `start_frame + decoded`: the first picture
    // not yet sent, so nothing already encoded is repeated.
    let mut decoded = 0u32;
    loop {
        // Asked to stop: the span is over, and a software decoder that has not
        // reached its end is what the next span may take up.
        if parked
            .as_ref()
            .is_some_and(|(_, stop)| stop.load(Ordering::Relaxed))
        {
            return match decoder {
                Pictures::Sw(sw) if sw.parkable() && !sw.flushed() => Some((sw, backlog)),
                _ => None,
            };
        }
        // Hardware that ends before its first picture has failed the same way
        // one that errors there has: the mid-GOP AV1 seek ends the stream with
        // nothing (see `decode::run_hw`), and it falls back here too.
        let next = match backlog.pop_front() {
            Some(frame) => Ok(Some(frame)),
            None => decoder.next(want),
        };
        let next = match next {
            Ok(None) if decoded == 0 && matches!(decoder, Pictures::Hw(_)) => {
                Err("hardware gave no picture from the start frame".into())
            }
            other => other,
        };
        let frame = match next {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                let _ = tx.send(Ok(None));
                return None;
            }
            Err(e) => {
                if matches!(decoder, Pictures::Hw(_)) {
                    // Before the first picture, or after some: either way the
                    // software seat takes the span from the first picture not yet
                    // sent (`decoded` is 0 in the first case), so nothing is
                    // encoded twice and nothing is skipped.
                    if std::env::var_os("EDITH_DEBUG").is_some() || decoded > 0 { eprintln!(
                        "export decode: hardware failed at frame {} ({e}); falling back to software",
                        start_frame + decoded
                    ); }
                    match Pictures::software(path, start_frame + decoded, wide, deint) {
                        Ok(sw) => {
                            decoder = sw;
                            continue;
                        }
                        Err(sw_refusal) => {
                            let _ = tx.send(Err(sw_refusal));
                            return None;
                        }
                    }
                }
                let _ = tx.send(Err(e));
                return None;
            }
        };
        // The consumer went away: the span ended early, or the export was
        // cancelled. Either way there is nothing left to decode for. A parked
        // span drains the channel before it drops it, so this is not reached.
        if tx.send(Ok(Some(frame))).is_err() {
            return None;
        }
        decoded += 1;
    }
}

/// The stream decoders, on the thread that opened them.
enum Pictures {
    Hw(Box<HwSession>),
    /// `ec-h264`, on this thread.
    Sw(Box<SwDecoder>),
    /// `ec-av1`, on a thread of its own -- see [`Av1Stream`].
    Av1(Box<Av1Stream>),
}

impl Pictures {
    /// The software seat for `path`: `ec-h264` for H.264, `ec-av1` on its own
    /// thread for AV1, and the pulled seats (`ec-h265`, `ec-vp9`, `ec-vp8`) for the
    /// rest. The codec is read once here, so it decides which seat takes the file.
    fn software(path: &Path, start_frame: u32, wide: bool, deint: Deinterlace) -> crate::Result<Self> {
        let (meta, demuxer) = Demuxer::open(path)?;
        match meta.codec {
            Codec::H264 => Ok(Self::Sw(Box::new(SwDecoder::new(demuxer, start_frame, wide, deint)))),
            Codec::Av1 => Ok(Self::Av1(Box::new(Av1Stream::spawn(demuxer, start_frame, wide)?))),
            _ => Ok(Self::Sw(Box::new(SwDecoder::open(path, start_frame, wide, deint)?))),
        }
    }

    /// The next picture, owned, ready to cross the channel. `want` is what the
    /// export's encoder can read straight off the GPU: where the hardware
    /// decoder can hand such a buffer over there is no copy here at all, and
    /// everything else is the 3 MB memcpy out of the decoder's own buffer that
    /// buys the decode and the encode their overlap.
    fn next(&mut self, want: Option<DmaWant>) -> crate::Result<Option<Yuv>> {
        match (self, want) {
            (Self::Hw(hw), Some(want)) => Ok(match hw.next_frame_dma(want)? {
                Some(HwPicture::Dma(dma)) => Some(Yuv::Dma(dma)),
                Some(HwPicture::Pixels(y, u, v, width, height)) => {
                    Some(Yuv::pixels(y, u, v, width, height))
                }
                None => None,
            }),
            (Self::Hw(hw), None) => Ok(hw
                .next_frame()?
                .map(|(y, u, v, width, height)| Yuv::pixels(y, u, v, width, height))),
            (Self::Av1(av1), _) => av1.next(),
            (Self::Sw(sw), _) => {
                if !sw.advance()? {
                    return Ok(None);
                }
                if let Some((y, u, v, width, height)) = sw.picture16() {
                    return Ok(Some(Yuv::Wide {
                        y: y.to_vec(),
                        u: u.to_vec(),
                        v: v.to_vec(),
                        width,
                        height,
                    }));
                }
                let (y, u, v, width, height) = sw.picture();
                Ok(Some(Yuv::pixels(y, u, v, width, height)))
            }
        }
    }
}

/// The software decoder one export span reads from: H.264 with its own reorder
/// and flush, or one of the pulled seats playback's spans read too.
struct SwDecoder {
    demuxer: Demuxer,
    /// The first display index the export takes. Pictures before it are decoded
    /// (the target references them) and then dropped.
    start: u32,
    seat: SwSeat,
}

enum SwSeat {
    H264(Box<H264Seat>),
    /// `wide` is the 10-bit plane buffer when this seat decodes at 10 bits.
    Pulled {
        stream: Box<Pulled>,
        out: I420,
        wide: Option<Wide>,
    },
}

struct H264Seat {
    decoder: H264Decoder,
    /// Display index of the next picture the decoder will produce. Signed: a
    /// sync sample inside what the edit list trims is before frame 0.
    index: i64,
    /// The display index the decoder started at (its sync sample), what
    /// [`resumable`] compares a later span's restart against.
    origin: i64,
    /// Set once the demuxer runs dry: `flush` has then released what the
    /// decoder's reorder still held, and only those pictures are left.
    flushed: bool,
    frame: Option<VideoFrame>,
    /// Whether the export is 10-bit, and the High 10 picture taken for it.
    wide: bool,
    deep: crate::decode::Deep,
    /// Deinterlaces an interlaced stream (a pass-through for every other).
    deint: crate::deinterlace::Deinterlacer,
    /// Pictures the deinterlacer released that `advance` has not handed over yet:
    /// the second field of a frame in field mode.
    pending: VecDeque<(i64, VideoFrame)>,
}

impl SwDecoder {
    fn open(path: &Path, start_frame: u32, wide: bool, deint: Deinterlace) -> crate::Result<Self> {
        let (meta, mut demuxer) = Demuxer::open(path)?;
        // Decoding restarts at a sync sample, so pictures between it and the in
        // point are decoded (the target references them) and then dropped.
        let seat = match Pulled::open(meta.codec, &mut demuxer, start_frame, deint) {
            Some(stream) => SwSeat::Pulled {
                stream: Box::new(stream),
                out: I420::default(),
                // Only HEVC and the intra-only professional codecs have a 10-bit seat; the others narrow as they always did.
                wide: (wide && (matches!(meta.codec, Codec::Hevc | Codec::ProRes | Codec::Dnxhd) || (meta.codec == Codec::Raw && demuxer.bit_depth() >= 10))).then(Wide::default),
            },
            None if meta.codec == Codec::H264 => {
                SwSeat::H264(H264Seat::open(&mut demuxer, start_frame, wide, deint))
            }
            // AV1 never gets here: `Pictures::software` gives it `Av1Stream`.
            None => return Err(format!("{} has no export seat", meta.codec.name()).into()),
        };
        Ok(Self {
            demuxer,
            start: start_frame,
            seat,
        })
    }

    /// Decodes up to and including the next picture at or after the in point.
    /// `false` at end of stream.
    fn advance(&mut self) -> crate::Result<bool> {
        match &mut self.seat {
            SwSeat::H264(seat) => seat.advance(&mut self.demuxer, self.start),
            SwSeat::Pulled { stream, out, wide } => loop {
                let next = match wide {
                    Some(wide) => stream.next_wide(&mut self.demuxer, wide)?,
                    None => stream.next(&mut self.demuxer, out)?,
                };
                let Some(due) = next else {
                    return Ok(false);
                };
                if due >= i64::from(self.start) {
                    return Ok(true);
                }
            },
        }
    }

    /// The 10-bit planes of the picture `advance` stopped on, when this seat
    /// decodes at 10 bits; `None` otherwise.
    fn picture16(&self) -> Option<Wide16View<'_>> {
        match &self.seat {
            SwSeat::Pulled { wide: Some(w), .. } => Some((&w.y, &w.u, &w.v, w.width, w.height)),
            SwSeat::H264(s) if s.deep.in_wide => {
                let w = &s.deep.wide;
                Some((&w.y, &w.u, &w.v, w.width, w.height))
            }
            _ => None,
        }
    }

    /// The picture `advance` stopped on, as planes and size.
    fn picture(&self) -> (&[u8], &[u8], &[u8], u32, u32) {
        match &self.seat {
            SwSeat::H264(seat) if seat.deep.on => {
                let n = &seat.deep.narrow;
                (&n.y, &n.u, &n.v, n.width, n.height)
            }
            SwSeat::H264(seat) => {
                let frame = seat.frame.as_ref().expect("advance stored a picture");
                let [y, u, v] = [&frame.planes[0], &frame.planes[1], &frame.planes[2]];
                debug_assert_eq!(y.stride, frame.width as usize, "padded luma plane");
                (&y.data, &u.data, &v.data, frame.width, frame.height)
            }
            SwSeat::Pulled { out, .. } => (&out.y, &out.u, &out.v, out.width, out.height),
        }
    }
}

/// An AV1 software span: [`decode_av1`] runs on a thread of its own and sends
/// every picture down a channel two deep, the same shape [`Streamed`] gives a
/// stream decoded elsewhere. The thread is needed because `ec-av1` pushes its
/// pictures through a callback rather than handing them out one at a time.
struct Av1Stream {
    frames: Receiver<crate::Result<Option<Yuv>>>,
    /// Held only to be joined: field order matters, as it does for
    /// [`Streamed`] -- `frames` drops first and wakes a decoder parked in a
    /// full `send`, and the join runs after.
    _thread: DecodeThread,
    /// Whether the thread has said "no more pictures" in as many words.
    ended: bool,
}

impl Av1Stream {
    fn spawn(demuxer: Demuxer, start_frame: u32, wide: bool) -> crate::Result<Self> {
        let (tx, frames) = sync_channel(2);
        let handle = thread::Builder::new()
            .name("export-decode-av1".into())
            .spawn(move || decode_av1(demuxer, start_frame, wide, &tx))?;
        Ok(Self {
            frames,
            _thread: DecodeThread(Some(handle)),
            ended: false,
        })
    }

    fn next(&mut self) -> crate::Result<Option<Yuv>> {
        if self.ended {
            return Ok(None);
        }
        match self.frames.recv() {
            Ok(Ok(Some(frame))) => Ok(Some(frame)),
            Ok(Ok(None)) => {
                self.ended = true;
                Ok(None)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the decoder stopped without a picture or a reason".into()),
        }
    }
}

/// The chroma plane size `len` samples can be, for a `width` x `height` luma
/// picture: full (4:4:4), half the width (4:2:2) or a quarter (4:2:0, odd sizes
/// rounding up). `None` for any other count, which no decoder here produces.
pub(crate) fn chroma_shape(len: usize, width: usize, height: usize) -> Option<(usize, usize)> {
    let (half_w, half_h) = (width.div_ceil(2), height.div_ceil(2));
    [(width, height), (half_w, height), (half_w, half_h)]
        .into_iter()
        .find(|&(cw, ch)| cw * ch == len)
}

/// A chroma plane of `cw` x `ch` samples averaged down to `ow` x `oh`, the
/// 4:2:0 size of the luma. A dimension that already matches is kept one sample
/// to one; one that is larger takes the mean of the (at most two) source
/// samples under each output sample, rounded to nearest. Samples keep the
/// depth they came in at, so a 10-bit picture averages in 10 bits and
/// [`crate::decode::narrow_av1`] shifts it as it would any other.
pub(crate) fn chroma_420<T: Copy + Into<u32>>(src: &[T], cw: usize, ch: usize, ow: usize, oh: usize) -> Vec<u32> {
    // The source run under output index `i` along one axis.
    let run = |src_len: usize, out_len: usize, i: usize| match src_len == out_len {
        true => (i, i + 1),
        false => (2 * i, (2 * i + 2).min(src_len)),
    };
    let mut out = Vec::with_capacity(ow * oh);
    for oy in 0..oh {
        let (y0, y1) = run(ch, oh, oy);
        for ox in 0..ow {
            let (x0, x1) = run(cw, ow, ox);
            let (mut sum, mut n) = (0u32, 0u32);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    sum += src[sy * cw + sx].into();
                    n += 1;
                }
            }
            out.push((sum + n / 2) / n);
        }
    }
    out
}

/// The AV1 software seat for the export: the same walk `decode::run_av1` makes
/// for playback, one chunk at a time. `ec-av1` takes a whole OBU stream, so the
/// demuxer's access units are gathered from one sync sample (a sequence header
/// leads it) up to the next, and each chunk decodes to its end -- its hidden
/// frames are references, and the frames they hold back in output order are
/// the tail. Shown pictures before `start_frame` are decoded and dropped.
///
/// Pictures are narrowed to 8-bit I420 by the same [`narrow_av1`] playback
/// uses, so an export and a playback of one file hand the encoder the same
/// planes. A 4:2:2 or 4:4:4 picture has its chroma averaged down to 4:2:0 first
/// ([`chroma_420`]), since the composer and every software encoder take I420 and
/// nothing else. A decode refusal ends the span with the error, after the
/// pictures already sent.
fn decode_av1(
    mut demuxer: Demuxer,
    start_frame: u32,
    wide: bool,
    tx: &SyncSender<crate::Result<Option<Yuv>>>,
) {
    let depth = demuxer.bit_depth();
    // A monochrome stream states it in its sequence header; see the chroma below.
    let gray = demuxer.chroma() == Some(Chroma::Gray);
    // Decoding restarts at a sync sample; see [`SwDecoder::new`].
    let mut index = demuxer.seek_to_sync_at_or_before(start_frame);
    let mut pending: Option<Vec<u8>> = None;
    let mut demux_dry = false;
    loop {
        // One chunk: the access units from a sync sample up to the next one.
        let mut stream = pending.take().unwrap_or_default();
        while !demux_dry && pending.is_none() {
            match demuxer.next_access_unit() {
                Ok(Some(au)) => {
                    if !stream.is_empty() && crate::decode::leads_with_sequence_header(&au) {
                        pending = Some(au);
                    } else {
                        stream.extend_from_slice(&au);
                    }
                }
                Ok(None) => demux_dry = true,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
        if stream.is_empty() {
            let _ = tx.send(Ok(None));
            return;
        }
        // Set by the sink when the consumer is gone, or when a picture is
        // refused; `Eof` is the sentinel that stops the chunk's decode there.
        let mut gone = false;
        let mut refused = None;
        let decode = decode_stream_with(&stream, |picture, _, shown| {
            if !shown {
                return Ok(());
            }
            let due = index;
            index += 1;
            if due < i64::from(start_frame) {
                return Ok(());
            }
            let (w, h) = (picture.width, picture.height);
            let (ow, oh) = (w.div_ceil(2), h.div_ceil(2));
            // 4:2:2 and 4:4:4 pictures are averaged down to the 4:2:0 the
            // composer and every software encoder take ([`chroma_420`]). A
            // monochrome stream carries no chroma, and the decoder hands back
            // zeros in its place: the neutral grey is written instead, at the
            // depth the samples are held in (512 for 10-bit).
            let Some((cw, ch)) = chroma_shape(picture.u.len(), w, h) else {
                refused = Some(format!(
                    "software AV1 decode (ec-av1) gave a {w}x{h} picture with {} chroma samples per plane, which is no 4:2:0, 4:2:2 or 4:4:4 layout",
                    picture.u.len()
                ));
                return Err(Av1Error::Eof);
            };
            let half = if gray {
                let mid = 1u16 << (depth - 1);
                Some(Av1Picture {
                    width: w,
                    height: h,
                    y: picture.y.clone(),
                    u: vec![mid; ow * oh],
                    v: vec![mid; ow * oh],
                })
            } else if (cw, ch) != (ow, oh) {
                let down = |plane: &[u16]| -> Vec<u16> {
                    chroma_420(plane, cw, ch, ow, oh)
                        .into_iter()
                        .map(|s| s as u16)
                        .collect()
                };
                Some(Av1Picture {
                    width: w,
                    height: h,
                    y: picture.y.clone(),
                    u: down(&picture.u),
                    v: down(&picture.v),
                })
            } else {
                None
            };
            let src = half.as_ref().unwrap_or(picture);
            let frame = if wide {
                // At 10-bit scale: a 10-bit stream's samples are there already, an
                // 8-bit one (in a 10-bit export) is shifted up by two.
                // A 12-bit one is rounded down by two.
                let plane = |s: &[u16]| -> Vec<u16> {
                    s.iter()
                        .map(|&x| match depth {
                            ..=9 => x << (10 - depth),
                            10 => x,
                            _ => ((x + (1 << (depth - 11))) >> (depth - 10)).min(1023),
                        })
                        .collect()
                };
                Yuv::Wide {
                    y: plane(&src.y),
                    u: plane(&src.u),
                    v: plane(&src.v),
                    width: w as u32,
                    height: h as u32,
                }
            } else {
                let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
                crate::decode::narrow_av1(src, depth, &mut y, &mut u, &mut v);
                Yuv::Pixels {
                    y,
                    u,
                    v,
                    width: w as u32,
                    height: h as u32,
                }
            };
            if tx.send(Ok(Some(frame))).is_err() {
                gone = true;
                return Err(Av1Error::Eof);
            }
            Ok(())
        });
        if gone {
            return;
        }
        if let Some(msg) = refused {
            let _ = tx.send(Err(msg.into()));
            return;
        }
        if let Err(e) = decode {
            let msg = format!("software AV1 decode failed at picture {index}: {e}");
            let _ = tx.send(Err(msg.into()));
            return;
        }
    }
}

impl SwDecoder {
    /// The H.264 seat over an opened demuxer. Decoding restarts at a sync
    /// sample, so pictures between it and the in point are decoded (the target
    /// references them) and then dropped.
    fn new(mut demuxer: Demuxer, start_frame: u32, wide: bool, deint: Deinterlace) -> Self {
        let seat = SwSeat::H264(H264Seat::open(&mut demuxer, start_frame, wide, deint));
        Self {
            demuxer,
            start: start_frame,
            seat,
        }
    }

    /// Whether this decoder can be parked for a later span: only the H.264 seat
    /// has a display index to resume from, so the pulled seats never are.
    fn parkable(&self) -> bool {
        // An interlaced seat holds a picture back for its successor, so it has
        // no display index a later span could resume from.
        matches!(&self.seat, SwSeat::H264(s) if !s.deint.is_interlaced())
    }

    /// The display index the seat's next picture will carry; `0` off the H.264 seat.
    fn index(&self) -> i64 {
        match &self.seat {
            SwSeat::H264(seat) => seat.index,
            SwSeat::Pulled { .. } => 0,
        }
    }

    /// The display index the seat started at; `0` off the H.264 seat.
    fn origin(&self) -> i64 {
        match &self.seat {
            SwSeat::H264(seat) => seat.origin,
            SwSeat::Pulled { .. } => 0,
        }
    }

    /// Set once the demuxer ran dry. Always `true` off the H.264 seat, which is
    /// what keeps a pulled decoder from being parked (see [`SwDecoder::parkable`]).
    fn flushed(&self) -> bool {
        match &self.seat {
            SwSeat::H264(seat) => seat.flushed || seat.deint.is_interlaced(),
            SwSeat::Pulled { .. } => true,
        }
    }
}

impl H264Seat {
    /// A fresh `ec-h264` seat whose first picture is display index `index`.
    ///
    /// An interlaced stream lands one picture earlier than the in point, so the
    /// deinterlacer has the picture before it, as a sequential decode does.
    fn open(demuxer: &mut Demuxer, start: u32, wide: bool, mode: Deinterlace) -> Box<Self> {
        let deint = crate::deinterlace::Deinterlacer::with_mode(
            mode,
            demuxer.h264_interlaced(),
            i64::from(start),
            ec_core::frame::Interlace::Progressive,
        )
        .cadenced(mode, demuxer);
        let index = demuxer.seek_to_sync_at_or_before(deint.landing(start));
        Box::new(Self {
            deint,
            ..*Self::at(index, wide)
        })
    }

    fn at(index: i64, wide: bool) -> Box<Self> {
        Box::new(Self {
            decoder: H264Decoder::new(CodecParameters::new(CodecId::H264))
                .expect("ec-h264 takes its own codec id"),
            index,
            origin: index,
            flushed: false,
            frame: None,
            wide,
            deep: Default::default(),
            deint: crate::deinterlace::Deinterlacer::new(false, 0, ec_core::frame::Interlace::Progressive),
            pending: VecDeque::new(),
        })
    }

    /// Decodes up to and including the next picture at or after the in point,
    /// leaving it in `frame`. `false` at end of stream. Pictures leave the
    /// decoder in display order -- clause C.4.5.3, the same contract the
    /// playback seat keeps -- so the landing index, itself a display index by
    /// the demuxer's contract, counts one to one with them.
    fn advance(&mut self, demuxer: &mut Demuxer, start: u32) -> crate::Result<bool> {
        loop {
            // The second field of the frame the last call took the first of.
            if let Some((due, pic)) = self.pending.pop_front() {
                if due < i64::from(start) {
                    continue;
                }
                self.deep.load(&pic, self.wide);
                self.frame = Some(pic);
                return Ok(true);
            }
            if !self.flushed {
                match demuxer.next_access_unit()? {
                    Some(au) => self
                        .decoder
                        .send_packet(&Packet::new(0, TimeBase::new(1, 1), au))
                        .map_err(|e| format!("decode at picture {}: {e}", self.index))?,
                    None => {
                        self.flushed = true;
                        self.decoder
                            .flush()
                            .map_err(|e| format!("decode at end of stream: {e}"))?;
                    }
                }
            }
            match self.decoder.receive_frame() {
                Ok(frame) => {
                    let ec_core::frame::Frame::Video(pic) = frame else {
                        continue;
                    };
                    let tag = self.index;
                    self.index += 1;
                    // Held one picture back while the deinterlacer waits for
                    // the successor; below the in point it only primes. In
                    // field mode it releases two, queued for the loop's top.
                    self.pending.extend(self.deint.push_all(tag, pic));
                }
                // End of stream counts once the flush has happened; before it,
                // either answer means go back for the next access unit. The
                // last picture an interlaced stream held leaves first.
                Err(e) if e.is_eof() && self.flushed => {
                    self.pending.extend(self.deint.finish_all());
                    if self.pending.is_empty() {
                        return Ok(false);
                    }
                }
                Err(e) if e.is_need_more() || e.is_eof() => {}
                Err(e) => return Err(format!("decode at picture {}: {e}", self.index).into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// A standalone file's soft track is titled with its language's name, or left
    /// untitled, and never with its file name; an embedded track keeps the title
    /// its container gave it.
    #[test]
    fn a_soft_track_is_titled_by_its_language_never_its_file_name() {
        let file = |language: &str| SubtitleTrack {
            path: "/media/talk.fr.srt".into(),
            track: None,
            language: language.into(),
            name: String::new(),
            label: "talk.fr.srt".into(),
            cues: Vec::new(),
            bitmap: false,
            refused: None,
        };
        assert_eq!(subtitle_title(&file("fra")), "French");
        assert_eq!(subtitle_title(&file("")), "", "unset: no title, and never the file name");
        let embedded = SubtitleTrack {
            track: Some(3),
            name: "Signs".into(),
            ..file("fra")
        };
        assert_eq!(subtitle_title(&embedded), "Signs");
    }

    /// Two caption lanes, one per palette track: a pick list names the lanes it
    /// keeps, and an empty one keeps them all -- in the plan and in the file.
    #[test]
    fn a_pick_list_keeps_only_the_lanes_it_names() {
        use crate::project::{LaneKind, SubClip};
        let track = |label: &str, text: &str| SubtitleTrack {
            path: "/nonexistent/film.mkv".into(),
            track: Some(1),
            language: "eng".into(),
            name: label.into(),
            label: label.into(),
            bitmap: false,
            cues: vec![Cue {
                start_us: 0,
                end_us: 1_000_000,
                text: text.into(),
                image: None,
                words: Vec::new(),
                karaoke: Vec::new(),
            }],
            refused: None,
        };
        let mut project = Project::single("/nonexistent/a.mp4", 150)
            .with_subtitles(vec![track("one", "first words"), track("two", "second words")]);
        for track in 0..2 {
            let lane = project.add_lane(LaneKind::Subtitle);
            project
                .place_sub(
                    lane,
                    0,
                    SubClip {
                        start: 0,
                        frames: 150,
                        track,
                        in_us: 0,
                        out_us: 1_000_000,
                        link: None,
                    },
                )
                .expect("an empty lane takes the placement");
        }
        let names = |subs: &[SubParams]| subs.iter().map(|s| s.name.clone()).collect::<Vec<_>>();

        let only_zero = lane_subtitles(&project, 30.0, None, &[0]);
        assert_eq!(names(&only_zero), ["one"], "pick [0] keeps track 0's lane alone");
        assert_eq!(only_zero[0].cues[0].text, "first words");

        let all = lane_subtitles(&project, 30.0, None, &[]);
        assert_eq!(names(&all), ["one", "two"], "an empty pick list keeps every lane");

        let plan = planned_subtitles(&project, Format::Mp4, [0]);
        assert!(plan.contains("S1 one") && !plan.contains("two"), "plan: {plan}");
        let plan = planned_subtitles(&project, Format::Mp4, Vec::<usize>::new());
        assert!(plan.contains("S1 one") && plan.contains("S2 two"), "plan: {plan}");
    }

    /// Four palette tracks, lanes S1 and S2 holding tracks 0 and 2: the file writes the tracks in
    /// the order the caller gave them, placed or not, and the card names them in that order.
    #[test]
    fn soft_tracks_are_written_in_the_order_the_caller_gave() {
        use crate::project::{LaneKind, SubClip};
        let track = |label: &str| SubtitleTrack {
            path: "/nonexistent/film.srt".into(),
            track: None,
            language: "eng".into(),
            name: label.into(),
            label: label.into(),
            bitmap: false,
            cues: vec![Cue {
                start_us: 0,
                end_us: 1_000_000,
                text: format!("{label} words"),
                image: None,
                words: Vec::new(),
                karaoke: Vec::new(),
            }],
            refused: None,
        };
        let mut project = Project::single("/nonexistent/a.mp4", 150)
            .with_subtitles(vec![track("one"), track("two"), track("three"), track("four")]);
        for track in [0, 2] {
            let lane = project.add_lane(LaneKind::Subtitle);
            project
                .place_sub(
                    lane,
                    0,
                    SubClip {
                        start: 0,
                        frames: 150,
                        track,
                        in_us: 0,
                        out_us: 1_000_000,
                        link: None,
                    },
                )
                .expect("an empty lane takes the placement");
        }
        let lanes = project.subtitle_lanes();
        let (s1, s2) = (lanes[0], lanes[1]);

        assert_eq!(soft_order(&project, &lanes, &[2, 0]), [SoftEntry::Lane(s2), SoftEntry::Lane(s1)]);
        assert_eq!(soft_order(&project, &lanes, &[0, 2]), [SoftEntry::Lane(s1), SoftEntry::Lane(s2)]);
        assert_eq!(soft_order(&project, &lanes, &[3, 1]), [SoftEntry::Track(3), SoftEntry::Track(1)]);
        assert_eq!(soft_order(&project, &lanes, &[]), [SoftEntry::Lane(s1), SoftEntry::Lane(s2)]);

        let plan = planned_subtitles(&project, Format::Mp4, [2, 0]);
        assert!(plan.contains("2 lanes → embedded (S2 three, S1 one)"), "plan: {plan}");
        let plan = planned_subtitles(&project, Format::Mp4, [3, 1]);
        assert!(plan.contains("2 tracks → embedded (four, two)"), "plan: {plan}");
        let plan = planned_subtitles(&project, Format::Mp4, [3, 2, 1, 0]);
        assert!(plan.contains("4 tracks → embedded (four, S2 three, two, S1 one)"), "plan: {plan}");
    }

    /// A ranged file names a track with no cue inside the range as dropped, and carries the rest;
    /// a standalone ASS script with override blocks is named as styling the file drops.
    #[test]
    fn a_range_drops_tracks_with_no_cue_in_it_and_ass_styling_is_named() {
        use crate::project::{LaneKind, SubClip};
        let track = |label: &str, path: &str| SubtitleTrack {
            path: path.into(),
            track: None,
            language: "eng".into(),
            name: label.into(),
            label: label.into(),
            bitmap: false,
            cues: vec![Cue {
                start_us: 0,
                end_us: 1_000_000,
                text: format!("{label} words"),
                image: None,
                words: Vec::new(),
                karaoke: Vec::new(),
            }],
            refused: None,
        };
        let ass = std::env::temp_dir().join(format!("edith_export_styling_{}.ass", std::process::id()));
        std::fs::write(
            &ass,
            "[Script Info]\nScriptType: v4.00+\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,{\\i1}Hi{\\i0}\n",
        )
        .expect("a scratch script");
        let mut project = Project::single("/nonexistent/a.mp4", 150)
            .with_subtitles(vec![track("one", ass.to_str().unwrap()), track("two", "/nonexistent/film.srt")]);
        for t in 0..2 {
            let lane = project.add_lane(LaneKind::Subtitle);
            project
                .place_sub(
                    lane,
                    0,
                    SubClip {
                        start: 0,
                        frames: 30,
                        track: t,
                        in_us: 0,
                        out_us: 1_000_000,
                        link: None,
                    },
                )
                .expect("an empty lane takes the placement");
        }
        // Cues sit in the first second (frames 0 to 30 at 30 fps): a range after it has none.
        let late = planned_subtitles_in(&project, Format::Mp4, [0, 1], Some((60, 150)), 30.0);
        assert!(late.contains("S1 one — no cues in range") && late.contains("S2 two — no cues in range"), "plan: {late}");
        assert!(!late.contains("→ embedded"), "plan: {late}");
        let early = planned_subtitles_in(&project, Format::Mp4, [0, 1], Some((0, 30)), 30.0);
        assert!(early.contains("2 lanes → embedded (S1 one, S2 two)"), "plan: {early}");

        assert_eq!(stripped_styling(&project, &[0, 1]), ["one"], "the ASS script's override tags");
        assert!(stripped_styling(&project, &[1]).is_empty(), "a plain SRT has no styling to drop");
        let _ = std::fs::remove_file(&ass);
    }

    #[test]
    fn fit_picture_clips_sets_every_picture_clip_on_the_copy() {
        let mut p = Project::single("/nonexistent/a.mp4", 90);
        let before = p.lane(Lane::V1)[0].fit;
        assert_eq!(before, FitPolicy::Fit, "a clip starts on its own default");
        assert_eq!(
            fit_picture_clips(&mut p, FitPolicy::Fill),
            FitOutcome { lanes: vec![(Lane::V1, 1)], overlays: 0 },
            "one picture clip"
        );
        assert_eq!(p.lane(Lane::V1)[0].fit, FitPolicy::Fill);
        assert_eq!(p.fit_of(Lane::V1, 0), FitPolicy::Fill);
    }

    /// The I+P seat's automatic quality is its own default, not the intra
    /// seat's mapping: at 1080p30 the intra mapping clamps to QP 40 and the
    /// GOP seat must name QP 28 instead.
    #[test]
    fn the_gop_default_is_not_the_intra_seats_clamped_qp() {
        let m = meta(1920, 1080);
        let auto = ExportSettings { format: Format::HevcMp4, ..Default::default() };
        assert_eq!(hevc_qp(&m, &auto), 40, "the intra mapping clamps at 1080p30");
        assert_eq!(
            hevc_mode_words(&m, &auto),
            Some("I+P GOP (1 s keyframes), constant quality QP 28; pass bitrate_kbps for a fixed rate".to_string())
        );
    }

    #[test]
    fn hevc_and_av1_words_name_the_quality_or_the_rate() {
        let m = meta(1920, 1080);
        let auto = |format| ExportSettings { format, ..Default::default() };
        let fixed = |format| ExportSettings { format, bitrate: Some(4_000_000), ..Default::default() };
        // With no bitrate the HEVC row is constant QP: the number is the one the encoder is given.
        assert_eq!(
            hevc_mode_words(&m, &auto(Format::HevcMp4)),
            Some(format!("I+P GOP (1 s keyframes), constant quality QP {HEVC_GOP_QP}; pass bitrate_kbps for a fixed rate"))
        );
        assert_eq!(
            hevc_mode_words(&m, &fixed(Format::HevcMp4)),
            Some("I+P GOP (1 s keyframes), 4 Mb/s".to_string())
        );
        assert_eq!(hevc_mode_words(&m, &auto(Format::Mp4)), None, "not an HEVC format");
        // AV1 is coded at a bitrate: the automatic one is named as the target it is.
        let target = av1_mode_words(&m, &auto(Format::Av1)).expect("an AV1 format");
        assert!(target.starts_with("average bitrate ") && target.contains("pass bitrate_kbps"), "{target}");
        assert_eq!(av1_mode_words(&m, &fixed(Format::Av1Mp4)), Some("average bitrate 4 Mb/s".to_string()));
        assert_eq!(rate_words(6220), "6.2 Mb/s");
        assert_eq!(rate_words(850), "850 kb/s");
        assert_eq!(av1_mode_words(&m, &auto(Format::HevcMp4)), None, "not an AV1 format");
    }

    #[test]
    fn fit_picture_clips_leaves_a_transformed_overlay_on_its_own_fit() {
        use crate::transform::TransformParams;
        let mut p = Project::single("/nonexistent/a.mp4", 90);
        let v2 = Lane::new(LaneKind::Video, 1);
        assert_eq!(p.add_lane(LaneKind::Video), v2);
        let logo = p.lane(Lane::V1)[0].clone();
        assert!(p.place(v2, 0, logo), "a logo on V2");
        // The logo as the project has it: 0.3 of the canvas width, 80% opaque, moved up and right.
        let placed = TransformParams { pos_x: 0.32, pos_y: -0.32, scale: 0.3, opacity: 0.8, ..Default::default() };
        assert!(p.set_transform(v2, 0, Some(placed)));
        assert_eq!(
            fit_picture_clips(&mut p, FitPolicy::Fill),
            FitOutcome { lanes: vec![(Lane::V1, 1)], overlays: 1 },
            "the base picture refits, the overlay does not"
        );
        assert_eq!(p.fit_of(Lane::V1, 0), FitPolicy::Fill);
        assert_eq!(p.fit_of(v2, 0), FitPolicy::Fit, "the overlay keeps its own fit");
        assert_eq!(p.transform_of(v2, 0), Some(&placed), "and its placement");

        // An identity transform is not an overlay: it refits like any full-frame clip.
        assert!(p.set_transform(v2, 0, Some(TransformParams::default())));
        assert_eq!(fit_picture_clips(&mut p, FitPolicy::Fill).overlays, 0);
        assert_eq!(p.fit_of(v2, 0), FitPolicy::Fill);
    }

    #[test]
    fn a_caption_and_a_title_clash_only_in_the_same_place_and_on_screen_together() {
        use crate::project::Title;
        use crate::text::TextStyle;
        let cue = |start_s: f64, end_s: f64, text: &str| Cue {
            start_us: (start_s * 1e6) as i64,
            end_us: (end_s * 1e6) as i64,
            text: text.into(),
            image: None,
            words: Vec::new(),
            karaoke: Vec::new(),
        };
        let title = |start: u32, frames: u32, text: &str, place: Place| Title {
            start,
            frames,
            text: text.into(),
            style: TextStyle { place, ..TextStyle::default() },
            fade_in: 0,
            fade_out: 0,
            in_anim: Default::default(),
            out_anim: Default::default(),
            anim_frames: 0,
        };
        // Captions 86.0 to 88.0 s; a bottom title from frame 2163 (86.52 s at 25 fps) for 3 s.
        let cues = [cue(86.0, 88.0, "hello")];
        let titles = [title(2163, 75, "EVENING, MIRADOURO", Place::Bottom)];
        let clash = caption_title_clash(&cues, &titles, Place::Bottom, 25.0).expect("same place, on screen together");
        assert!((clash.0 - 86.52).abs() < 0.01, "the clash starts where both are up: {clash:?}");
        assert_eq!(clash.1, 0);
        assert_eq!(caption_title_clash(&cues, &titles, Place::Top, 25.0), None, "a top caption clears a bottom title");
        // The title is up after the captions end: no clash.
        let late = [title(2300, 25, "LATE", Place::Bottom)];
        assert_eq!(caption_title_clash(&cues, &late, Place::Bottom, 25.0), None, "not on screen together");
        // A blank cue is no words on the picture.
        let blank = [cue(86.0, 88.0, "  ")];
        assert_eq!(caption_title_clash(&blank, &titles, Place::Bottom, 25.0), None);
    }

    #[test]
    fn h264_levels_follow_table_a1() {
        use super::h264_level_idc;
        assert_eq!(h264_level_idc(1280, 720, 30, 1), 31);
        assert_eq!(h264_level_idc(1920, 1080, 30, 1), 40);
        assert_eq!(h264_level_idc(1920, 1080, 60, 1), 42);
        assert_eq!(h264_level_idc(2560, 1440, 30, 1), 50);
        assert_eq!(h264_level_idc(3840, 1608, 24000, 1001), 51);
        assert_eq!(h264_level_idc(3840, 2160, 60, 1), 52);
        assert_eq!(h264_level_idc(640, 360, 30, 1), 30);
    }

    use super::*;

    /// The bar of a *feature film*: a picture count times a permille is a
    /// product that has to be computed in `u64`, or a timeline past ~4.5 M
    /// pictures (42 hours at 30 fps) overflowed the `u32` this used to be --
    /// a panic in debug inside the export thread, and a bar that thrashed in
    /// release. Every value here is a fraction of the whole, so none of them
    /// may leave the scale, and the counts straddle the old overflow point.
    #[test]
    fn a_films_frame_count_does_not_overflow_the_bar() {
        // The straddle, in the very product the old shape computed: `boundary`
        // is the largest picture count whose `done * band` still fitted a
        // `u32`, and one more is where it began to wrap -- a panic in debug,
        // inside the export thread.
        let band = u64::from(PROGRESS_SCALE - AUDIO_BAND);
        let boundary = u64::from(u32::MAX) / band;
        assert!(boundary * band <= u64::from(u32::MAX));
        assert!((boundary + 1) * band > u64::from(u32::MAX));
        for total in [boundary as u32 + 1, 21_600_000, u32::MAX] {
            assert_eq!(picture_progress(total, total), PROGRESS_SCALE, "{total}");
            assert_eq!(picture_progress(0, total), AUDIO_BAND, "{total}");
            let half = picture_progress(total / 2, total);
            assert!(
                (AUDIO_BAND..PROGRESS_SCALE).contains(&half),
                "half of {total} is {half}"
            );
            assert!(
                picture_progress(total - 1, total) <= PROGRESS_SCALE,
                "{total}"
            );
        }
    }

    /// How a decode thread ended is never guessed from the channel being shut.
    /// A source that genuinely ran out says so, and the span ends on it; a
    /// thread that died without a word is a failure, because reading it as end
    /// of stream would truncate the picture and *report the export finished*.
    #[test]
    fn a_decoder_that_dies_without_a_word_is_not_end_of_stream() {
        let streamed = |handle, frames| {
            ClipDecoder::Streamed(Streamed {
                frames,
                _thread: DecodeThread(Some(handle)),
                current: None,
                ended: false,
                park: None,
            })
        };

        let (tx, frames) = sync_channel::<crate::Result<Option<Yuv>>>(2);
        // Said its piece, then finished: the span walks on to its next clip.
        let handle = thread::spawn(move || {
            tx.send(Ok(None)).expect("the receiver is alive");
            None
        });
        let mut ended = streamed(handle, frames);
        assert!(ended.next().expect("a clean end is not an error").is_none());
        // ...and asking again is the same answer and not a disconnect error.
        assert!(ended.next().expect("still not an error").is_none());

        let (tx, frames) = sync_channel::<crate::Result<Option<Yuv>>>(2);
        // Died holding the sender: the channel shuts with nothing said.
        let handle = thread::spawn(move || {
            let _tx = tx;
            panic!("decoder died");
        });
        let mut died = streamed(handle, frames);
        let Err(e) = died.next() else {
            panic!("a silent death must fail the export, not end the clip")
        };
        assert!(
            e.to_string().contains("without a picture or a reason"),
            "{e}"
        );
    }

    /// When a parked decoder may carry on for a later span. A decoder cannot go
    /// back, so the span must start at or after the first picture it still
    /// holds; and it must have been started at the sync sample a fresh decoder
    /// would seek to, or the pictures after it would not be the fresh ones.
    #[test]
    fn a_parked_decoder_resumes_only_where_a_fresh_one_would_match() {
        // Stopped at display 330 with two decoded pictures unclaimed (328 and
        // 329), started at the sync sample 250.
        let stopped = |start| resumable(330, 2, 250, 250, start);
        assert!(stopped(328), "the first held picture is the span's first");
        assert!(stopped(329), "the second held picture is the span's first");
        assert!(stopped(330), "the span starts where the decoder stopped");
        assert!(stopped(400), "a span further on decodes on to its picture");
        assert!(!stopped(327), "a span before what is held needs a restart");
        // Nothing unclaimed: the decoder's own position is the first picture.
        assert!(resumable(330, 0, 250, 250, 330));
        assert!(!resumable(330, 0, 250, 250, 329));
        // A fresh decoder for the span would seek to a later sync sample than
        // the one this decoder started at, so this one is not the same decode.
        assert!(!resumable(330, 2, 250, 300, 400));
        // And one that started at a later sync sample than a fresh one would
        // is not the same decode either.
        assert!(!resumable(330, 2, 300, 250, 330));
    }

    /// Three seconds of two tones, one per channel, interleaved: enough shape
    /// that a correlation says something and enough length that the encoder
    /// settles into a mode.
    fn two_tones(secs: usize) -> Vec<f32> {
        (0..OPUS_RATE as usize * secs)
            .flat_map(|i| {
                let t = i as f32 / OPUS_RATE as f32;
                let tau = std::f32::consts::TAU;
                [
                    0.3 * (tau * 440.0 * t).sin() + 0.2 * (tau * 1234.0 * t).sin(),
                    0.25 * (tau * 880.0 * t).sin(),
                ]
            })
            .collect()
    }

    /// Three seconds of the same two tones summed onto one channel: the mono
    /// mix `opus-rs 0.1.26` mis-encoded at every rate, which is why a mono
    /// export kept the AAC path until `ec-opus` took the seat.
    fn mono_tones(secs: usize) -> Vec<f32> {
        (0..OPUS_RATE as usize * secs)
            .map(|i| {
                let t = i as f32 / OPUS_RATE as f32;
                let tau = std::f32::consts::TAU;
                0.3 * (tau * 440.0 * t).sin() + 0.2 * (tau * 1234.0 * t).sin()
            })
            .collect()
    }

    /// The best correlation of one channel of `got` against `want`, and the
    /// offset it sat at. `stride` is the interleave width: 2 for the stereo
    /// fixtures, 1 for mono.
    fn align(want: &[f32], got: &[f32], stride: usize) -> (f64, usize) {
        let (mut best, mut at) = (-2.0, 0);
        let window = OPUS_RATE as usize;
        for lag in 0..1400 {
            let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
            for (x, y) in want[..window]
                .iter()
                .step_by(stride)
                .zip(got[lag * stride..].iter().step_by(stride))
            {
                num += f64::from(*x) * f64::from(*y);
                da += f64::from(*x) * f64::from(*x);
                db += f64::from(*y) * f64::from(*y);
            }
            let c = num / (da.sqrt() * db.sqrt()).max(1e-12);
            if c > best {
                (best, at) = (c, lag);
            }
        }
        (best, at)
    }

    fn ec_decode(packets: &[crate::AacPacket]) -> Vec<f32> {
        let mut decoder = ec_opus::MultistreamDecoder::with_rate(OPUS_RATE, 1, 1, &[0, 1]);
        let mut pcm = Vec::new();
        for packet in packets {
            pcm.extend(
                decoder
                    .decode_packet(&packet.bytes)
                    .expect("a valid packet"),
            );
        }
        pcm
    }

    /// [`ec_decode`] for a mono packet stream: one stream, no coupling.
    fn ec_decode_mono(packets: &[crate::AacPacket]) -> Vec<f32> {
        let mut decoder = ec_opus::MultistreamDecoder::with_rate(OPUS_RATE, 1, 0, &[0]);
        let mut pcm = Vec::new();
        for packet in packets {
            pcm.extend(
                decoder
                    .decode_packet(&packet.bytes)
                    .expect("a valid packet"),
            );
        }
        pcm
    }

    /// Why [`OPUS_MAX_KBPS`] is a cap, why [`OPUS_PRE_SKIP`] is a constant and
    /// why [`opus_fidelity`] exists at all -- all three measured here through
    /// the same decoder door the product reads every Opus file back with, the
    /// decoder being the witness and the *encoder* the one under test.
    ///
    /// The second half pins the two failures the seat's move here fixed: under
    /// `opus-rs 0.1.26` the encoder wrote stereo packets that only its own
    /// decoder read back above roughly 165 kbps (fidelity 0.06 at 256), and it
    /// mis-encoded mono at every rate -- which is what the old 128 cap and the
    /// mono-mix-keeps-AAC rule stood on. Both asserts are survival now, and a
    /// regression fails here, loudly: that is the signal to lower
    /// [`OPUS_MAX_KBPS`] again, not a reason to widen it quietly.
    #[test]
    fn the_opus_encoder_is_pinned_to_the_band_it_was_measured_in() {
        let pcm = two_tones(3);
        for kbps in [96, OPUS_MAX_KBPS] {
            let (packets, pre_skip) = encode_opus(&pcm, kbps, 2)
                .expect("the encoder opens")
                .expect("and inside the band it passes its own round trip");
            assert_eq!(pre_skip, OPUS_PRE_SKIP + OPUS_FRAME as u16);
            assert_eq!(
                packets.len(),
                pcm.len().div_ceil(OPUS_FRAME * 2) + 2,
                "one packet per 20 ms frame, plus the warm-up and the flush"
            );
            let decoded = ec_decode(&packets);
            let (c, lag) = align(&pcm, &decoded, 2);
            // From the first audible sample, head included -- and the head is
            // why this is 0.98 and not the 0.999 below: the encoder's first
            // audible 20 ms comes out a few dB down (one frame of cold start;
            // a second warm-up frame changes the numbers in no decimal place),
            // so that ramp is stated rather than hidden behind a loose
            // threshold.
            assert!(c >= 0.98, "{kbps} kbps came back at correlation {c:.4}");
            let (settled, _) = align(&pcm[OPUS_FRAME * 2..], &decoded[OPUS_FRAME * 2..], 2);
            assert!(
                settled >= 0.999,
                "{kbps} kbps past the first frame: correlation {settled:.4}"
            );
            // The whole point of the number in the header: drop exactly that
            // many samples and the sound starts where the timeline says. A lag
            // that is not it is a file that plays late or eats its own head.
            assert_eq!(
                lag,
                usize::from(pre_skip),
                "{kbps} kbps: the declared pre-skip is not where the sound starts"
            );
            // ...and the tail is all there: the flush frame pushed the last
            // samples out, so what is left after the pre-skip covers the input.
            assert!(
                decoded.len() - usize::from(pre_skip) * 2 >= pcm.len(),
                "{kbps} kbps: the track ends {} samples short of the timeline",
                (pcm.len() + usize::from(pre_skip) * 2 - decoded.len()) / 2
            );
            // The gate the export runs before it writes anything, on a track
            // that deserves to pass it.
            let fidelity = opus_fidelity(&packets, &pcm, usize::from(pre_skip), 2);
            assert!(
                fidelity >= OPUS_MIN_FIDELITY,
                "{kbps} kbps scored {fidelity:.4} on its own round trip"
            );
        }

        // Mono, the shape `opus-rs 0.1.26` mis-encoded at every rate and the
        // reason a mono mix kept the AAC path: straight through the same seat,
        // including at the rate where the old encoder was already writing
        // stereo noise, let alone mono.
        let mono = mono_tones(3);
        for kbps in [96, 256] {
            let (packets, pre_skip) = encode_opus(&mono, kbps, 1)
                .expect("the encoder opens")
                .expect("and mono passes its own round trip");
            assert_eq!(pre_skip, OPUS_PRE_SKIP + OPUS_FRAME as u16);
            let decoded = ec_decode_mono(&packets);
            let (c, lag) = align(&mono, &decoded, 1);
            assert!(
                c >= 0.98,
                "mono at {kbps} kbps came back at correlation {c:.4}"
            );
            let (settled, _) = align(&mono[OPUS_FRAME..], &decoded[OPUS_FRAME..], 1);
            assert!(
                settled >= 0.999,
                "mono at {kbps} kbps past the first frame: correlation {settled:.4}"
            );
            assert_eq!(
                lag,
                usize::from(pre_skip),
                "mono at {kbps} kbps: the declared pre-skip is not where the sound starts"
            );
            assert!(
                decoded.len() - usize::from(pre_skip) >= mono.len(),
                "mono at {kbps} kbps: the track ends {} samples short of the timeline",
                mono.len() + usize::from(pre_skip) - decoded.len()
            );
            let fidelity = opus_fidelity(&packets, &mono, usize::from(pre_skip), 1);
            assert!(
                fidelity >= OPUS_MIN_FIDELITY,
                "mono at {kbps} kbps scored {fidelity:.4} on its own round trip"
            );
        }
    }

    /// The gate on a track too long to listen to whole: it samples, and the
    /// middle of what it hears is the answer. A track whose sound is *not* what
    /// was encoded fails it; one bad five-second window does not, which is
    /// deliberate and measured ([`opus_fidelity`] carries the distribution:
    /// five windows of a good film's mix score under 0.95, the lowest 0.803).
    /// A silent track is not a failure either, which is what it used to be
    /// scored as -- 0.0 against a denominator of nothing, and an all-quiet
    /// timeline fell back to AAC for it.
    ///
    /// 61 s of sound: one second past the minute the gate stops listening whole,
    /// which is what puts this on the sampled path at all.
    #[test]
    fn a_long_track_is_judged_by_the_middle_of_what_it_samples() {
        let pcm = two_tones(61);
        let (packets, pre_skip) = encode_opus(&pcm, 96, 2)
            .expect("the encoder opens")
            .expect("and passes its own round trip");
        assert!(
            packets.len() > FIDELITY_WINDOW * FIDELITY_WINDOWS,
            "{} packets is not the sampled path",
            packets.len()
        );
        let pre_skip = usize::from(pre_skip);
        let whole = opus_fidelity(&packets, &pcm, pre_skip, 2);
        assert!(whole >= OPUS_MIN_FIDELITY, "a good track scored {whole:.4}");

        // One window's worth of the *mix* replaced by its own phase inverse:
        // the packets still decode, they are simply no longer this sound. Placed
        // on a window the gate samples -- window 6 of the twelve, by the same
        // arithmetic the gate spreads them with. The track still passes, and
        // that is the measured decision, not an oversight: this codec really
        // does score 0.80 on five seconds of film it otherwise handles at 0.998.
        let step = (packets.len() - FIDELITY_WINDOW) / (FIDELITY_WINDOWS - 1);
        let from = (6 * step * OPUS_FRAME).saturating_sub(pre_skip) * 2;
        let mut one_bad = pcm.clone();
        for sample in &mut one_bad[from..(from + FIDELITY_WINDOW * OPUS_FRAME * 2).min(pcm.len())] {
            *sample = -*sample;
        }
        let dip = opus_fidelity(&packets, &one_bad, pre_skip, 2);
        assert!(
            dip >= OPUS_MIN_FIDELITY,
            "one ruined window sank the whole track ({dip:.4})"
        );

        // ...and the shape the gate is actually for, which is what a rate the
        // encoder cannot hold does: the track is not the mix any more, not one
        // window of it. The middle of the list moves with it.
        let mut ruined = pcm.clone();
        for sample in &mut ruined[pcm.len() / 4..] {
            *sample = -*sample;
        }
        let ruined = opus_fidelity(&packets, &ruined, pre_skip, 2);
        assert!(
            ruined < OPUS_MIN_FIDELITY,
            "a track that is no longer the mix scored {ruined:.4}"
        );

        // ...and silence is silence: nothing to correlate is not a round trip
        // that failed. Short, because the floor is the same on either path.
        let quiet = vec![0f32; OPUS_RATE as usize * 2 * 2];
        let (packets, pre_skip) = encode_opus(&quiet, 96, 2)
            .expect("the encoder opens")
            .expect("silence is written as Opus, not sent to AAC");
        let fidelity = opus_fidelity(&packets, &quiet, usize::from(pre_skip), 2);
        assert!(
            fidelity >= OPUS_MIN_FIDELITY,
            "a silent track scored {fidelity:.4} and would fall back to AAC"
        );
    }

    /// The one rule that keeps a picture from being converted twice: a frame the
    /// tone map has already brought to BT.709 is never handed the 2020 matrix as
    /// well, on either canvas -- and everything else about the remap is
    /// unchanged by the tone map existing.
    #[test]
    fn a_tone_mapped_span_is_never_matrix_remapped_too() {
        let hdr = ColorDescription {
            matrix: Matrix::Bt2020Ncl,
            transfer: Transfer::Pq,
            full_range: false,
        };
        let sdr = ColorDescription::default(); // BT.601
        // The (from, to) pair of a remap, which is all these cases are about.
        let pair = |remap: Option<Remap>| remap.map(|r| (r.from, r.to));
        for out in [Matrix::Bt709, Matrix::Bt601] {
            let toned = pair(remap_into(Some(hdr), true, out));
            assert!(
                toned.is_none_or(|(from, _)| from == Matrix::Bt709),
                "tone-mapped onto a {out:?} canvas asked for {toned:?}"
            );
            // Untouched by any of this: the 2020 clip that was *not* tone-mapped
            // (an unreachable state today, and still the matrix answer), the
            // ordinary 601-on-709 reconcile, and a gap.
            assert_eq!(
                pair(remap_into(Some(hdr), false, out)),
                (out != Matrix::Bt2020Ncl).then_some((Matrix::Bt2020Ncl, out))
            );
            assert_eq!(
                pair(remap_into(Some(sdr), false, out)),
                (out != Matrix::Bt601).then_some((Matrix::Bt601, out))
            );
            assert_eq!(remap_into(None, false, out), None);
        }
        assert_eq!(
            pair(remap_into(Some(hdr), true, Matrix::Bt601)),
            Some((Matrix::Bt709, Matrix::Bt601)),
            "a tone-mapped clip below 720 lines still takes its 709 -> 601 step"
        );
    }

    /// A full-range source is scaled into the limited span before it is matrix
    /// converted, and only an untouched one: a tone-mapped picture is already
    /// limited, and a limited source has nothing to scale.
    #[test]
    fn a_full_range_source_is_limited_before_it_is_remapped() {
        let full = ColorDescription {
            matrix: Matrix::Bt709,
            transfer: Transfer::Sdr,
            full_range: true,
        };
        let limited = ColorDescription {
            full_range: false,
            ..full
        };
        assert_eq!(
            remap_into(Some(full), false, Matrix::Bt709),
            Some(Remap {
                from: Matrix::Bt709,
                to: Matrix::Bt709,
                limit: true,
            }),
            "same matrix, but the range still has to come down"
        );
        assert_eq!(remap_into(Some(limited), false, Matrix::Bt709), None);
        assert_eq!(remap_into(Some(full), true, Matrix::Bt709), None);
    }

    /// The picture as [`place_picture`] hands it to the encoder: a full-range
    /// source on a canvas of its own size, so the placement is the identity and
    /// what comes out is exactly the range change.
    #[test]
    fn a_full_range_picture_is_placed_as_limited_codes() {
        let (w, h) = (4u32, 2u32);
        let y = [0u8, 255, 130, 130, 0, 255, 130, 130];
        let (u, v) = ([0u8, 255], [255u8, 0]);
        let mut canvas = Composer::new(w, h, crate::scale::FitPolicy::default());
        let (mut turned, mut stretched, mut graded, mut transformed) = (
            (Vec::<u8>::new(), Vec::<u8>::new(), Vec::<u8>::new()),
            (Vec::<u8>::new(), Vec::<u8>::new(), Vec::<u8>::new()),
            (Vec::<u8>::new(), Vec::<u8>::new(), Vec::<u8>::new()),
            (Vec::<u8>::new(), Vec::<u8>::new(), Vec::<u8>::new()),
        );
        let remap = Some(Remap {
            from: Matrix::Bt709,
            to: Matrix::Bt709,
            limit: true,
        });
        let (py, pu, pv, _, _) = place_picture(
            &y,
            &u,
            &v,
            w,
            h,
            Rotation::default(),
            crate::demux::PixelAspect::SQUARE,
            None,
            TransformParams::default(),
            remap,
            None,
            &mut turned,
            &mut stretched,
            &mut graded,
            &mut transformed,
            &mut canvas,
        );
        assert_eq!(py, [16, 235, 128, 128, 16, 235, 128, 128]);
        assert_eq!(pu, [16, 240]);
        assert_eq!(pv, [240, 16]);
    }

    /// A track of three cues, in whichever clock `track` says: `Some(n)` is a
    /// track of the media file, `None` a `.srt` beside it.
    fn subtitles(path: &str, track: Option<u64>) -> SubtitleTrack {
        SubtitleTrack {
            path: path.into(),
            track,
            language: "eng".into(),
            name: String::new(),
            label: "eng".into(),
            bitmap: false,
            cues: vec![
                Cue {
                    start_us: 500_000,
                    end_us: 1_500_000,
                    text: "first".into(),
                    image: None,
                    words: Vec::new(),
                    karaoke: Vec::new(),
                },
                Cue {
                    start_us: 2_000_000,
                    end_us: 3_250_000,
                    text: "second".into(),
                    image: None,
                    words: Vec::new(),
                    karaoke: Vec::new(),
                },
                Cue {
                    start_us: 4_000_000,
                    end_us: 4_750_000,
                    text: "third".into(),
                    image: None,
                    words: Vec::new(),
                    karaoke: Vec::new(),
                },
            ],
            refused: None,
        }
    }

    /// A standalone subtitle file is timed against the timeline itself -- there
    /// is no clip to hang it on -- so a cut moves the pictures and not it. What
    /// it does get is the exported length: a cue past the end of the file would
    /// otherwise be a block after the last picture.
    ///
    /// The pair of this, the embedded clock, is measured against a real exported
    /// file in `tests/export_subs.rs`; both live in one function and this is the
    /// half that needs no encoder.
    #[test]
    fn a_subtitle_file_beside_the_media_keeps_the_timelines_own_clock() {
        // 5 seconds at 30, with `[0.5s, 2.5s)` cut out of it: 3 seconds left.
        let mut project = Project::single("/nonexistent/film.mp4", 150);
        let track = subtitles("/nonexistent/film.srt", None);
        assert!(project.ripple_delete(15, 60));
        assert_eq!(project.timeline_frames(), 90);
        let cues = timeline_cues(&project, &track, 30.0);
        assert_eq!(
            cues,
            vec![
                Cue {
                    start_us: 500_000,
                    end_us: 1_500_000,
                    text: "first".into(),
                    image: None,
                    words: Vec::new(),
                    karaoke: Vec::new(),
                },
                // Clipped to the three seconds the export writes...
                Cue {
                    start_us: 2_000_000,
                    end_us: 3_000_000,
                    text: "second".into(),
                    image: None,
                    words: Vec::new(),
                    karaoke: Vec::new(),
                },
                // ...and the one wholly past the end is not written at all.
            ]
        );
        // A track *of a file* the project no longer plays has no span to be
        // carried through either, so it falls back to the same clock rather
        // than exporting nothing.
        let orphan = subtitles("/nonexistent/other.mkv", Some(2));
        assert_eq!(timeline_cues(&project, &orphan, 30.0).len(), 2);
    }

    /// The embedded clock, without an encoder in it: the cut moves the cues with
    /// the pictures, and a cue nothing clipped keeps its own microseconds.
    #[test]
    fn a_track_of_the_media_is_carried_through_the_cut() {
        let mut project = Project::single("/nonexistent/film.mkv", 150);
        let track = subtitles("/nonexistent/film.mkv", Some(2));
        assert!(project.ripple_delete(15, 60), "cut 0.5s..2.5s out");
        let cues = timeline_cues(&project, &track, 30.0);
        assert_eq!(
            cues,
            vec![
                // The first cue was wholly inside the cut. The second straddled
                // its far edge and kept the 0.75s that is still there...
                Cue {
                    start_us: 500_000,
                    end_us: 1_250_000,
                    text: "second".into(),
                    image: None,
                    words: Vec::new(),
                    karaoke: Vec::new(),
                },
                // ...and the third simply moves back the two seconds that went.
                Cue {
                    start_us: 2_000_000,
                    end_us: 2_750_000,
                    text: "third".into(),
                    image: None,
                    words: Vec::new(),
                    karaoke: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn export_spans_clips_to_the_marked_range_keeping_absolute_start() {
        let project = Project::single("/nonexistent/film.mkv", 150);
        // The whole timeline, unmarked: one span, the source's own length.
        let whole = export_spans(&project, None);
        assert_eq!(whole.len(), 1);
        assert_eq!((whole[0].start, whole[0].len), (0, 150));
        // A range wholly inside the span: `start` stays the timeline's own
        // coordinate ([`composite_color_at`] needs that), and the length is
        // exactly what the mark asked for.
        let clipped = export_spans(&project, Some((30, 100)));
        assert_eq!(clipped.len(), 1);
        assert_eq!((clipped[0].start, clipped[0].len), (30, 70));
        // A range past the end of the media clamps to it rather than reading
        // short on the wrong side.
        let past_end = export_spans(&project, Some((100, 500)));
        assert_eq!((past_end[0].start, past_end[0].len), (100, 50));
    }

    #[test]
    fn bitrate_clamps_at_both_ends() {
        let meta = |width, height, frame_rate| VideoMeta {
            width,
            height,
            frame_rate,
            frame_count: 1,
            codec: crate::demux::Codec::H264,
            color: Default::default(),
            matrix_assumed: false,
            rotation: Rotation::None,
            // Square pixels, coded where they are declared: a meta no container
            // wrote, so nothing was read to disagree with it.
            coded_width: width,
            coded_height: height,
            pixel_aspect: crate::demux::PixelAspect::SQUARE,
        };
        // 1280 * 720 * 30 * 0.1
        assert_eq!(bitrate_for(&meta(1280, 720, 30.0)), 2_764_800);
        assert_eq!(bitrate_for(&meta(320, 240, 30.0)), 230_400, "tiny");
        assert_eq!(
            bitrate_for(&meta(160, 120, 15.0)),
            MIN_BITRATE,
            "tinier floors"
        );
        assert_eq!(bitrate_for(&meta(3840, 2160, 60.0)), MAX_BITRATE, "huge");
        // A rate somebody typed goes through untouched to its own ceiling, which
        // is above where the derived one stops: the card offers 50 Mbps, and a
        // second clamp here would write a file at a bitrate nobody picked.
        let asked = |bps| {
            bitrate_of(
                &meta(1920, 1080, 30.0),
                &ExportSettings {
                    bitrate: Some(bps),
                    ..Default::default()
                },
            )
        };
        assert_eq!(asked(50_000_000), 50_000_000, "the card's ceiling travels");
        assert_eq!(asked(21_000_000), 21_000_000, "past the derived range");
        assert_eq!(asked(600_000), 600_000, "a sub-Mbps ask travels");
        assert_eq!(asked(999_000_000), MAX_EXPLICIT_BITRATE, "still bounded");
        assert_eq!(asked(1), MIN_BITRATE, "and floored");
    }

    /// A clip's fade takes the copy path away, exactly as an equalizer does:
    /// a copied AAC packet cannot carry gain math nobody put in the file. The
    /// bug this guards is [`forces_encode`] gating on `equalized`/`mastered`
    /// but never checking a clip's own `fade_in`/`fade_out` at all -- the
    /// default Web MP4 export then labelled and took the "AAC copy" path for
    /// a project whose picture showed a fade, and the fade never reached the
    /// file.
    #[test]
    fn a_fade_forces_the_audio_to_be_re_encoded() {
        let mut project = Project::single("/nonexistent/film.mp4", 90);
        assert!(!forces_encode(&project, false), "flat, so a copy is fine");
        assert!(project.set_fade_in(
            crate::project::Lane::new(crate::project::LaneKind::Audio, 0),
            0,
            15
        ));
        assert!(
            forces_encode(&project, false),
            "a fade-in on the only clip must force a re-encode"
        );
    }

    fn meta(width: u32, height: u32) -> VideoMeta {
        VideoMeta {
            width,
            height,
            frame_rate: 30.0,
            frame_count: 1,
            codec: crate::demux::Codec::H264,
            color: Default::default(),
            matrix_assumed: false,
            rotation: Rotation::None,
            // Square pixels, coded where they are declared: a meta no
            // container wrote, so nothing was read to disagree with it.
            coded_width: width,
            coded_height: height,
            pixel_aspect: crate::demux::PixelAspect::SQUARE,
        }
    }

    /// The bitrate row a user picked, read as the quality tier it is: a higher
    /// row is a lower QP, the band is honoured at both ends, and the same
    /// picture at the same rate gets the same number whatever its size (the
    /// mapping is bits per *pixel*, which is what makes 720p and 1080p one
    /// table).
    #[test]
    fn the_quality_rows_map_onto_quantisers_that_only_go_one_way() {
        let at = |width, height, mbps: Option<u64>| {
            hevc_qp(
                &meta(width, height),
                &ExportSettings {
                    bitrate: mbps.map(|m| m * 1_000_000),
                    ..Default::default()
                },
            )
        };
        // 20 Mbps at 1080p30 is 0.32 bits a pixel against the 0.6 measured at
        // QP 27, which is just over half: five QP up, and QP is not an
        // arithmetic scale, so this is the number the formula gives.
        assert_eq!(at(1920, 1080, Some(20)), 32);
        // Every step down the card is a step up in QP, never sideways.
        let rows: Vec<i32> = [20, 12, 8, 4, 2, 1]
            .into_iter()
            .map(|mbps| at(1920, 1080, Some(mbps)))
            .collect();
        assert!(
            rows.windows(2).all(|w| w[0] <= w[1]),
            "a lower bitrate row must never code at a lower QP: {rows:?}"
        );
        assert_eq!(*rows.last().unwrap(), HEVC_QP_MAX, "the floor is clamped");
        // ...and the top of the range is the clamp, not an unbounded number: 20
        // Mbps at 320x240 is 8.7 bits a pixel, which no picture needs.
        assert_eq!(at(320, 240, Some(20)), HEVC_QP_MIN);
        // The same bits per pixel is the same quantiser at any size: 1080p at
        // 20 Mbps and 720p at 8.9 Mbps are one picture quality.
        assert_eq!(at(1920, 1080, Some(20)), at(1280, 720, Some(9)));
        // The automatic bitrate is 0.1 bpp at every size, so it is one QP
        // everywhere -- the honest end of "intra-only, large files": the file
        // will be bigger than the row says, and the row buys quality.
        assert_eq!(at(1920, 1080, None), at(1280, 720, None));
    }

    /// An odd picture cannot be cropped back to itself in 4:2:0 -- the
    /// conformance window is stated in chroma samples -- so it is refused by
    /// name rather than written a column wider than the timeline.
    #[test]
    fn an_odd_picture_is_refused_an_hevc_export_by_name() {
        let settings = ExportSettings {
            format: Format::Hevc,
            ..Default::default()
        };
        let refused = Enc::open(&meta(1919, 1080), &settings, false, ColorDescription::default())
            .err()
            .expect("an odd width has no chroma plane")
            .to_string();
        assert!(refused.contains("1919x1080"), "{refused}");
        assert!(refused.contains("even"), "{refused}");
        // ...and the even one it neighbours opens.
        assert!(Enc::open(&meta(1920, 1080), &settings, false, ColorDescription::default()).is_ok());
    }

    /// H.264 codes 16x16 macroblocks and ec-h264's floor is the macroblock
    /// itself -- the old encoder took any positive even size -- so a
    /// sub-macroblock picture is refused by name, like every other seat
    /// refusal, rather than surfaced from inside a running export.
    #[test]
    fn a_sub_macroblock_picture_is_refused_an_h264_export_by_name() {
        let settings = ExportSettings {
            format: Format::Mp4,
            seat: EncoderSeat::Software,
            ..Default::default()
        };
        let refused = Enc::open(&meta(8, 8), &settings, false, ColorDescription::default())
            .err()
            .expect("8x8 is smaller than one macroblock")
            .to_string();
        assert!(refused.contains("8x8"), "{refused}");
        assert!(refused.contains("16x16"), "{refused}");
    }

    /// Weights ramp from just above 0 to just below 1 and never touch either
    /// end -- the frame *after* the window is where a viewer would see plain
    /// B, and the window itself never quite repeats a hard cut's own instant.
    #[test]
    fn dissolve_weight_ramps_strictly_between_zero_and_one() {
        assert_eq!(
            dissolve_weight(0, 1),
            0.5,
            "a one-frame window is the midpoint"
        );
        assert_eq!(dissolve_weight(0, 4), 0.2);
        assert_eq!(dissolve_weight(1, 4), 0.4);
        assert_eq!(dissolve_weight(2, 4), 0.6);
        assert_eq!(dissolve_weight(3, 4), 0.8);
    }

    /// [`dissolve_window`] reads [`crate::project::Clip::transition_out`] back
    /// only once adjacency is re-checked at render time: two clips a split
    /// just made, still touching, still name B by its own source and its own
    /// timeline start.
    #[test]
    fn dissolve_window_names_bs_source_in_frame_window_and_start() {
        use crate::project::{Lane, Project};
        let mut p = Project::single("/nonexistent/film.mkv", 20);
        assert!(p.split(15));
        assert!(p.set_transition_out(Lane::V1, 0, 5));
        let spans = export_spans(&p, None);
        let a_span = spans.iter().find(|s| s.start == 0).expect("clip A's span");
        assert_eq!(
            dissolve_window(&p, a_span),
            Some((0, 15, 5, 15)),
            "b's source, b's in_frame, the window, b's timeline start"
        );
    }

    /// A `transition_out` set while two clips touched, then made stale by a
    /// trim that opens a gap between them: the number is still in storage,
    /// [`dissolve_window`] reads `None` from it, and the span renders as the
    /// hard cut two clips that no longer touch actually are.
    #[test]
    fn a_stale_transition_out_after_a_trim_renders_as_a_hard_cut() {
        use crate::project::{Edge, Lane, Project};
        let mut p = Project::single("/nonexistent/film.mkv", 20);
        assert!(p.split(15));
        assert!(p.set_transition_out(Lane::V1, 0, 5));
        assert!(p.trim(Lane::V1, 1, Edge::Start, 18, &[100]), "opens a gap");
        let spans = export_spans(&p, None);
        let a_span = spans.iter().find(|s| s.start == 0).expect("clip A's span");
        assert!(
            dissolve_window(&p, a_span).is_none(),
            "b no longer abuts a; hard cut"
        );
    }
}

#[cfg(test)]
mod audio_language_tests {
    use super::*;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// `test_multilang.mp4` has an `und` track 0 and a `fra` track 1.
    #[test]
    fn a_stream_states_its_language_or_none() {
        let path = asset("test_multilang.mp4");
        assert_eq!(stream_language(&path, 0), None, "und is no language");
        assert_eq!(stream_language(&path, 1).as_deref(), Some("fra"));
        assert_eq!(stream_language(&path, 9), None, "a track the file does not have");
    }

    /// The tag is the one language every clip shares; any disagreement or any `und` leaves it out.
    #[test]
    fn the_mix_takes_a_language_only_when_every_clip_shares_it() {
        let s = |l: &str| Some(l.to_string());
        assert_eq!(shared_language([s("fra"), s("fra")]), s("fra"));
        assert_eq!(shared_language([s("fra"), s("eng")]), None, "two languages");
        assert_eq!(shared_language([s("fra"), None]), None, "one clip states none");
        assert_eq!(shared_language(Vec::<Option<String>>::new()), None, "no sound at all");
    }

    /// The clips on the audio lanes name the streams the sound comes from: a `und` clip beside
    /// a `fra` one leaves the tag out.
    #[test]
    fn a_mix_of_und_and_french_clips_has_no_tag() {
        let path = asset("test_multilang.mp4");
        let mut p = Project::single(&path, 30);
        assert_eq!(audio_language(&p), None, "the first track is und");
        let fra = p.import(&path, 1);
        assert!(p.append_clip(fra, 30));
        assert_eq!(audio_language(&p), None, "the und clip is still on A1");
    }
}

#[cfg(test)]
mod burn_stack_tests {
    #[test]
    fn two_overlapping_cues_are_both_drawn_stacked() {
        use super::{Burn, Cue, cues_on_screen};
        use crate::text::TextStyle;
        let cue = |text: &str| Cue {
            start_us: 0,
            end_us: 5_000_000,
            text: text.to_string(),
            image: None,
            words: Vec::new(),
            karaoke: Vec::new(),
        };
        let (w, h) = (640u32, 360u32);
        let paint = |cues: Vec<Cue>| {
            let mut burn = Burn {
                cues,
                style: TextStyle {
                    boxed: false,
                    ..TextStyle::default()
                },
                fps: 30.0,
                full_range: false,
                last: None,
                titles: Vec::new(),
                title_last: Vec::new(),
            };
            let y = vec![40u8; (w * h) as usize];
            let u = vec![128u8; (w / 2 * (h / 2)) as usize];
            let v = u.clone();
            burn.paint(30, w, h, (&y, &u, &v))
                .expect("a caption is drawn")
        };
        // The rows that carry a lit (white) pixel in the luma plane.
        let lit_rows = |planes: &(Vec<u8>, Vec<u8>, Vec<u8>)| -> Vec<u32> {
            (0..h)
                .filter(|&r| {
                    let row = &planes.0[(r * w) as usize..((r + 1) * w) as usize];
                    row.iter().any(|&l| l > 40)
                })
                .collect()
        };
        let cues = vec![cue("First cue"), cue("Second cue")];
        assert_eq!(cues_on_screen(&cues, 30, 30.0), vec![0, 1], "both are on screen");
        let one = lit_rows(&paint(vec![cue("First cue")]));
        let both = lit_rows(&paint(cues));
        assert!(
            both.len() >= 2 * one.len() - 1,
            "second cue not drawn: {} lit rows against {} for the first alone",
            both.len(),
            one.len()
        );
        // The first cue keeps its place on the bottom edge; the second stacks above.
        assert_eq!(both.last(), one.last(), "the first cue moved");
    }
}

#[cfg(test)]
mod chroma_420_tests {
    use super::*;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// The layout a chroma plane's sample count names, for each subsampling.
    #[test]
    fn a_chroma_count_names_its_layout() {
        assert_eq!(chroma_shape(64 * 64, 64, 64), Some((64, 64)), "4:4:4");
        assert_eq!(chroma_shape(32 * 64, 64, 64), Some((32, 64)), "4:2:2");
        assert_eq!(chroma_shape(32 * 32, 64, 64), Some((32, 32)), "4:2:0");
        assert_eq!(chroma_shape(3 * 3, 5, 3), Some((3, 3)), "odd 4:4:4 keeps its width");
        assert_eq!(chroma_shape(3 * 2, 5, 3), Some((3, 2)), "odd 4:2:2");
        assert_eq!(chroma_shape(7, 5, 3), None, "no layout has seven samples");
    }

    /// A 2x2 block of 4:4:4 chroma becomes its rounded mean.
    #[test]
    fn a_444_block_averages_to_its_rounded_mean() {
        let src: Vec<u16> = vec![1, 2, 3, 4];
        assert_eq!(chroma_420(&src, 2, 2, 1, 1), vec![3], "(10 + 2) / 4");
        let src: Vec<u16> = vec![0, 0, 0, 1];
        assert_eq!(chroma_420(&src, 2, 2, 1, 1), vec![0], "a quarter rounds down");
        let src: Vec<u16> = vec![0, 0, 1, 1];
        assert_eq!(chroma_420(&src, 2, 2, 1, 1), vec![1], "a half rounds up");
    }

    /// 4:2:2 has full height, so only the rows pair up.
    #[test]
    fn a_422_plane_pairs_rows_only() {
        // Two columns, four rows: rows (0,1) and (2,3) average, columns stay apart.
        let src: Vec<u8> = vec![10, 100, 20, 200, 30, 0, 40, 0];
        assert_eq!(chroma_420(&src, 2, 4, 2, 2), vec![15, 150, 35, 0]);
    }

    /// An odd edge averages the samples there are, and 10-bit samples stay in 10 bits.
    #[test]
    fn an_odd_edge_and_ten_bit_samples_are_handled() {
        // 3x3 chroma down to 2x2: the last column and row are a single sample each,
        // so the corner is the 8 alone and the edge pairs are (2, 5) and (6, 7).
        let src: Vec<u16> = (0..9).collect();
        assert_eq!(chroma_420(&src, 3, 3, 2, 2), vec![2, 4, 7, 8]);
        // A 10-bit mean is still a 10-bit value, not a truncated 8-bit one.
        let src: Vec<u16> = vec![1023, 1023, 1022, 1023];
        assert_eq!(chroma_420(&src, 2, 2, 1, 1), vec![1023]);
    }

    /// The AV1 seat hands the composer 4:2:0 planes for every layout edith decodes
    /// (4:2:0, 4:2:2, 4:4:4 and gray), at the size a 4:2:0 picture would have.
    #[test]
    fn every_av1_fixture_arrives_as_420() {
        for name in ["test_av1_444.mp4", "test_av1_422.mp4", "test_av1_12bit.mp4"] {
            let mut pictures = Pictures::software(&asset(name), 0, false, Deinterlace::Frame).expect(name);
            let Ok(Some(Yuv::Pixels { u, v, width, height, .. })) = pictures.next(None) else {
                panic!("{name}: no picture");
            };
            let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
            assert_eq!((u.len(), v.len()), (cw * ch, cw * ch), "{name}: chroma is 4:2:0");
        }
        for name in ["test_av1_gray.mp4", "test_av1_420.mp4"] {
            let mut pictures = Pictures::software(&asset(name), 0, false, Deinterlace::Frame).expect(name);
            let Ok(Some(Yuv::Pixels { y, u, v, width, height })) = pictures.next(None) else {
                panic!("{name}: no picture");
            };
            let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
            assert_eq!(y.len(), (width * height) as usize, "{name}: luma");
            assert_eq!((u.len(), v.len()), (cw * ch, cw * ch), "{name}: chroma is 4:2:0");
            if name.contains("gray") {
                assert!(u.iter().chain(&v).all(|&s| s == 128), "{name}: grey chroma is 128");
            }
        }
    }
}

#[cfg(test)]
mod planned_sound_tests {
    use super::*;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// The sound [`planned_seats`] predicts for `timeline` in an mp4, with the
    /// picture's meta taken from `picture`.
    fn predicted_sound(timeline: &str, picture: &str) -> &'static str {
        let project = Project::single(asset(timeline), 30);
        let (meta, _) = Demuxer::open(&asset(picture)).expect("the picture's file opens");
        let settings = ExportSettings { format: Format::Mp4, ..Default::default() };
        planned_seats(&project, &meta, &settings).1
    }

    /// A DTS track is decoded by `ec-dts`, so the export predicts an encode of its
    /// sound (as it does for an mp3), not the "can't be decoded" card.
    #[test]
    fn a_dts_source_is_predicted_as_an_encode() {
        assert_eq!(
            predicted_sound("test_dts.mkv", "test_dts.mkv"),
            "AAC · SW encode (ec-aac)"
        );
    }

    /// An mp3 on the timeline has no sample table to copy from: the sound is
    /// decoded and encoded as AAC, not copied.
    #[test]
    fn a_standalone_mp3_source_is_predicted_as_an_encode() {
        assert_eq!(
            predicted_sound("test_tone.mp3", "test_av.mp4"),
            "AAC · SW encode (ec-aac)"
        );
    }

    /// The untouched AAC of an mp4 is still the copy it was.
    #[test]
    fn an_untouched_aac_mp4_is_still_predicted_as_a_copy() {
        assert_eq!(predicted_sound("test_av.mp4", "test_av.mp4"), "AAC copy");
    }

    /// The HDR meta and picture size of a fixture, as the export reads them.
    fn meta_of(name: &str) -> VideoMeta {
        Demuxer::open(&asset(name)).expect("the fixture opens").0
    }

    /// A PQ source is tone-mapped to SDR in every picture export, and the plan says so.
    #[test]
    fn a_pq_source_is_predicted_as_tone_mapped() {
        let project = Project::single(asset("test_pq.mp4"), 30);
        let meta = meta_of("test_pq.mp4");
        let settings = ExportSettings { format: Format::Mp4, ..Default::default() };
        let expected = match ColorDescription::output_sized(meta.width, meta.height).matrix {
            Matrix::Bt601 => "HDR (PQ) source tone-mapped to SDR BT.601",
            _ => "HDR (PQ) source tone-mapped to SDR BT.709",
        };
        assert_eq!(planned_tone(&project, &meta, &settings).as_deref(), Some(expected));
    }

    /// The matrix the words name is the one the file is tagged with: a standard-definition
    /// canvas is BT.601 and an HD one BT.709, whatever the source's own size, and a
    /// `size` setting moves the canvas the rule is applied to.
    #[test]
    fn the_tone_map_names_the_matrix_of_the_size_it_is_written_at() {
        let project = Project::single(asset("test_pq.mp4"), 30);
        let mut hd = meta_of("test_pq.mp4");
        hd.width = 1920;
        hd.height = 1080;
        let mut sd = hd;
        sd.width = 720;
        sd.height = 576;
        let mp4 = ExportSettings { format: Format::Mp4, ..Default::default() };
        assert_eq!(
            planned_tone(&project, &hd, &mp4).as_deref(),
            Some("HDR (PQ) source tone-mapped to SDR BT.709")
        );
        assert_eq!(
            planned_tone(&project, &sd, &mp4).as_deref(),
            Some("HDR (PQ) source tone-mapped to SDR BT.601"),
            "720x576 is SD, and is tagged BT.601"
        );
        let small = ExportSettings { size: Some((640, 360)), ..mp4.clone() };
        assert_eq!(
            planned_tone(&project, &hd, &small).as_deref(),
            Some("HDR (PQ) source tone-mapped to SDR BT.601"),
            "a 640x360 export is SD though the project is HD"
        );
        let hd_again = ExportSettings { size: Some((1280, 720)), ..mp4.clone() };
        assert_eq!(
            planned_tone(&project, &sd, &hd_again).as_deref(),
            Some("HDR (PQ) source tone-mapped to SDR BT.709"),
            "a 1280x720 export of an SD canvas is HD"
        );
    }

    /// A preview that keeps its source's curve is not tone-mapped, and an SDR timeline or a
    /// sound-only format has nothing to say.
    #[test]
    fn only_a_tone_mapped_picture_is_predicted_as_one() {
        let pq = Project::single(asset("test_hdr_sei.mkv"), 30);
        let meta = meta_of("test_hdr_sei.mkv");
        let keep = ExportSettings {
            format: Format::Mp4,
            keep_source_colour: true,
            ..Default::default()
        };
        assert_eq!(planned_tone(&pq, &meta, &keep), None, "a preview keeps the curve");
        let mp4 = ExportSettings { format: Format::Mp4, ..Default::default() };
        assert!(planned_tone(&pq, &meta, &mp4).is_some(), "the mkv's PQ too");
        let sdr = Project::single(asset("test_av.mp4"), 30);
        assert_eq!(planned_tone(&sdr, &meta, &mp4), None, "an SDR timeline");
        let wav = ExportSettings { format: Format::Wav, ..Default::default() };
        assert_eq!(planned_tone(&pq, &meta, &wav), None, "no picture to tone-map");
    }

    /// A size setting draws the export at that size, coded size included, and leaves every
    /// other fact of the project's canvas as it is; no size is the project's canvas.
    #[test]
    fn a_size_setting_moves_the_canvas_and_only_the_canvas() {
        let meta = meta_of("test_av.mp4");
        let none = ExportSettings::default();
        assert_eq!(output_meta(&meta, &none), meta);
        let small = ExportSettings { size: Some((640, 360)), ..Default::default() };
        let out = output_meta(&meta, &small);
        assert_eq!((out.width, out.height), (640, 360));
        assert_eq!((out.coded_width, out.coded_height), (640, 360));
        assert_eq!(out.frame_rate, meta.frame_rate);
        assert_eq!(out.frame_count, meta.frame_count);
        assert_eq!(out.pixel_aspect, meta.pixel_aspect);
    }

    /// An mkv export that is encoded names the codec its mix will get: Opus when the sources
    /// put it at 48 kHz mono or stereo, AAC otherwise. `test_av1.mkv` is 44.1 kHz stereo AAC,
    /// so its export is AAC, as its done line says; `test_seek_chirp.mkv` is 48 kHz mono Opus.
    #[test]
    fn a_matroska_export_names_the_codec_its_mix_will_get() {
        let settings = ExportSettings { format: Format::Av1, ..Default::default() };
        let aac = Project::single(asset("test_av1.mkv"), 30);
        let meta = meta_of("test_av1.mkv");
        let (_, sound) = planned_seats(&aac, &meta, &settings);
        assert_eq!(sound, "AAC · SW encode (ec-aac)");
        let opus = Project::single(asset("test_seek_chirp.mkv"), 30);
        let meta = meta_of("test_seek_chirp.mkv");
        let (_, sound) = planned_seats(&opus, &meta, &settings);
        assert_eq!(sound, "Opus · SW encode (ec-opus)");
    }
}

#[cfg(test)]
mod vp9_444_tests {
    use super::*;

    #[test]
    fn a_422_or_444_source_names_its_reduction_on_a_video_export() {
        let mut settings = ExportSettings { format: Format::Mp4, ..Default::default() };
        assert_eq!(
            chroma_reduction_words(Codec::Vp9, Chroma::C444, &settings).as_deref(),
            Some("VP9 4:4:4 source: chroma reduced to 4:2:0 for export"),
        );
        assert_eq!(
            chroma_reduction_words(Codec::Vp9, Chroma::C422, &settings).as_deref(),
            Some("VP9 4:2:2 source: chroma reduced to 4:2:0 for export"),
        );
        assert_eq!(
            chroma_reduction_words(Codec::H264, Chroma::C444, &settings).as_deref(),
            Some("H.264 4:4:4 source: chroma reduced to 4:2:0 for export"),
        );
        assert!(chroma_reduction_words(Codec::Vp9, Chroma::C420, &settings).is_none());
        assert!(chroma_reduction_words(Codec::Vp9, Chroma::Gray, &settings).is_none());
        for format in [Format::Wav, Format::Flac, Format::Mp3, Format::Ogg, Format::M4a, Format::Opus] {
            settings.format = format;
            assert!(
                chroma_reduction_words(Codec::Vp9, Chroma::C444, &settings).is_none(),
                "{format:?} has no picture to reduce",
            );
        }
    }

    /// A VP9 profile 1 4:4:4 file is never offered to the GPU on an export: the decode
    /// door's gate refuses it before the plugin is asked, so the plugin's refusal never
    /// reaches stderr. Stderr is not read in-process; the seam is the plugin's attempt count.
    #[test]
    fn a_vp9_444_export_never_offers_the_file_to_the_gpu() {
        let path = std::env::temp_dir().join(format!("edith-vp9-444-{}.webm", std::process::id()));
        let generated = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=64x64:rate=5:duration=1"])
            .args(["-c:v", "libvpx-vp9", "-pix_fmt", "yuv444p", "-f", "webm"])
            .arg(&path)
            .status()
            .map(|status| status.success() && path.exists())
            .unwrap_or(false);
        if !generated {
            eprintln!("no ffmpeg with libvpx-vp9: the 4:4:4 fixture cannot be generated, skipping");
            return;
        }
        let chroma = Demuxer::open(&path).ok().and_then(|(_, demuxer)| demuxer.chroma());
        assert_eq!(chroma, Some(Chroma::C444), "the fixture is a 4:4:4 stream");
        let (tx, rx) = sync_channel(64);
        let before = crate::hw::OPEN_ATTEMPTS.with(|n| n.get());
        let _ = decode_stream(&path, 0, None, &tx, None, false, Deinterlace::Frame);
        let after = crate::hw::OPEN_ATTEMPTS.with(|n| n.get());
        drop(rx);
        let _ = std::fs::remove_file(&path);
        assert_eq!(after, before, "the GPU was asked for a decoder for a 4:4:4 VP9 file");
    }
}
