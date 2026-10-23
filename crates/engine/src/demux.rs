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

use crate::colorspace::{
    Chroma, ColorDescription, ContentLight, Tags, bitstream_chroma, bitstream_tags,
};
use crate::video_profile;
use ec_h264_syntax::{AnnexBIter, Pps, Sps, unescape_rbsp};
use ec_core::Demuxer as _;

pub(crate) mod avi;
pub mod captions;
pub(crate) mod dv;
pub(crate) mod ps;
pub(crate) mod ts;
pub use avi::is_avi;
pub use dv::is_dv;
pub use ps::{carries_picture, is_mpeg_system, is_mpegps};
pub use ts::is_mpegts;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// What a cancelled read fails with. A message rather than a variant because
/// [`crate::Error`] is a boxed error and always was; [`is_cancelled`] is what
/// tells this apart from a file that really would not open, so an abandoned
/// read is never reported to anyone as a broken file.
const CANCELLED: &str = "read cancelled";

thread_local! {
    /// The flag the read running on *this* thread is abandoned by, installed by
    /// [`with_cancel`].
    ///
    /// Thread-scoped rather than passed down, and deliberately: a read is a
    /// thread here -- an import worker, an audio feeder, a decode worker -- and
    /// the walk that has to be stopped sits five frames under an `open` with a
    /// dozen callers, none of which have anything to cancel. This way the two
    /// that do say so at the top of their own thread and every loop below them
    /// obeys, at the price of one thread-local load per cluster.
    static CANCEL: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `read` with `flag` as this thread's cancel: setting it abandons the
/// walks inside at their next element, with [`CANCELLED`] rather than a verdict
/// about the file. Restores whatever was installed before, so a nested read is
/// not silently un-cancelled.
pub fn with_cancel<T>(
    flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    read: impl FnOnce() -> T,
) -> T {
    let previous = CANCEL.with(|cell| cell.borrow_mut().replace(flag.clone()));
    let out = read();
    CANCEL.with(|cell| *cell.borrow_mut() = previous);
    out
}

/// Whether the read on this thread has been abandoned; `false` where nobody
/// installed a flag, which is every caller that has nothing to cancel.
fn cancelled() -> bool {
    CANCEL.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    })
}

/// Whether `error` is a read that was abandoned rather than a file that could
/// not be read: what keeps a cancelled import off the notice bar and a
/// superseded audio open off stderr.
pub fn is_cancelled(error: &crate::Error) -> bool {
    error.to_string() == CANCELLED
}

/// The mp4 arm's half of [`cancelled`]: the Matroska walk polls the flag itself
/// between elements, but `Mp4Reader::read_header` is the `mp4` crate's own loop
/// over the `moov`, called once and not returned to until the whole header is
/// parsed. This is the same poll wearing a `Read`/`Seek` face instead, so a
/// cancel mid-`moov` surfaces exactly where the crate next asks this file for
/// bytes -- as an [`io::Error`](std::io::Error) that `Mp4Reader::read_header`
/// has no choice but to propagate, in [`CANCELLED`]'s own words so
/// [`is_cancelled`] tells it apart from the file the same way either arm does.
struct CancelableReader<R>(R);

impl<R: Read> Read for CancelableReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if cancelled() {
            return Err(std::io::Error::other(CANCELLED));
        }
        self.0.read(buf)
    }
}

impl<R: Seek> Seek for CancelableReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

/// The same answer a walk gives up with on its own, for a caller that finds
/// its cancel already set before there is any walk to abandon -- a proxy's
/// header read finding its flag up the moment its seat is claimed, say. Kept
/// to one string so [`is_cancelled`] answers both the same way.
pub fn cancelled_read_error() -> crate::Error {
    CANCELLED.into()
}

/// What the video track is coded with. Not a decoder choice by itself: every
/// codec here has a software decoder in this binary (`ec-h264`, `ec-h265`,
/// `ec-av1`, `ec-vp9`, `ec-vp8`), so the plugin is an alternative to them, not a
/// requirement, for playback and export alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Vp9,
    Vp8,
    Av1,
    /// MPEG-1 and MPEG-2 video, out of a transport stream only.
    Mpeg2,
    /// Apple ProRes (422 Proxy to 4444 XQ): intra-only, software seat only.
    ProRes,
    /// Avid DNxHD / DNxHR: intra-only, software seat only.
    Dnxhd,
    /// Motion JPEG: intra-only, full range, software seat only.
    Mjpeg,
    /// DV video (DV25, DVCPRO25/50, DVCPRO HD): intra-only, software seat only.
    Dv,
    /// MPEG-4 Part 2 (Simple, Advanced Simple) and H.263 baseline: software seat only.
    Mpeg4,
    /// Microsoft MPEG-4 version 2 and 3 (DivX 3: `MP42`, `DIV3`...), in an AVI: software seat only.
    MsMpeg4,
    /// Uncompressed video in an AVI (`BI_RGB` 24 and 32 bit, YUY2, UYVY, I420, YV12): software
    /// seat only.
    Raw,
}

impl Codec {
    /// How a refusal names it, and how a mismatch reads to a user.
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Vp9 => "VP9",
            Self::Vp8 => "VP8",
            Self::Av1 => "AV1",
            Self::Mpeg2 => "MPEG-2",
            Self::ProRes => "ProRes",
            Self::Dnxhd => "DNxHD",
            Self::Mjpeg => "MJPEG",
            Self::Dv => "DV",
            Self::Mpeg4 => "MPEG-4",
            Self::MsMpeg4 => "MS-MPEG4",
            Self::Raw => "Raw",
        }
    }

    /// The refusal: the plugin *is* here and opened the file, and then
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
    (Codec::Vp8, "V_VP8", b"vp08"),
    (Codec::Av1, "V_AV1", b"av01"),
    (Codec::Mjpeg, "V_MJPEG", b"jpeg"),
    // MPEG-4 Part 2: Simple, Advanced Simple and the (rare) Advanced profile ids are one
    // decoder; `mp4v` carries the VOL header in its `esds`.
    (Codec::Mpeg4, "V_MPEG4/ISO/ASP", b"mp4v"),
    (Codec::Mpeg4, "V_MPEG4/ISO/SP", b"mp4v"),
    (Codec::Mpeg4, "V_MPEG4/ISO/AP", b"mp4v"),
];

/// The mp4 sample entries that name a codec beside its [`CODEC_IDS`] row, or that have no
/// Matroska id to share a row with: `mjpa` (Motion-JPEG A), the six ProRes flavours (Proxy,
/// LT, Standard, HQ, 4444, 4444 XQ) and Avid's `AVdn` (DNxHD) / `AVdh` (DNxHR). The Matroska
/// half of DNxHD is [`mkv_private_codec`]: its ids are generic ones that name the codec in
/// `CodecPrivate`; ProRes has no Matroska id here ([`UNSUPPORTED`]).
const MP4_ONLY: &[(Codec, &[u8; 4])] = &[
    (Codec::Mjpeg, b"mjpa"),
    // MJPEG-B: `ec-mjpeg` rewraps each field as a baseline JPEG.
    (Codec::Mjpeg, b"mjpb"),
    // Uncompressed video: `raw ` (RGB), `2vuy`, `yuvs`, `yuv2`, `v210`, `v410`, `b16g`. `raw ` and
    // `yuvs` are PCM audio entries too; only a track whose handler is `vide` is a picture here.
    (Codec::Raw, b"raw "),
    (Codec::Raw, b"2vuy"),
    (Codec::Raw, b"yuvs"),
    (Codec::Raw, b"yuv2"),
    (Codec::Raw, b"v210"),
    (Codec::Raw, b"v410"),
    (Codec::Raw, b"b16g"),
    (Codec::ProRes, b"apco"),
    (Codec::ProRes, b"apcs"),
    (Codec::ProRes, b"apcn"),
    (Codec::ProRes, b"apch"),
    (Codec::ProRes, b"ap4h"),
    (Codec::ProRes, b"ap4x"),
    (Codec::Dnxhd, b"AVdn"),
    (Codec::Dnxhd, b"AVdh"),
    // H.263 baseline: the 3GPP (`s263`) and QuickTime (`h263`) entries.
    (Codec::Mpeg4, b"s263"),
    (Codec::Mpeg4, b"h263"),
    // DV: the QuickTime entries for DV25 / DVCPRO25 (`dvc `, `dvcp`, `dvpp`, `dvcs`, `dvis`),
    // DVCPRO50 (`dv5n`, `dv5p`, `dv5q`) and DVCPRO HD (`dvh*`). A frame names its own system.
    (Codec::Dv, b"dvc "),
    (Codec::Dv, b"dvcp"),
    (Codec::Dv, b"dvpp"),
    (Codec::Dv, b"dvcs"),
    (Codec::Dv, b"dvis"),
    (Codec::Dv, b"dv5n"),
    (Codec::Dv, b"dv5p"),
    (Codec::Dv, b"dv5q"),
    (Codec::Dv, b"dvhq"),
    (Codec::Dv, b"dvh1"),
    (Codec::Dv, b"dvh2"),
    (Codec::Dv, b"dvh3"),
    (Codec::Dv, b"dvh4"),
    (Codec::Dv, b"dvh5"),
    (Codec::Dv, b"dvh6"),
    (Codec::Dv, b"dvhp"),
    (Codec::Dv, b"dvhs"),
    (Codec::Dv, b"dvhn"),
    (Codec::Dv, b"dvhr"),
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
    ("V_MPEG2", "no MPEG-2 decoder, hardware or software"),
    (
        "V_MS/VFW/FOURCC",
        "a codec inside a BITMAPINFOHEADER; only Avid DNxHD/DNxHR (AVdn, AVdh) and the XviD / DivX 4-5 / FMP4 MPEG-4 Part 2 tags are unwrapped, nothing else",
    ),
    (
        "V_QUICKTIME",
        "a QuickTime sample description inside Matroska; only Avid DNxHD/DNxHR (AVdn, AVdh) is unwrapped, nothing else",
    ),
    (
        "V_PRORES",
        "Apple ProRes in Matroska; ec-matroska maps no ProRes id, so ProRes is read from QuickTime/MP4 only",
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

/// The Matroska video codecs named only in `CodecPrivate`: Avid DNxHD/DNxHR inside a
/// `V_QUICKTIME` track (the QuickTime sample description, whose four-character code follows
/// its size) or a `V_MS/VFW/FOURCC` track (the BITMAPINFOHEADER's `biCompression`). The same
/// rule `ec-matroska` applies.
fn mkv_private_codec(id: &str, private: &[u8]) -> Option<Codec> {
    let tag = match id {
        "V_QUICKTIME" => private.get(4..8)?,
        "V_MS/VFW/FOURCC" => private.get(16..20)?,
        _ => return None,
    };
    if matches!(tag, b"AVdn" | b"AVdh") {
        return Some(Codec::Dnxhd);
    }
    // A VfW track of the XviD / DivX 4-5 / FMP4 family (not MS-MPEG4 `DIV3`), as `ec-matroska`.
    let up = tag.to_ascii_uppercase();
    (id == "V_MS/VFW/FOURCC"
        && matches!(&up[..], b"XVID" | b"XVIX" | b"DIVX" | b"DX50" | b"FMP4" | b"MP4V" | b"M4S2" | b"RMP4"))
    .then_some(Codec::Mpeg4)
}

/// Whether an MPEG-4 Part 2 access unit carries its own Visual Object Sequence / Object
/// Layer header (`00 00 01 B0`, `B5`, or a `20..2F` layer start) ahead of the first VOP
/// (`B6`): where it does, the configuration out of the container is not injected again.
pub(crate) fn has_vol(au: &[u8]) -> bool {
    for w in au.windows(4) {
        if w[..3] == [0, 0, 1] {
            match w[3] {
                0xB0 | 0xB5 | 0x20..=0x2F => return true,
                0xB6 => return false,
                _ => {}
            }
        }
    }
    false
}

/// Which codec an mp4 `stsd` sample entry fourcc names. The mp4 half of
/// [`CODEC_IDS`], plus `hev1`: HEVC arrives under either fourcc and the record
/// the parameter sets come out of is the same `hvcC` either way; and the
/// [`MP4_ONLY`] entries.
fn mp4_codec(fourcc: &[u8; 4]) -> Option<Codec> {
    if fourcc == b"hev1" {
        return Some(Codec::Hevc);
    }
    if let Some(&(codec, _)) = MP4_ONLY.iter().find(|(_, f)| *f == fourcc) {
        return Some(codec);
    }
    CODEC_IDS
        .iter()
        .find(|(_, _, mp4)| *mp4 == fourcc)
        .map(|&(codec, ..)| codec)
}

/// An `ec-mp4` error as the engine's: a cancelled read keeps its own message
/// ([`cancelled_read_error`]'s words) so [`is_cancelled`] still recognises it.
///
/// Two of `ec-mp4`'s answers are the engine's plain words for a bad file. A file
/// that stops before its `moov` is a truncated copy: `ec-mp4` says "need more
/// data", which means nothing to someone holding a file. A file that is not an
/// MP4 or QuickTime container at all (a text file given an `.mp4` name) is "not
/// an mp4 or QuickTime container". Both keep the words the front ends match on
/// ("a box with a larger size than it", "ftyp not found") so probe and import
/// give the same refusal for them.
pub(crate) fn mp4_error(e: ec_core::Error) -> crate::Error {
    match e {
        ec_core::Error::Io(io) => io.into(),
        ec_core::Error::NeedMore => {
            "file contains a box with a larger size than it: the file is truncated, \
             its movie header (moov) is missing or cut short"
                .to_string()
                .into()
        }
        ec_core::Error::Unsupported { what, .. } if what == "this file" => {
            "ftyp not found: it is not an mp4 or QuickTime container".to_string().into()
        }
        other => other.to_string().into(),
    }
}

/// The first 16 bytes of an ASF file: the GUID of its Header Object, which every Windows Media
/// file opens with, WMA and WMV alike.
const ASF_HEADER_GUID: [u8; 16] = [
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];

/// The refusal an ASF / WMV file gets: named, and never the mp4 "ftyp" one.
pub(crate) const ASF_REFUSAL: &str =
    "ASF/WMV (Windows Media) files are not supported: convert to MP4 or MKV";

/// Refuses an ASF / WMV file by name ([`ASF_REFUSAL`]). Every other file passes, and so does
/// one that cannot be read here: the reader that opens it says why.
pub(crate) fn refuse_asf(path: &Path) -> crate::Result<()> {
    let mut head = [0u8; 16];
    let is_asf = File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok()
        && head == ASF_HEADER_GUID;
    match is_asf {
        true => Err(ASF_REFUSAL.to_string().into()),
        false => Ok(()),
    }
}

/// The refusal an audio-only Speex file gets (an Ogg whose first packet is a Speex header).
pub(crate) const SPEEX_REFUSAL: &str = "Speex audio is not supported";

/// Whether `path` is an Ogg file whose first packet is a Speex header (`Speex   `, eight bytes).
/// The Ogg page header is 27 bytes, then one lacing byte per segment (`head[26]` of them), then
/// the packet itself.
pub(crate) fn is_ogg_speex(path: &Path) -> bool {
    let mut head = [0u8; 4096];
    let Ok(read) = File::open(path).and_then(|mut f| f.read(&mut head)) else {
        return false;
    };
    let head = &head[..read];
    if !head.starts_with(b"OggS") || head.len() < 27 {
        return false;
    }
    let at = 27 + usize::from(head[26]);
    head.get(at..).is_some_and(|packet| packet.starts_with(b"Speex   "))
}

/// Refuses an audio-only Speex file by name ([`SPEEX_REFUSAL`]).
pub(crate) fn refuse_speex(path: &Path) -> crate::Result<()> {
    match is_ogg_speex(path) {
        true => Err(SPEEX_REFUSAL.to_string().into()),
        false => Ok(()),
    }
}

/// The one refusal that is an **answer**: this file carries no picture at all
/// that this engine reads -- a song in a video container, an audio-only mp4 or
/// Matroska, a `.mka` under any name.
///
/// A type rather than a sentence, because two callers want two different things
/// from it. A front-end shows its `Display`, which is the words it always was,
/// and refuses the import by them. [`crate::proxy`] wants to tell it from *"this
/// file is broken"*: a song is not a film to stand in for and saying "NO PROXY"
/// about one is a refusal nobody asked for, while a film that will not open is
/// worth a line. Neither could tell them apart by matching on prose, and prose
/// is the one thing in this engine that is free to be reworded.
///
/// A file whose picture is in a codec nothing here decodes is **not** this: it
/// has a video track, the refusal names the codec, and that stays loud.
#[derive(Debug)]
pub struct NoVideoTrack(String);

/// A container that states a picture larger than this engine will compose:
/// past [`crate::MAX_RESOLUTION`], where a per-frame buffer stops being a
/// sane thing to allocate. Refused by name, like the other named refusals,
/// because "this file is too big to open" is something a user can act on and
/// a capacity overflow one layer down is not.
#[derive(Debug)]
pub struct UnsupportedDisplaySize(u64);

impl std::fmt::Display for UnsupportedDisplaySize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this file states a {} pixel picture, past the {} pixel maximum",
            self.0,
            crate::MAX_RESOLUTION
        )
    }
}

impl std::error::Error for UnsupportedDisplaySize {}

fn unsupported_display_size(stated: u64) -> crate::Error {
    UnsupportedDisplaySize(stated).into()
}

impl UnsupportedDisplaySize {
    /// Whether an `error` is one of these, however deep in a `Box` it is --
    /// [`NoVideoTrack::is_it`]'s and [`UnsupportedRotation::is_it`]'s twin,
    /// for a caller that shows the refusal differently from the others.
    pub fn is_it(error: &crate::Error) -> bool {
        error.downcast_ref::<Self>().is_some()
    }
}

impl std::fmt::Display for NoVideoTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoVideoTrack {}

impl NoVideoTrack {
    /// Whether `error` is one of these, however deep in a `Box` it is: the
    /// question [`crate::proxy`] asks, in one place so no caller downcasts by
    /// hand.
    pub fn is_it(error: &crate::Error) -> bool {
        error.downcast_ref::<Self>().is_some()
    }
}

/// How far a container's display matrix says the picture must be turned to be
/// seen the way it was shot: quarter turns **clockwise**, the direction
/// [`crate::scale::rotate_i420_90s`] takes and the one a clip's own
/// [`TransformParams`](crate::transform::TransformParams) rotate is measured
/// in.
///
/// This is metadata about the *pixels*, not a user's edit: an mp4's `tkhd`
/// matrix, a phone's 90-degree turn, and every consumer of this engine's
/// pictures -- the preview and the encoder alike -- has to make it, or the film
/// plays and exports sideways. [`VideoMeta::width`] and
/// [`VideoMeta::height`] are therefore the *displayed* size: the coded picture
/// is the swapped one, and every picture this engine hands out is turned to
/// match. A container asking for anything that is not a right angle is refused
/// at the door ([`UnsupportedRotation`]) rather than shown wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    #[default]
    None,
    Cw90,
    Cw180,
    Cw270,
}

impl Rotation {
    /// Quarter turns clockwise, the count
    /// [`crate::scale::rotate_i420_90s`] applies.
    pub fn steps(self) -> u8 {
        match self {
            Rotation::None => 0,
            Rotation::Cw90 => 1,
            Rotation::Cw180 => 2,
            Rotation::Cw270 => 3,
        }
    }

    /// Whether the turned picture's width and height are the coded ones
    /// swapped.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Cw90 | Rotation::Cw270)
    }

    /// Whether there is anything to do at all, asked exactly as
    /// [`TransformParams::is_identity`](crate::transform::TransformParams::is_identity)
    /// is: the common case of a file that was shot the way it is stored.
    pub fn is_none(self) -> bool {
        matches!(self, Rotation::None)
    }
}

/// A container that asks for a turn this engine cannot make: a display matrix
/// that is not a pure right-angle rotation -- a mirror, a shear, or an angle
/// that is not a multiple of 90 degrees.
///
/// Named and refused rather than ignored, for [`NoVideoTrack`]'s reason prose
/// might have to be reworded: a file whose picture is 45 degrees off has no
/// correct rendition here, and showing it sideways is a wrong picture rather
/// than a missing feature. The `Display` is what a user is refused by.
#[derive(Debug)]
pub struct UnsupportedRotation {
    /// The angle the matrix states, where it states one at all -- `None` for a
    /// mirror or a shear, which is not one -- for the sentence only. It is
    /// counter-clockwise, as ffprobe reports a file's `rotation` side data, for
    /// both containers: an mp4 matrix and an mkv roll that name the same turn
    /// state the same number.
    pub degrees: Option<f64>,
}

impl std::fmt::Display for UnsupportedRotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.degrees {
            Some(degrees) => write!(
                f,
                "this file carries a display matrix of {degrees:.1}°, which is not a quarter \
                 turn; only 0°, 90°, 180° and 270° can be straightened here"
            ),
            None => f.write_str(
                "this file's display matrix is not a rotation at all (a mirror or a shear); only \
                 0°, 90°, 180° and 270° can be straightened here",
            ),
        }
    }
}

impl std::error::Error for UnsupportedRotation {}

impl UnsupportedRotation {
    /// Whether `error` is one of these, however deep in a `Box` it is --
    /// [`NoVideoTrack::is_it`]'s twin.
    pub fn is_it(error: &crate::Error) -> bool {
        error.downcast_ref::<Self>().is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PixelAspect {
    /// Horizontal sample size: the width a coded pixel is drawn at, relative to
    /// [`PixelAspect::v`]. 1 and 1 together are square pixels, which is every
    /// file that states no aspect anywhere in its container.
    pub h: u32,
    /// The vertical half of the same ratio; zero means the container said
    /// nothing usable and the pixels are square.
    pub v: u32,
}

impl PixelAspect {
    /// The square pixels everything before anamorphic sources was.
    pub const SQUARE: Self = Self { h: 1, v: 1 };

    /// The widest display raster this engine will stretch a coded frame into.
    /// A container can claim any ratio at all, but no real one needs more
    /// than a 16k canvas's worth of columns, and the bound is what keeps a
    /// header claim from sizing an allocation no machine can satisfy.
    const MAX_DISPLAY_WIDTH: u32 = 65_535;

    /// The largest term a *reduced* aspect ratio may keep. A real display
    /// ratio is a small pair of integers however it was spelled -- 4:3, 16:9,
    /// 8:5, 12:11 -- and ffmpeg's unreduced DAR spellings reduce to one.
    /// A pair whose reduction still carries a term past this is not a ratio
    /// at all but four chosen bytes, and it is refused as one.
    const MAX_ASPECT_TERM: u32 = 1024;

    /// The ratio a container stated, or [`None`] when it stated one this
    /// engine cannot draw. Both halves arrive from header bytes -- read
    /// straight out of an mp4 `pasp` box, or computed from a Matroska
    /// `Display*` pair beside the coded size -- so the test lives here, once,
    /// rather than in each door that reads one.
    ///
    /// Unusable means: a zero half (nothing to divide by), or a *reduced*
    /// pair carrying a term past [`Self::MAX_ASPECT_TERM`] (a pair of chosen
    /// bytes rather than a ratio). Judged reduced, so an unreduced spelling of
    /// a real one -- ffmpeg writes 17280/12960 for 16:9 -- is judged by the
    /// picture it names and not by how many digits it took.
    pub fn stated(h: u32, v: u32) -> Option<Self> {
        if h == 0 || v == 0 {
            return None;
        }
        let (mut a, mut b) = (h, v);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        (h / a <= Self::MAX_ASPECT_TERM && v / a <= Self::MAX_ASPECT_TERM).then_some(Self { h, v })
    }

    /// The ratio a Matroska `DisplayUnit 0` `Display*` pair names, or [`None`]
    /// when the pair is not a picture.
    ///
    /// This is deliberately *not* [`Self::stated`], and the reason is what a
    /// unit-0 pair reduces to. The pair is a pixel SIZE, so reducing it
    /// against the coded size carries the sizes, not terms: 1920x1080 over
    /// 1440x1080 reduces to 4:3, but a 1920x803 scope -- one of the two most
    /// common letterbox ratios there are -- reduces to 1440:803, and a
    /// 1920x1037 flat to 1440:1037. A term bound tight enough to catch chosen
    /// bytes refuses exactly the pictures this arm exists to show.
    ///
    /// Only the *width* half is taken, because width is the only axis an
    /// aspect moves in this engine: `VideoMeta::height` is the coded height
    /// for every file, by design and by its own documentation. So the pair
    /// states the display width and the height is left where it is -- the same
    /// answer a 4:3 SAR over 1440x1080 gives, 1920x1080.
    ///
    /// What needs bounding is the raster, and [`Self::width_factor`] bounds
    /// that with [`Self::MAX_DISPLAY_WIDTH`] on every path. So this has only
    /// to refuse the degenerate claims: a zero half.
    ///
    /// # A named design gap, not an oversight
    ///
    /// The stated display *height* is read, checked, and then dropped. That is
    /// correct against this engine's model today -- `VideoMeta::height` is the
    /// coded height for every file, every door builds the display pair as
    /// `width_factor(coded_width)` against the coded height, and every consumer
    /// (`decode.rs`, `export.rs`, `hw.rs`'s `pixel_aspect_h`/`_v` beside
    /// `coded_width`/`coded_height`) reads the aspect as width-only.
    ///
    /// It becomes a real defect the moment any path starts honouring a stated
    /// display height: a 1920x803 pair would then have to drive
    /// `VideoMeta::height`, and this function would be silently feeding it
    /// `coded.0` instead. So the drop is recorded here rather than left as a
    /// line that looks like an oversight, and whoever makes that change should
    /// start by reading this paragraph -- the height half is being discarded
    /// deliberately, and the reason it was safe to discard is exactly the thing
    /// that change would remove.
    pub fn stated_size(display_width: u64, display_height: u64, coded: (u32, u32)) -> Option<Self> {
        // The height is checked for the degenerate claim and then goes unused:
        // a zero-height pair is a header that lies, and one this engine reads
        // as stating nothing. A real height cannot reach an allocation on this
        // path, because `VideoMeta::height` is the *coded* height, which both
        // doors already bound against [`crate::MAX_RESOLUTION`].
        if display_width == 0 || display_height == 0 || coded.0 == 0 {
            return None;
        }
        Some(Self {
            h: display_width.min(u64::from(u32::MAX)) as u32,
            v: coded.0,
        })
    }

    /// The ratio a Matroska `DisplayUnit 3` display *aspect ratio* names against
    /// the coded frame, or [`None`] when the ratio is degenerate.
    ///
    /// Deliberately not [`Self::stated`], and for the same reason as
    /// [`Self::stated_size`]: the SAR is the DAR brought onto the coded frame,
    /// which multiplies by the coded size, so what the result reduces to
    /// carries *sizes* rather than terms. A 239:100 scope -- 2.39:1, one of the
    /// most common letterbox ratios there are -- derives 2151:1600 over a
    /// 1920x1080 coded frame and the same over 3840x2160, both past any term
    /// bound, while over a 1440x1080 frame it derives 717:400 and passes. The
    /// same ratio judged differently by the frame it happens to sit on is a
    /// bound measuring the wrong thing.
    ///
    /// The pair itself is the engine's own model, unchanged: the DAR against
    /// the coded frame, `dar_w * coded_h : dar_h * coded_w`, which is what
    /// [`Self::width_factor`] has always multiplied the coded width by. So this
    /// returns it unreduced and lets `width_factor` do the arithmetic and apply
    /// [`Self::MAX_DISPLAY_WIDTH`] to the raster it produces -- a bound on the
    /// picture, rather than on the digits of a derivation.
    pub fn stated_ratio(dar_width: u64, dar_height: u64, coded: (u32, u32)) -> Option<Self> {
        if dar_width == 0 || dar_height == 0 || coded.0 == 0 || coded.1 == 0 {
            return None;
        }
        Some(Self {
            h: dar_width
                .saturating_mul(u64::from(coded.1))
                .min(u64::from(u32::MAX)) as u32,
            v: dar_height
                .saturating_mul(u64::from(coded.0))
                .min(u64::from(u32::MAX)) as u32,
        })
    }

    /// Whether the displayed picture is the coded one, drawn one to one.
    ///
    /// Both halves of the ratio are untrusted bytes -- four of them in an mp4
    /// `pasp` box, and a product of two header values on the Matroska door --
    /// so a half that is zero is a claim this engine cannot divide by, and it
    /// means square rather than a division by zero: the file states nothing
    /// usable, which is what every file that states no aspect at all states.
    pub fn is_square(self) -> bool {
        self.h == 0 || self.v == 0 || self.h == self.v
    }

    /// How many coded columns one displayed column is worth: the factor the
    /// width is stretched by, exactly as swscale takes it. 1 for square.
    ///
    /// Rounded to the even grid 4:2:0 composes on, so a ratio that is not a
    /// whole number of columns still lands on a picture the chroma planes
    /// cover -- the same rounding swscale applies, and the same one that keeps
    /// the stretch a whole-sample operation rather than a smear.
    pub fn width_factor(self, coded_width: u32) -> u32 {
        if self.is_square() {
            return coded_width;
        }
        // (coded * h / v), rounded up to even: 1440 at 4:3 gives 1920 exactly,
        // and a ratio that does not divide evenly is widened to the nearest
        // even column count rather than shown cropped or squeezed.
        //
        // This is the one function every door reaches, so it is also where the
        // arithmetic is made total -- a ratio may be *read* here ([`Self::stated`]),
        // *built* here from a `Display*` pixel pair ([`Self::stated_size`]), or
        // both, and this cannot tell which. So it bounds the one thing all
        // three agree on: the raster. `coded * h` cannot overflow a u64, but
        // the quotient is then *narrowed* to a u32 and can wrap -- 1920 *
        // 0x00FF_FFFF narrows to 2_147_481_728 -- and a narrowed zero becomes a
        // zero-column raster. Anything outside [`Self::MAX_DISPLAY_WIDTH`], in
        // either direction, is drawn at the coded size instead: the picture is
        // shown slightly wrong, which is what every unrenderable header
        // deserves, rather than aborting the decode worker on a capacity
        // overflow before a pixel is drawn.
        //
        // Judging the *ratio* instead would be the wrong test here: a
        // `DisplayUnit 0` pair is a pixel size, so its reduced form carries
        // the sizes (a 1920x803 scope reduces to 1440:803) and no term bound
        // can be both tight enough to catch chosen bytes and loose enough to
        // admit the two most common letterbox ratios. The raster bound has no
        // such conflict.
        let wide = u64::from(coded_width) * u64::from(self.h);
        let scaled = wide.div_ceil(u64::from(self.v));
        // Down to the even grid, not up: `(x | 1) & !1` is what this has always
        // done, and it is what an odd quotient rounds to. A 239:100 scope over
        // a 3840x2160 coded frame scales to 5163 and has always displayed
        // 5162, not 5164 -- `next_multiple_of` would round the other way and
        // silently change every existing anamorphic picture by two columns.
        let rounded = scaled | 1;
        let rounded = rounded & !1;
        if rounded == 0 || rounded > u64::from(Self::MAX_DISPLAY_WIDTH) {
            return coded_width;
        }
        rounded as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoMeta {
    /// The displayed width: the coded one, swapped where the file's display
    /// matrix is a 90- or 270-degree turn (see [`Rotation`]) *and* stretched
    /// where the file's pixel aspect is not square (see [`PixelAspect`]) -- the
    /// size a player draws the picture at, which is what a timeline is cut in.
    /// The picture as stored is [`VideoMeta::coded_width`] by
    /// [`VideoMeta::coded_height`].
    pub width: u32,
    /// The displayed height; see [`VideoMeta::width`]. The aspect never touches
    /// it: a non-square pixel stretches the width, always -- 4:3 SAR over
    /// 1440x1080 displays 1920x1080, and the height is the coded one either way.
    pub height: u32,
    /// The size the samples are stored at, before the display-matrix turn and
    /// the pixel stretch: what a decoder hands back, what a copy carries, and
    /// the shape every container header of a *copied* file has to declare. The
    /// displayed pair above is derived; this one is read.
    pub coded_width: u32,
    /// The stored height; see [`VideoMeta::coded_width`].
    pub coded_height: u32,
    /// The sample aspect ratio the container states, square where it states
    /// none. Metadata about the pixels, like [`VideoMeta::rotation`] is about
    /// their orientation: the decode keeps the coded pixels and everything
    /// downstream is told the ratio, which is what applies it exactly once --
    /// at the conversion to the square-pixel raster.
    pub pixel_aspect: PixelAspect,
    pub frame_rate: f64,
    pub frame_count: u32,
    pub codec: Codec,
    /// What this stream's samples mean -- which matrix, which transfer curve,
    /// which range -- resolved off the container's tags, the bitstream's, and
    /// the resolution, in that order. See [`crate::colorspace`].
    pub color: ColorDescription,
    /// Whether [`VideoMeta::color`]'s matrix is the size rule's guess rather than
    /// something the file states: see [`ColorDescription::matrix_assumed`].
    pub matrix_assumed: bool,
    /// How far the picture must be turned to be seen right, off the container's
    /// display matrix: an mp4 `tkhd` matrix, or a Matroska `Projection` roll on
    /// a rectangular projection -- the same quarter turns read off either.
    /// [`Rotation::None`] for everything whose container states no turn, and for
    /// a projection that is not rectangular: on a spherical or cubemap one those
    /// same pose elements describe how the *sphere* is oriented, which is no turn
    /// of the picture, so the pose is deliberately not applied and the file
    /// paints as a flat frame -- what every player does with a 360 clip, and
    /// cheaper than a refusal that would cost a file that plays. A turn that is
    /// not a quarter turn -- a mirror, a shear, an angle in between -- is refused
    /// at the door ([`UnsupportedRotation`]) rather than shown wrong.
    pub rotation: Rotation,
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
        Demuxer::Mp4(d) => d.reader.duration_secs(),
        Demuxer::Ts(d) => d.container_secs(),
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
            // In file order, the same first-in-file audio track the audio path
            // picks, so the bitrate row and the audio row of one card name one
            // track -- see [`MediaBitrate::audio`], which says what that number is
            // not.
            let audio = d
                .reader
                .tracks()
                .iter()
                .filter_map(|t| t.stream)
                .find(|&stream| {
                    d.reader.streams().iter().any(|s| {
                        s.index == stream
                            && matches!(s.params.media, ec_core::MediaParameters::Audio(_))
                    })
                });
            let rate = |stream| bits_per_second(mp4_track_bytes(&d.reader, stream), secs);
            (rate(d.stream), audio.and_then(rate))
        }
        // A transport stream indexes nothing, so the packets are counted: one walk of the
        // file, the price the Matroska arm paid at open.
        Demuxer::Ts(d) => {
            let (video, audio) = d.stream_bytes();
            (
                video.and_then(|b| bits_per_second(b, secs)),
                audio.and_then(|b| bits_per_second(b, secs)),
            )
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
pub(crate) fn bits_per_second(bytes: u64, secs: f64) -> Option<u64> {
    (secs > 0.0)
        .then(|| (bytes as f64 * 8.0 / secs) as u64)
        .filter(|&bps| bps > 0)
}

/// How long the file states it plays, read from its header: Matroska's
/// `Info.Duration`, or the mp4 `mvhd`. `None` where the header states none.
fn stated_secs(demuxer: &Demuxer) -> Option<f64> {
    match demuxer {
        Demuxer::Mkv(d) => d.container_secs,
        Demuxer::Mp4(d) => d.reader.duration_secs(),
        Demuxer::Ts(d) => d.container_secs(),
    }
}

/// What a file is, beside the rates [`probe_bitrate`] gives: the container it
/// is wrapped in, how big it is, and what its picture is coded as. Every field
/// the file does not state is `None`, the way [`MediaBitrate`] leaves out a rate
/// it cannot measure.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaFacts {
    /// The container as a person names it: `mp4`, `mov`, `m4a`, `m4v`, `3gp`,
    /// `mkv`, `webm`. The header decides where it can; the extension does the rest.
    pub container: Option<String>,
    /// The file's size on disk in bytes.
    pub bytes: Option<u64>,
    /// The picture track. `None` for a file with none: sound alone, a still, or
    /// a file that will not open.
    pub video: Option<VideoFacts>,
    /// How long the file plays: for a picture, its playable frames over its
    /// rate (the length the timeline gives it); the header's length where the
    /// picture has none to count (mp4 `mvhd`, Matroska `Info.Duration`); for
    /// sound alone the audio header's length. `None` for a still, or a file
    /// whose header states none.
    pub duration_secs: Option<f64>,
    /// Every video, audio and subtitle stream of the file, in file order within
    /// each kind. Empty where the header is not one this reads (a plain WAV or
    /// MP3 gets its audio rows from [`crate::AudioSession::probe_streams`], with
    /// no codec identifier). Kept beside the fields above, which are unchanged.
    pub streams: Vec<StreamFacts>,
    /// The file carries cover art: an ID3v2 `APIC` frame, a FLAC `PICTURE`
    /// block, an mp4 `covr` atom, or a Matroska attachment of an `image/*` type.
    /// A cover is never a playable stream: it is in no entry of `streams` and is
    /// not `video`, so a song with a cover stays audio-only.
    pub attached_pic: bool,
}

/// The playing time an mp4 or Matroska header states for audio stream `stream` (its index among
/// the audio streams). `None` where the header states none, or the file is neither container.
pub(crate) fn header_audio_secs(path: &Path, stream: usize) -> Option<f64> {
    header_streams(path)
        .ok()
        .flatten()?
        .into_iter()
        .find(|s| s.kind == StreamKind::Audio && s.index == stream)?
        .duration_secs
        .filter(|&secs| secs > 0.0)
}

/// The kind of a stream [`StreamFacts`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
}

/// One stream of a file as its header names it.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamFacts {
    pub kind: StreamKind,
    /// Position among the file's streams of this kind. For audio in a Matroska
    /// file this is the numbering [`crate::AudioSession::probe_streams`] hands
    /// out (the track that plays first is 0), so it is the `stream` the audio
    /// API takes; everywhere else it is file order.
    pub index: usize,
    /// The codec identifier as the file spells it: an mp4 sample entry's fourcc
    /// (`avc1`, `hvc1`, `mp4a`, `Opus`, `fLaC`, `tx3g`, `wvtt`), or a Matroska
    /// CodecID (`V_VP9`, `A_OPUS`, `S_TEXT/UTF8`). `None` where the header gives
    /// none.
    pub codec_id: Option<String>,
    /// The stream's language as the header states it, the way ffprobe reads it:
    /// an mp4 track's `elng` (an extended tag, when the file has one) and
    /// otherwise its `mdhd` three-letter code, which is `und` where the file
    /// says none; a Matroska audio or video track's `Language`, `und` where it
    /// states none. `None` where the header states none, which is every subtitle
    /// track of a Matroska file.
    pub language: Option<String>,
    /// Audio only: the channel count the header states. An mp4 sample entry's
    /// `channelcount`, or an AC-3 / E-AC-3 `dac3` / `dec3` box's `acmod` and
    /// `lfeon` (the sample entry's own count is 2 for most AC-3 files whatever
    /// they carry); a Matroska `Channels`. `None` where the header states none.
    pub channels: Option<u16>,
    /// Audio only: the sample rate in Hz, from an mp4 sample entry or a
    /// Matroska `SamplingFrequency`. `None` where the header states none (a
    /// rate above 65535 Hz is stored as 0 in an mp4 entry, and reads as none).
    pub sample_rate: Option<u32>,
    /// The stream's own header length in seconds: the mp4 `mdhd` duration over
    /// its timescale, which is the stream's length and not the playable one.
    /// Matroska has no per-track length element, so every track of a Matroska
    /// file reports the file's `Info.Duration` (`Duration` times `TimestampScale`)
    /// and the same value for each of them. `None` where the header states none.
    pub duration_secs: Option<f64>,
    /// Audio only, and AAC only: the profile the header states, spelled the way
    /// ffprobe spells it in its `profile` field: `LC`, `Main`, `SSR`, `LTP`,
    /// `HE-AAC` (SBR), `HE-AACv2` (SBR and parametric stereo), `LD`, `ELD`.
    /// Read from an mp4 `esds` AudioSpecificConfig, a Matroska `A_AAC`
    /// `CodecPrivate` (the same config), or else an `A_AAC/MPEG4/...` CodecID
    /// that names it. Explicit signalling is read off the config: an object
    /// type of 5 or 29, or the backward-compatible sync extensions (SBR `0x2b7`,
    /// PS `0x548`) after an AOT 2 config. A plain AAC-LC config carries SBR or PS
    /// only implicitly, in its bitstream, so the first few packets are decoded to
    /// find them: SBR gives `HE-AAC`, SBR with PS gives `HE-AACv2`, and neither
    /// leaves `LC`. `None` for every other codec, and for an AAC header that
    /// states no profile.
    pub profile: Option<String>,
    /// Audio only, and lossless only: the sample depth the stored stream
    /// states, the way ffprobe reports it. FLAC's STREAMINFO `bits_per_sample`
    /// (read from an mp4 `dfLa`, a Matroska `A_FLAC` `CodecPrivate`, or a bare
    /// `.flac` file), ALAC's ALACSpecificConfig `bitDepth` (an mp4 `alac` box or
    /// a Matroska `A_ALAC` `CodecPrivate`), and PCM's bit depth (a bare WAV's
    /// `fmt ` chunk, a Matroska `BitDepth`). ffprobe's `bits_per_raw_sample` is
    /// the same figure for FLAC and ALAC; for PCM it is `bits_per_sample`, the
    /// one ffprobe fills. `None` for every lossy codec, and for a lossless stream
    /// whose header states no depth (a PCM mp4 entry is not read).
    pub bit_depth: Option<u32>,
}

/// The picture track of a file, as its header and first keyframe state it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoFacts {
    pub codec: Codec,
    /// The displayed size, as [`VideoMeta::width`] and [`VideoMeta::height`] give it.
    pub width: u32,
    pub height: u32,
    /// `None` where the container states no rate.
    pub frame_rate: Option<f64>,
    /// Bits per luma sample. `None` where nothing here reads it: an H.264
    /// stream's SPS is not parsed for its depth, and every other codec's is.
    pub bit_depth: Option<u8>,
    /// Whether the picture's colour matrix is the size rule's guess rather than
    /// something the file states (no VUI, sequence header, `colr` or Matroska
    /// `Colour` matrix): true exactly when [`VideoMeta::matrix_assumed`] is.
    /// Transfer and range are not covered: they fall back to fixed values.
    pub matrix_assumed: bool,
    /// The profile the stream states, spelled as ffprobe spells its `profile`
    /// field: H.264 from the SPS (`Baseline`, `Constrained Baseline`, `Main`,
    /// `Extended`, `High`, `High 10`, `High 4:2:2`, `High 4:4:4 Predictive`, and
    /// the intra variants), HEVC from the `hvcC` (`Main`, `Main 10`, `Main Still
    /// Picture`, `Rext`), AV1 from the sequence header (`Main`, `High`,
    /// `Professional`), VP9 from the frame header (`Profile 0` to `Profile 3`).
    /// `None` where the stream states none of those.
    pub profile: Option<&'static str>,
    /// `Some("tff")` or `Some("bff")` for an interlaced H.264 stream: the field
    /// that displays first. The stream's own flags decide it, the container's
    /// `fiel` / `FieldOrder` is the fallback. `None` for a progressive stream and
    /// for every other codec.
    pub interlaced: Option<&'static str>,
}

/// How long the picture plays: its frame count over its rate. `None` where the
/// container states no rate or the picture has no frames.
fn picture_secs(meta: &VideoMeta) -> Option<f64> {
    (meta.frame_rate > 0.0 && meta.frame_count > 0)
        .then(|| f64::from(meta.frame_count) / meta.frame_rate)
}

/// One audio track as [`header_streams`] lists it: codec identifier, language,
/// channel count, sample rate and AAC profile, each where the header states it.
type AudioHeader = (
    String,
    Option<String>,
    Option<u16>,
    Option<u32>,
    Option<&'static str>,
    Option<u32>,
);

/// Every stream of an mp4-family or Matroska file, from its header alone: the
/// codec identifier each one carries and the length its own header states.
/// `None` for a file neither container reads (a WAV or MP3 has no header
/// listing its tracks); `Err` only for an mp4 or Matroska header that will not
/// walk.
fn header_streams(path: &Path) -> crate::Result<Option<Vec<StreamFacts>>> {
    if is_matroska(path) {
        let mut file = File::open(path)?;
        let end = file.metadata()?.len();
        let segment = mkv_segment(&mut file, end)?;
        let mut codecs = Vec::new();
        // Every Matroska track gets the file's length: see `StreamFacts::duration_secs`.
        let mut length = None;
        let mut at = segment.0;
        while let Some((id, body, stop)) = ebml_element(&mut file, at, segment.1)? {
            match id {
                CLUSTER => break,
                TRACKS => codecs = mkv_track_codecs(&mut file, body, stop)?,
                INFO => length = mkv_info_secs(&mut file, body, stop)?,
                _ => {}
            }
            at = stop;
        }
        // Sound is numbered the way the audio API numbers it, so `index` is a
        // stream `AudioSession::open_multi_streams` takes. A picture the decoder
        // refuses by name (a display matrix that is not a quarter
        // turn) fails that track walk too, and must not cost the whole list: the
        // sound is then listed in file order, which is the numbering of a file
        // with one audio track anyway.
        // (codec, language, channels, sample rate) per audio track, in that order.
        let audio: Vec<AudioHeader> = match matroska_audio_tracks(path) {
            Ok(tracks) => crate::audio::mkv_stream_order(path, &tracks)
                .into_iter()
                .map(|at| {
                    let t = &tracks[at];
                    let profile = mkv_aac_profile(&t.codec, &t.config);
                    // A plain AOT 2 config may carry an implicit SBR or PS payload:
                    // the track's first blocks say which (only where it has a config).
                    let profile = match profile {
                        Some("LC") if !t.config.is_empty() => {
                            let units: Vec<Vec<u8>> = crate::audio::first_packets(
                                path,
                                u32::try_from(t.number).unwrap_or(u32::MAX),
                                IMPLICIT_PROFILE_UNITS,
                            );
                            Some(implicit_aac_profile(&t.config, &units))
                        }
                        other => other,
                    };
                    let bit_depth = mkv_lossless_depth(&t.codec, &t.config, t.bit_depth);
                    (
                        t.codec.clone(),
                        Some(t.language.clone()),
                        t.channels,
                        t.sample_rate,
                        profile,
                        bit_depth,
                    )
                })
                .collect(),
            Err(_) => codecs
                .iter()
                .filter(|(kind, _, _)| *kind == 2)
                .map(|(_, codec, _)| {
                    (
                        codec.clone(),
                        None,
                        None,
                        None,
                        mkv_aac_profile(codec, &[]),
                        None,
                    )
                })
                .collect(),
        };
        let mut out: Vec<StreamFacts> = audio
            .into_iter()
            .enumerate()
            .map(
                |(index, (codec, language, channels, sample_rate, profile, bit_depth))| {
                    StreamFacts {
                        kind: StreamKind::Audio,
                        index,
                        codec_id: Some(codec),
                        language,
                        channels,
                        sample_rate,
                        duration_secs: length,
                        profile: profile.map(str::to_owned),
                        bit_depth,
                    }
                },
            )
            .collect();
        let (mut video, mut subtitle) = (0, 0);
        for (kind, codec, language) in codecs {
            let (kind, index) = match kind {
                1 => {
                    video += 1;
                    (StreamKind::Video, video - 1)
                }
                0x11 => {
                    subtitle += 1;
                    (StreamKind::Subtitle, subtitle - 1)
                }
                _ => continue,
            };
            out.push(StreamFacts {
                language: matches!(kind, StreamKind::Video).then_some(language),
                kind,
                index,
                codec_id: Some(codec).filter(|c| !c.is_empty()),
                channels: None,
                sample_rate: None,
                duration_secs: length,
                profile: None,
                bit_depth: None,
            });
        }
        return Ok(Some(out));
    }
    if ps::is_mpeg_system(path) {
        return ts::header_streams(path).map(Some);
    }
    let Some(moov) = read_top_level(path, b"moov")? else {
        return Ok(None);
    };
    let (mut video, mut audio, mut subtitle) = (0, 0, 0);
    let mut out = Vec::new();
    for (_, trak) in boxes(&moov).filter(|(kind, _)| *kind == b"trak") {
        let Some(mdia) = child(trak, b"mdia") else {
            continue;
        };
        // The handler is a FullBox (4) then pre_defined (4), then the type.
        let handler = child(mdia, b"hdlr").and_then(|h| h.get(8..12));
        let (kind, slot) = match handler {
            Some(b"vide") => (StreamKind::Video, &mut video),
            Some(b"soun") => (StreamKind::Audio, &mut audio),
            Some(b"sbtl" | b"text" | b"subt") => (StreamKind::Subtitle, &mut subtitle),
            _ => continue,
        };
        // The first sample entry's fourcc, out of the same `stsd` the picture
        // reader reads by hand: mp4 0.14 keeps no name for a kind it drops.
        let entry = child(mdia, b"minf")
            .and_then(|b| child(b, b"stbl"))
            .and_then(|b| child(b, b"stsd"))
            .and_then(|stsd| boxes(stsd.get(8..).unwrap_or_default()).next());
        let codec_id = entry.map(|(fourcc, _)| String::from_utf8_lossy(fourcc).into_owned());
        let (channels, sample_rate) = match (kind, entry) {
            (StreamKind::Audio, Some((fourcc, payload))) => sound_entry_layout(fourcc, payload),
            _ => (None, None),
        };
        let bit_depth = match (kind, entry) {
            (StreamKind::Audio, Some((fourcc, payload))) => mp4_lossless_depth(fourcc, payload),
            _ => None,
        };
        // An `mp4a` entry's `esds` (after the 28-byte sample entry header) states
        // the AAC profile in its AudioSpecificConfig.
        let profile = match entry {
            Some((b"mp4a", payload)) => child(payload.get(28..).unwrap_or_default(), b"esds")
                .and_then(esds_config)
                .and_then(aac_profile_of_config)
                .map(str::to_owned),
            _ => None,
        };
        out.push(StreamFacts {
            kind,
            index: *slot,
            codec_id,
            language: mp4_language(mdia),
            channels,
            sample_rate,
            duration_secs: child(mdia, b"mdhd").and_then(mdhd_secs),
            profile,
            bit_depth,
        });
        *slot += 1;
    }
    refine_mp4_profiles(path, &mut out);
    Ok(Some(out))
}

/// Looks at the first samples of each mp4 audio track whose config states plain
/// AAC-LC, to see whether an implicit SBR or PS payload makes it HE-AAC or
/// HE-AACv2 (see [`implicit_aac_profile`]). The k-th audio stream here is the
/// k-th audio track of `ec-mp4`, both being in file order. Errors leave the
/// profile as the config states it.
fn refine_mp4_profiles(path: &Path, streams: &mut [StreamFacts]) {
    if !streams
        .iter()
        .any(|s| s.kind == StreamKind::Audio && s.profile.as_deref() == Some("LC"))
    {
        return;
    }
    let Ok(mut reader) = crate::mp4track::open(path) else {
        return;
    };
    let tracks = crate::mp4track::audio_tracks(&reader);
    for (facts, track) in streams
        .iter_mut()
        .filter(|s| s.kind == StreamKind::Audio)
        .zip(tracks)
    {
        let (Some(stream), true) = (track.1, facts.profile.as_deref() == Some("LC")) else {
            continue;
        };
        let Some(config) = crate::mp4track::extradata(&reader, stream) else {
            continue;
        };
        let count = reader.samples(stream).len().min(IMPLICIT_PROFILE_UNITS);
        let units: Vec<Vec<u8>> = (0..count)
            .filter_map(|i| reader.read_sample(stream, i).ok())
            .map(|packet| packet.data.to_vec())
            .collect();
        facts.profile = Some(implicit_aac_profile(&config, &units).to_owned());
    }
}

/// The AAC profile, in ffprobe's spelling, that an AudioSpecificConfig states.
/// Explicit signalling only, read from the header: an object type of 5 (HE-AAC)
/// or 29 (HE-AACv2), or a plain AOT 2 config whose backward-compatible sync
/// extensions (`0x2b7` for SBR, `0x548` for PS) name SBR and PS. A plain AOT 2
/// config with no sync extension reads as `LC`; an implicit SBR or PS lives only
/// in the bitstream, and [`implicit_aac_profile`] decodes the first frames for it.
/// `None` for a config `ec_aac` will not read or an object type not listed.
fn aac_profile_of_config(config: &[u8]) -> Option<&'static str> {
    let cfg = ec_aac::parse_audio_specific_config(config).ok()?;
    let (sbr, ps) = match *config.first()? >> 3 {
        5 | 29 => (true, cfg.ps_present),
        2 => sync_extension_flags(config)?,
        1 => return Some("Main"),
        3 => return Some("SSR"),
        4 => return Some("LTP"),
        23 => return Some("LD"),
        39 => return Some("ELD"),
        _ => return None,
    };
    Some(match (sbr, ps) {
        (_, true) => "HE-AACv2",
        (true, false) => "HE-AAC",
        _ => "LC",
    })
}

/// A bit reader over an AudioSpecificConfig, for the few fields past what
/// `ec_aac` returns: its sync extensions do not say whether PS is signalled.
#[derive(Clone, Copy)]
struct ConfigBits<'a> {
    data: &'a [u8],
    at: usize,
}

impl ConfigBits<'_> {
    fn bits(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.at / 8)?;
            v = (v << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Some(v)
    }

    fn left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.at)
    }

    /// Consumes the next `n` bits if they are the sync word `want`.
    fn sync(&mut self, n: usize, want: u32) -> bool {
        let mut look = *self;
        if self.left() >= n && look.bits(n) == Some(want) {
            *self = look;
            true
        } else {
            false
        }
    }
}

/// Whether an AOT 2 config's backward-compatible sync extensions (ISO/IEC
/// 14496-3 1.6.5.2) signal SBR and PS: `(sbr, ps)`. `None` for a config that
/// does not parse as one. A config with a program config element, whose
/// layout the extension walk does not follow, reads as neither.
fn sync_extension_flags(config: &[u8]) -> Option<(bool, bool)> {
    let mut r = ConfigBits {
        data: config,
        at: 0,
    };
    r.bits(5)?; // audioObjectType 2
    if r.bits(4)? == 15 {
        r.bits(24)?; // an explicit sampling frequency
    }
    if r.bits(4)? == 0 {
        return Some((false, false));
    }
    // GASpecificConfig: frame_length_flag, then the core delay and extension flag.
    r.bits(1)?;
    if r.bits(1)? == 1 {
        r.bits(14)?;
    }
    r.bits(1)?;
    if !r.sync(11, 0x2b7) || r.bits(5)? != 5 {
        return Some((false, false));
    }
    if r.bits(1)? == 0 {
        return Some((false, false));
    }
    if r.bits(4)? == 15 {
        r.bits(24)?;
    }
    let ps = r.sync(11, 0x548) && r.bits(1)? == 1;
    Some((true, ps))
}

/// The profile an AOT 2 (`LC`) stream states once its first access units are
/// decoded: an SBR payload makes it `HE-AAC`, an SBR payload with a PS one
/// `HE-AACv2`, and neither leaves it `LC`. Only the first few units are read, so
/// the cost is bounded however long the stream is. A unit that fails to decode
/// ends the look, and what was seen before it stands.
fn implicit_aac_profile(config: &[u8], units: &[Vec<u8>]) -> &'static str {
    let Ok(mut decoder) = ec_aac::AacDecoder::with_config_bytes(config) else {
        return "LC";
    };
    for unit in units.iter().take(IMPLICIT_PROFILE_UNITS) {
        if decoder.decode(unit, None).is_err() || decoder.ps_implicit() {
            break;
        }
    }
    match (decoder.sbr_implicit(), decoder.ps_implicit()) {
        (_, true) => "HE-AACv2",
        (true, false) => "HE-AAC",
        _ => "LC",
    }
}

/// How many access units [`implicit_aac_profile`] reads: an SBR payload shows in
/// the first frame that carries one, and PS rides with it.
const IMPLICIT_PROFILE_UNITS: usize = 3;

/// The sample depth a FLAC STREAMINFO states: `bits_per_sample` is five bits
/// across bytes 12 and 13 of the 34-byte block (after the 10 bytes of block
/// sizes, frame sizes, sample rate and channel count), stored minus one.
fn flac_streaminfo_depth(info: &[u8]) -> Option<u32> {
    let b = info.get(12..14)?;
    Some(u32::from((((b[0] & 1) << 4) | (b[1] >> 4)) + 1))
}

/// The depth of a FLAC stream from its metadata blocks: a bare `fLaC` file's
/// blocks, an mp4 `dfLa` body past its FullBox header, or a Matroska `A_FLAC`
/// `CodecPrivate` (with or without the marker). STREAMINFO is always the first
/// block, so anything else first reads as `None`.
fn flac_depth_of(blocks: &[u8]) -> Option<u32> {
    let blocks = blocks.strip_prefix(b"fLaC").unwrap_or(blocks);
    let (&head, rest) = blocks.split_first()?;
    if head & 0x7f != 0 {
        return None;
    }
    let len = rest
        .get(..3)?
        .iter()
        .fold(0usize, |n, &b| (n << 8) | usize::from(b));
    flac_streaminfo_depth(rest.get(3..3 + len)?)
}

/// The bit depth of an ALACSpecificConfig: byte 5 of its 24 bytes (after the
/// frame length and the version bytes). An mp4 `alac` box carries it behind a
/// four-byte FullBox header, which is taken off here.
fn alac_depth(config: &[u8]) -> Option<u32> {
    let cfg = match config.len() {
        28 => &config[4..],
        _ => config,
    };
    (cfg.len() == 24)
        .then(|| u32::from(cfg[5]))
        .filter(|d| *d > 0)
}

/// The depth a Matroska lossless track states: a FLAC or ALAC config, or the
/// `BitDepth` of a PCM track. `None` for anything lossy or float.
fn mkv_lossless_depth(codec: &str, private: &[u8], pcm_bits: Option<u32>) -> Option<u32> {
    match codec {
        "A_FLAC" => flac_depth_of(private),
        "A_ALAC" => alac_depth(private),
        c if c.starts_with("A_PCM/INT/") => pcm_bits,
        _ => None,
    }
}

/// The depth an mp4 audio sample entry states, from the codec-specific box
/// behind its 28-byte header: `dfLa` for FLAC, `alac` for ALAC.
fn mp4_lossless_depth(fourcc: &[u8; 4], payload: &[u8]) -> Option<u32> {
    let body = payload.get(28..).unwrap_or_default();
    match fourcc {
        b"fLaC" => child(body, b"dfLa").and_then(|d| flac_depth_of(d.get(4..)?)),
        b"alac" => child(body, b"alac").and_then(alac_depth),
        _ => None,
    }
}

/// The bit depth of a bare FLAC or WAV file, read from its first 4 KiB: the
/// STREAMINFO of a `fLaC` file, or a PCM `fmt ` chunk's `wBitsPerSample`. A
/// float or compressed WAV, and every other file, reads as `None`.
fn bare_bit_depth(path: &Path) -> Option<u32> {
    let mut head = [0u8; 4096];
    let read = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let head = &head[..read];
    if head.starts_with(b"fLaC") {
        return flac_depth_of(head);
    }
    if head.get(..4) != Some(b"RIFF") || head.get(8..12) != Some(b"WAVE") {
        return None;
    }
    let mut at = 12;
    while let Some(id) = head.get(at..at + 4) {
        let size = head
            .get(at + 4..at + 8)?
            .try_into()
            .map(u32::from_le_bytes)
            .ok()? as usize;
        let body = head.get(at + 8..at + 8 + size);
        if id == b"fmt " {
            let fmt = body?;
            let tag = u16::from_le_bytes(fmt.get(..2)?.try_into().ok()?);
            // Plain PCM, or WAVE_FORMAT_EXTENSIBLE whose sub-format GUID is PCM.
            let pcm = tag == 1 || (tag == 0xFFFE && matches!(fmt.get(24..26), Some([1, 0])));
            let bits = u16::from_le_bytes(fmt.get(14..16)?.try_into().ok()?);
            return (pcm && bits > 0).then_some(u32::from(bits));
        }
        at += 8 + size + (size & 1);
    }
    None
}

/// The AAC profile a Matroska `A_AAC` track states: its `CodecPrivate` config
/// when it has one (ffmpeg writes one), else the `CodecID` spelling
/// `A_AAC/MPEG{2,4}/{MAIN,LC,SSR,LTP}[/SBR]`, where `/SBR` is the explicit SBR
/// signalling.
fn mkv_aac_profile(codec: &str, private: &[u8]) -> Option<&'static str> {
    if !codec.starts_with("A_AAC") {
        return None;
    }
    aac_profile_of_config(private).or_else(|| {
        let mut parts = codec.strip_prefix("A_AAC/")?.split('/');
        if !matches!(parts.next()?, "MPEG2" | "MPEG4") {
            return None;
        }
        let kind = parts.next()?;
        let ext: Vec<&str> = parts.collect();
        Some(match kind {
            "MAIN" => "Main",
            "SSR" => "SSR",
            "LTP" => "LTP",
            "LC" if ext.contains(&"SBR") => "HE-AAC",
            "LC" => "LC",
            _ => return None,
        })
    })
}

/// One MPEG-4 descriptor off the front of `buf`: its tag, its body and what
/// follows it. The length is the 1-to-4-byte base-128 field every descriptor
/// carries.
fn descriptor(buf: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *buf.first()?;
    let mut at = 1;
    let mut len = 0usize;
    for _ in 0..4 {
        let b = *buf.get(at)?;
        at += 1;
        len = (len << 7) | usize::from(b & 0x7f);
        if b & 0x80 == 0 {
            break;
        }
    }
    let end = at.checked_add(len)?;
    Some((tag, buf.get(at..end)?, buf.get(end..)?))
}

/// The AudioSpecificConfig an `esds` box carries: the DecoderSpecificInfo
/// (tag 5) inside the DecoderConfigDescriptor (tag 4) inside the
/// ES_Descriptor (tag 3). `esds` is a FullBox, so its descriptors start after
/// four bytes of version and flags.
fn esds_config(esds: &[u8]) -> Option<&[u8]> {
    let (tag, es, _) = descriptor(esds.get(4..)?)?;
    if tag != 3 {
        return None;
    }
    // ES_ID (2), then the flags byte, then the optional fields those flags name.
    let flags = *es.get(2)?;
    let mut rest = es.get(3..)?;
    if flags & 0x80 != 0 {
        rest = rest.get(2..)?;
    }
    if flags & 0x40 != 0 {
        rest = rest.get(1 + usize::from(*rest.first()?)..)?;
    }
    if flags & 0x20 != 0 {
        rest = rest.get(2..)?;
    }
    let (tag, dec, _) = descriptor(rest)?;
    if tag != 4 {
        return None;
    }
    // objectTypeIndication, streamType, bufferSizeDB, maxBitrate, avgBitrate: 13 bytes.
    let mut sub = dec.get(13..)?;
    while let Some((tag, body, after)) = descriptor(sub) {
        if tag == 5 {
            return Some(body);
        }
        sub = after;
    }
    None
}

/// The channel count and sample rate an mp4 audio sample entry states, from its
/// payload (the bytes after the entry's own box header). The audio entry lays
/// out 6 reserved bytes, the data reference index, 8 bytes of version and
/// vendor, then `channelcount` at 16 and the 16.16 `samplerate` at 24. An AC-3
/// or E-AC-3 entry's count is replaced by what its `dac3` / `dec3` box says.
fn sound_entry_layout(fourcc: &[u8; 4], payload: &[u8]) -> (Option<u16>, Option<u32>) {
    let channels = payload
        .get(16..18)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .filter(|c| *c > 0);
    let rate = payload
        .get(24..28)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) >> 16)
        .filter(|r| *r > 0);
    // The AudioSampleEntry header is 28 bytes; its child boxes follow it.
    let boxed = match fourcc {
        b"ac-3" => child(payload.get(28..).unwrap_or_default(), b"dac3").and_then(ac3_channels),
        b"ec-3" => child(payload.get(28..).unwrap_or_default(), b"dec3").and_then(eac3_channels),
        _ => None,
    };
    (boxed.or(channels), rate)
}

/// Channels from an AC-3 `dac3` payload: `acmod` (3 bits) and `lfeon` (1 bit)
/// follow fscod (2), bsid (5) and bsmod (3), so they sit at bits 10..13 and 13
/// of the 24-bit field.
fn ac3_channels(payload: &[u8]) -> Option<u16> {
    let bits = u32::from_be_bytes([0, *payload.first()?, *payload.get(1)?, *payload.get(2)?]);
    Some(acmod_channels((bits >> 11) as u8 & 0x7, (bits >> 10) as u8 & 0x1))
}

/// Channels from an E-AC-3 `dec3` payload: the first independent substream's
/// `acmod` and `lfeon` sit at bits 12..15 and 15 of the 24-bit field that
/// starts after the 2-byte data rate and substream count.
fn eac3_channels(payload: &[u8]) -> Option<u16> {
    let bits = u32::from_be_bytes([
        0,
        *payload.get(2)?,
        *payload.get(3)?,
        *payload.get(4)?,
    ]);
    Some(acmod_channels((bits >> 9) as u8 & 0x7, (bits >> 8) as u8 & 0x1))
}

/// Full-range channels for an `acmod` and `lfeon` (ETSI TS 102 366, table 4.3).
fn acmod_channels(acmod: u8, lfe: u8) -> u16 {
    const MAIN: [u16; 8] = [2, 1, 2, 3, 3, 4, 4, 5];
    MAIN[usize::from(acmod & 0x7)] + u16::from(lfe & 1)
}

/// A track's language as ffprobe reads it: the `elng` extended tag when the
/// track has one, otherwise the `mdhd` three-letter code. `None` only where
/// neither box is there to read.
fn mp4_language(mdia: &[u8]) -> Option<String> {
    // `elng` is a FullBox (4 bytes of version and flags) and then a NUL-ended
    // tag such as `fr` or `fr-CA`; an empty one says nothing.
    if let Some(tag) = child(mdia, b"elng").and_then(|e| e.get(4..)) {
        let text = tag.split(|b| *b == 0).next().unwrap_or_default();
        if !text.is_empty() {
            return Some(String::from_utf8_lossy(text).into_owned());
        }
    }
    let mdhd = child(mdia, b"mdhd")?;
    // The packed ISO-639-2 code follows the timestamps: 16 bytes into a version 0
    // box, 28 into a version 1 one (which has 64-bit times).
    let at = match mdhd.first()? {
        1 => 28,
        _ => 16,
    };
    let packed = u16::from_be_bytes(mdhd.get(at + 4..at + 6)?.try_into().ok()?);
    Some(unpack_language(packed))
}

/// Three 5-bit letters, each offset by `0x60`, packed into 15 bits. A code that
/// is not three letters `a`-`z` says no language: a zero code (none written,
/// `0x0000` decodes to backticks) and the `0x7fff` some muxers write (decodes to
/// DELs) are both `und`, the spec's own word for it. This is the convention the
/// Matroska path uses too ([`mkv_language`]), so an untagged track reads the
/// same in either container.
fn unpack_language(packed: u16) -> String {
    let code: String = [10, 5, 0]
        .into_iter()
        .map(|shift| char::from(((packed >> shift) & 0x1f) as u8 + 0x60))
        .collect();
    if code.bytes().all(|b| b.is_ascii_lowercase()) {
        code
    } else {
        "und".to_string()
    }
}

/// The length an `mdhd` box states, in seconds: its duration over its timescale.
/// Version 0 packs both as 32-bit fields, version 1 as 64-bit ones.
fn mdhd_secs(payload: &[u8]) -> Option<f64> {
    let (timescale, duration) = match payload.first()? {
        1 => (
            u32::from_be_bytes(payload.get(20..24)?.try_into().ok()?),
            u64::from_be_bytes(payload.get(24..32)?.try_into().ok()?),
        ),
        _ => (
            u32::from_be_bytes(payload.get(12..16)?.try_into().ok()?),
            // All ones is the spec's "unknown", not a length.
            match u32::from_be_bytes(payload.get(16..20)?.try_into().ok()?) {
                u32::MAX => return None,
                d => u64::from(d),
            },
        ),
    };
    (timescale > 0 && duration > 0).then(|| duration as f64 / f64::from(timescale))
}

/// The `DiscardPadding` of a Matroska audio track's last block, in nanoseconds:
/// the trailing samples its muxer says a decoder drops. Vorbis in Matroska has
/// no page granule to cut its end by, and its last block overhangs the stream
/// by exactly this (ffmpeg writes it as the last packet's discard padding, and
/// its decode ends there). `None` for anything that is not Matroska or a track
/// with no block in it; `Some(0)` where the last block states no padding.
pub(crate) fn matroska_tail_padding_ns(path: &Path, number: u64) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let end = file.metadata().ok()?.len();
    let segment = mkv_segment(&mut file, end).ok()?;
    let padding = mkv_last_discard(&mut file, segment, number).ok()??;
    Some(u64::try_from(padding).unwrap_or(0))
}

/// Every block of track `number` in a Matroska file, in storage order, as
/// `(ts, at, len)`: its timestamp in `TimestampScale` ticks (the value a
/// packet's pts carries out of the reader), and the file offset and length of
/// its bytes. Cached per file the way the audio index is ([`mkv_blocks`]).
///
/// `None` for anything that is not Matroska, a track with no blocks, or a track
/// whose blocks are not stored as their own bytes (header stripping, zlib), since
/// a caller reading `at..at+len` would then read something else.
pub(crate) fn matroska_track_blocks(path: &Path, number: u64) -> Option<Vec<(i64, u64, usize)>> {
    let mut file = File::open(path).ok()?;
    let end = file.metadata().ok()?.len();
    let segment = mkv_segment(&mut file, end).ok()?;
    let plain = mkv_tracks(&mut file, segment)
        .ok()?
        .1
        .iter()
        .any(|t| t.number == number && t.unpack == Unpack::None);
    if !plain {
        return None;
    }
    let (blocks, _, _) = mkv_blocks(path, &mut file, segment, number).ok()?;
    (!blocks.is_empty()).then(|| blocks.iter().map(|b| (b.ts, b.at, b.len)).collect())
}

/// The `DiscardPadding` (nanoseconds, zero where none) of the last block of track
/// `number` in `segment`, by a header walk of every cluster: blocks are skipped,
/// not read. `None` when the track has no block.
fn mkv_last_discard(
    file: &mut File,
    segment: (u64, u64),
    number: u64,
) -> crate::Result<Option<i64>> {
    let mut last: Option<i64> = None;
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(file, at, segment.1)? {
        at = stop;
        if id != CLUSTER {
            continue;
        }
        let mut child = body;
        while let Some((cid, cbody, cstop)) = ebml_element(file, child, stop)? {
            child = cstop;
            let (block, padding) = match cid {
                SIMPLE_BLOCK => (Some(mkv_block(file, cbody, cstop)?), 0),
                BLOCK_GROUP => {
                    let (mut block, mut padding) = (None, 0i64);
                    let mut group = cbody;
                    while let Some((gid, gbody, gstop)) = ebml_element(file, group, cstop)? {
                        group = gstop;
                        match gid {
                            BLOCK => block = Some(mkv_block(file, gbody, gstop)?),
                            DISCARD_PADDING => {
                                padding = ebml_sint(&ebml_bytes(file, gbody, gstop)?)
                            }
                            _ => {}
                        }
                    }
                    (block, padding)
                }
                _ => continue,
            };
            if block.is_some_and(|b| b.number == number) {
                last = Some(padding);
            }
        }
    }
    Ok(last)
}

/// An EBML signed integer: big-endian two's complement in as many bytes as it
/// takes, sign-extended.
fn ebml_sint(bytes: &[u8]) -> i64 {
    let negative = bytes.first().is_some_and(|b| b & 0x80 != 0);
    bytes
        .iter()
        .take(8)
        .fold(if negative { -1 } else { 0 }, |v, &b| {
            (v << 8) | i64::from(b)
        })
}

/// The length `Info` states, in seconds: `Duration` in `TimestampScale` ticks
/// (the spec's 1 ms default where the scale is absent). `None` where there is
/// no `Duration` or it is zero. Reads the header fields only, so it is not
/// refused by a picture the playback path will not take.
fn mkv_info_secs(file: &mut File, body: u64, stop: u64) -> crate::Result<Option<f64>> {
    let (mut scale, mut duration) = (1_000_000u64, None);
    let mut at = body;
    while let Some(e) = ebml_element(file, at, stop)? {
        match e.0 {
            TIMESTAMP_SCALE => scale = ebml_uint(file, e.1, e.2)?.max(1),
            DURATION => duration = Some(ebml_float(file, e.1, e.2)?),
            _ => {}
        }
        at = e.2;
    }
    Ok(duration
        .filter(|d| *d > 0.0)
        .map(|d| d * scale as f64 / 1e9))
}

/// The length a Matroska file's `Info` states, in seconds, from its header alone.
/// `None` for a file that is not Matroska, or whose header states none. This is
/// the figure ffprobe reports as the container's duration.
fn mkv_stated_secs(path: &Path) -> Option<f64> {
    if !is_matroska(path) {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let end = file.metadata().ok()?.len();
    let segment = mkv_segment(&mut file, end).ok()?;
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(&mut file, at, segment.1).ok()? {
        match id {
            CLUSTER => break,
            INFO => return mkv_info_secs(&mut file, body, stop).ok().flatten(),
            _ => {}
        }
        at = stop;
    }
    None
}

/// The playable length of an Ogg Opus stream, per RFC 7845 section 4: the last
/// page's granule position minus the `OpusHead` pre-skip, over 48 kHz (Opus
/// granules always count at 48 kHz, whatever the input rate). The pre-skip is
/// priming no player plays, and it is what the decode path drops
/// ([`crate::audio`]), so the timeline ends on the last sample. ffprobe's
/// `format=duration` does not subtract it (3.0065 s on `test_tone.opus`, where
/// the granule is 144312 and the pre-skip 312), and that is the one figure this
/// does not match. `None` for anything else, or a stream whose last page
/// finishes no packet.
fn ogg_opus_secs(path: &Path) -> Option<f64> {
    const OPUS_RATE: f64 = 48_000.0;
    // Pages are at most a little over 64 KiB, so the last one starts in this tail.
    const TAIL: u64 = 1 << 17;
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut head = [0u8; 512];
    let read = file.read(&mut head).ok()?;
    let head = &head[..read];
    if !head.starts_with(b"OggS") {
        return None;
    }
    // The `OpusHead` packet: version (1), channels (1), then the pre-skip as a
    // little-endian u16 at byte 10 of the packet.
    let opus_head = head.windows(8).position(|w| w == b"OpusHead")?;
    let pre_skip = head
        .get(opus_head + 10..opus_head + 12)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))?;
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    // A page header is 27 bytes plus its segment table; the capture pattern, the
    // version byte (0) and the granule (-1 when no packet ends on the page) are
    // what is read. Searched back from the end, so the last page wins.
    let mut at = tail.len().checked_sub(14)?;
    loop {
        if tail[at..].starts_with(b"OggS") && tail[at + 4] == 0 {
            let granule = i64::from_le_bytes(tail[at + 6..at + 14].try_into().ok()?);
            if granule >= 0 {
                let playable = (granule as u64).saturating_sub(u64::from(pre_skip));
                return Some(playable as f64 / OPUS_RATE);
            }
        }
        at = at.checked_sub(1)?;
    }
}

/// The `(TrackType, CodecID)` of every `TrackEntry` in a Matroska `Tracks`
/// element, in file order. Header only.
fn mkv_track_codecs(file: &mut File, body: u64, stop: u64) -> crate::Result<Vec<(u64, String, String)>> {
    let mut out = Vec::new();
    let mut at = body;
    while let Some((id, entry, entry_stop)) = ebml_element(file, at, stop)? {
        if id == TRACK_ENTRY {
            let (mut kind, mut codec) = (0, String::new());
            let (mut language, mut bcp47) = (String::new(), String::new());
            let mut inner = entry;
            while let Some((field, fbody, fstop)) = ebml_element(file, inner, entry_stop)? {
                match field {
                    TRACK_TYPE => kind = ebml_uint(file, fbody, fstop)?,
                    CODEC_ID => codec = string_of(file, fbody, fstop)?,
                    TRACK_LANGUAGE => language = string_of(file, fbody, fstop)?,
                    TRACK_LANGUAGE_BCP47 => bcp47 = string_of(file, fbody, fstop)?,
                    _ => {}
                }
                inner = fstop;
            }
            out.push((kind, codec, mkv_language(&language, &bcp47)));
        }
        at = entry_stop;
    }
    Ok(out)
}

/// What `path` is, from its header alone -- one [`Demuxer::open`] and no decoder,
/// the same cost [`probe_bitrate`] pays, so it belongs off the render thread.
pub fn probe_media(path: &Path) -> MediaFacts {
    let bytes = std::fs::metadata(path).ok().map(|m| m.len());
    let container = container_name(path);
    // A still has no playing time of its own. Sound has no picture to describe,
    // and its length is the audio header's, which `AudioSession` reads cheaply.
    if crate::is_image(path) {
        return MediaFacts {
            container,
            bytes,
            video: None,
            duration_secs: None,
            streams: Vec::new(),
            attached_pic: false,
        };
    }
    let attached_pic = has_cover_art(path);
    if crate::is_audio(path) {
        // A length the container states wins over one counted from the decoder:
        // ffprobe reports the Matroska `Info` duration, and an Ogg Opus granule
        // (both read from the header alone). An ADTS stream states none, so its
        // length is its frame count (1024 samples per frame), which is what
        // [`crate::AudioSession`] counts.
        let stated = mkv_stated_secs(path).or_else(|| ogg_opus_secs(path));
        let streams = header_streams(path).ok().flatten().unwrap_or_default();
        let streams = match streams.is_empty() {
            true => crate::AudioSession::probe_streams(path)
                .unwrap_or_default()
                .into_iter()
                .map(|s| StreamFacts {
                    kind: StreamKind::Audio,
                    index: s.index,
                    codec_id: Some(audio_codec_id(path, &s.codec)),
                    language: s.lang.clone(),
                    channels: Some(s.channels).filter(|c| *c > 0),
                    sample_rate: Some(s.sample_rate).filter(|r| *r > 0),
                    duration_secs: stated.or_else(|| {
                        crate::AudioSession::stream_duration_secs(path, s.index)
                            .ok()
                            .flatten()
                    }),
                    profile: None,
                    bit_depth: bare_bit_depth(path),
                })
                .collect(),
            false => streams,
        };
        return MediaFacts {
            container,
            bytes,
            video: None,
            duration_secs: stated
                .or_else(|| crate::AudioSession::duration_secs(path).ok().flatten()),
            streams,
            attached_pic,
        };
    }
    // Every stream the header names, and each one's own codec identifier and
    // length. A header that will not walk leaves the list empty rather than
    // taking the rest of the facts down with it.
    let streams = match header_streams(path) {
        Ok(Some(streams)) => streams,
        _ => Vec::new(),
    };
    let (video, duration_secs) = match Demuxer::open(path) {
        Ok((meta, mut demuxer)) => (
            Some(VideoFacts {
                codec: meta.codec,
                width: meta.width,
                height: meta.height,
                frame_rate: (meta.frame_rate > 0.0).then_some(meta.frame_rate),
                // An 8-bit H.264 stream states no depth of its own worth showing,
                // so it is not answered with the 8 the demuxer falls back to; a
                // High 10 one is.
                bit_depth: (meta.codec != Codec::H264 || demuxer.bit_depth() > 8)
                    .then(|| demuxer.bit_depth()),
                matrix_assumed: meta.matrix_assumed,
                profile: demuxer.profile(),
                interlaced: crate::deinterlace::field_order(&mut demuxer, path),
            }),
            // The playable picture's length, which is what the timeline lays
            // a video source out at ([`PlaybackSession::file_frames`]). The
            // header's length can be a longer audio track's (a 2 s clip with a
            // 60 s commentary); that one is reported beside it, not as this.
            picture_secs(&meta).or_else(|| stated_secs(&demuxer)),
        ),
        // No picture track (sound alone, or a file that will not open as video):
        // the audio header's length is the only one there is.
        Err(_) => (
            None,
            crate::AudioSession::duration_secs(path).ok().flatten(),
        ),
    };
    MediaFacts {
        container,
        bytes,
        video,
        duration_secs,
        streams,
        attached_pic,
    }
}

/// Whether the file carries cover art, read from its header by what the bytes
/// open with rather than the extension: an ID3v2 tag (`APIC`, or `PIC` in v2.2),
/// a FLAC metadata block of type 6 (`PICTURE`), a Matroska attachment whose
/// MIME type is `image/*`, or otherwise an mp4-family `moov` with an
/// `udta/meta/ilst/covr` atom. Any read that fails answers `false`.
fn has_cover_art(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut head = [0u8; 4];
    if file.read_exact(&mut head).is_err() {
        return false;
    }
    if head.starts_with(b"ID3") {
        return id3_has_cover(path);
    }
    if &head == b"fLaC" {
        return flac_has_picture(&mut file);
    }
    if is_matroska(path) {
        return mkv_has_picture_attachment(&mut file).unwrap_or(false);
    }
    read_top_level(path, b"moov")
        .ok()
        .flatten()
        .is_some_and(|moov| mp4_has_covr(&moov))
}

/// The ID3v2 tag at the start of `path`, walked frame by frame for a picture.
/// The tag is read whole (its size is in its header) up to a 64 MiB cap, which
/// is past any real tag; a larger one is read as having no cover it can see.
fn id3_has_cover(path: &Path) -> bool {
    const MAX_TAG: usize = 64 << 20;
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut head = [0u8; 10];
    if file.read_exact(&mut head).is_err() {
        return false;
    }
    let version = head[3];
    let size = synchsafe(&head[6..10]);
    if size > MAX_TAG {
        return false;
    }
    // A tag cut short by the file's end still names the frames it holds. The
    // capacity is what the file can back, not what the header claims.
    let left = file.metadata().map_or(0, |m| m.len() as usize);
    let mut tag = Vec::with_capacity(size.min(left));
    if file.take(size as u64).read_to_end(&mut tag).is_err() {
        return false;
    }
    let (id_len, size_len) = match version {
        2 => (3, 3),
        _ => (4, 4),
    };
    let mut at = 0;
    if head[5] & 0x40 != 0 {
        // An extended header: v2.3 gives its size plainly, v2.4 synchsafe and
        // inclusive of its own four bytes.
        let Some(ext) = tag.get(..4) else {
            return false;
        };
        let ext = match version {
            4 => synchsafe(ext),
            _ => be_u32(ext) as usize + 4,
        };
        at = ext;
    }
    while let Some(frame) = tag.get(at..) {
        let Some(id) = frame.get(..id_len) else {
            break;
        };
        // Padding (a zero byte where a frame ID would be) ends the frames.
        if id[0] == 0 {
            break;
        }
        if id == b"APIC" || id == b"PIC" {
            return true;
        }
        let Some(len_bytes) = frame.get(id_len..id_len + size_len) else {
            break;
        };
        let body = match version {
            4 => synchsafe(len_bytes),
            _ => be_u32(len_bytes) as usize,
        };
        // The frame header: ID and size, then v2.3 and v2.4's two flag bytes.
        let header = if version == 2 { 6 } else { 10 };
        at += header + body;
    }
    false
}

/// Whether a FLAC stream's metadata blocks include a `PICTURE` (type 6). The
/// blocks are skipped by seeking, so a large picture is never read.
fn flac_has_picture(file: &mut File) -> bool {
    // Past the `fLaC` marker.
    if file.seek(SeekFrom::Start(4)).is_err() {
        return false;
    }
    loop {
        let mut header = [0u8; 4];
        if file.read_exact(&mut header).is_err() {
            return false;
        }
        let last = header[0] & 0x80 != 0;
        if header[0] & 0x7f == 6 {
            return true;
        }
        let len = i64::from(u32::from_be_bytes([0, header[1], header[2], header[3]]));
        if last || file.seek(SeekFrom::Current(len)).is_err() {
            return false;
        }
    }
}

/// Whether a Matroska file has an `Attachments` element naming an `image/*`
/// file. Every child of the segment is visited, clusters skipped by their size,
/// because a muxer may write the attachments after the clusters.
fn mkv_has_picture_attachment(file: &mut File) -> crate::Result<bool> {
    const ATTACHMENTS: u32 = 0x1941_A469;
    const ATTACHED_FILE: u32 = 0x61A7;
    const FILE_MIME_TYPE: u32 = 0x4660;
    let end = file.metadata()?.len();
    let segment = mkv_segment(file, end)?;
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(file, at, segment.1)? {
        if id == ATTACHMENTS {
            let mut inner = body;
            while let Some((attached, fbody, fstop)) = ebml_element(file, inner, stop)? {
                if attached == ATTACHED_FILE {
                    let mut field = fbody;
                    while let Some((f, mbody, mstop)) = ebml_element(file, field, fstop)? {
                        if f == FILE_MIME_TYPE
                            && string_of(file, mbody, mstop)?.starts_with("image/")
                        {
                            return Ok(true);
                        }
                        field = mstop;
                    }
                }
                inner = fstop;
            }
        }
        at = stop;
    }
    Ok(false)
}

/// Whether an mp4 `moov` holds a `udta/meta/ilst/covr` atom. `meta` is a FullBox,
/// so its four version and flag bytes come before its children.
fn mp4_has_covr(moov: &[u8]) -> bool {
    child(moov, b"udta")
        .and_then(|udta| child(udta, b"meta"))
        .and_then(|meta| meta.get(4..))
        .and_then(|meta| child(meta, b"ilst"))
        .and_then(|ilst| child(ilst, b"covr"))
        .is_some()
}

/// An ID3v2 synchsafe integer: four bytes of seven bits each.
fn synchsafe(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0usize, |acc, b| acc << 7 | usize::from(b & 0x7f))
}

/// A big-endian `u32` from up to four bytes, missing bytes read as zero.
fn be_u32(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .take(4)
        .fold(0u32, |acc, b| acc << 8 | u32::from(*b))
}

/// The codec identifier of a sound-only file: what its header says when the
/// header says it (`fLaC`, an MPEG audio frame, an Ogg `OpusHead` or
/// `\x01vorbis`), and otherwise the name the audio reader gave the stream.
fn audio_codec_id(path: &Path, probed: &str) -> String {
    // The reader's own name wins whenever it has one; the header is read only to
    // name a stream the reader could not (`an unsupported format`).
    if !probed.starts_with("an ") && probed != "unknown" {
        return probed.to_string();
    }
    let mut head = [0u8; 512];
    let read = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let head = &head[..read];
    let sniffed = if head.starts_with(b"fLaC") {
        Some("flac")
    } else if head.starts_with(b"OggS") {
        if head.windows(8).any(|w| w == b"OpusHead") {
            Some("opus")
        } else if head.windows(7).any(|w| w == b"\x01vorbis") {
            Some("vorbis")
        } else {
            None
        }
    } else if mpeg_audio_at(head) {
        Some("mp3")
    } else {
        None
    };
    sniffed.map_or_else(|| probed.to_string(), str::to_string)
}

/// Whether the bytes open with an MPEG audio frame: an ID3v2 tag (whose size
/// is skipped) followed by a frame sync, or a frame sync at the very start.
/// Only the start is looked at, so PCM or other data that happens to hold the
/// sync bytes further in is not mistaken for MP3.
fn mpeg_audio_at(head: &[u8]) -> bool {
    let mut at = 0;
    if head.starts_with(b"ID3") && head.len() >= 10 {
        let size = head[6..10]
            .iter()
            .fold(0usize, |acc, b| acc << 7 | usize::from(b & 0x7f));
        at = 10 + size;
    }
    head.get(at..at + 2)
        .is_some_and(|w| w[0] == 0xff && w[1] & 0xe0 == 0xe0 && w[1] & 0x06 != 0)
}

/// Why a file's picture is refused, in the words the demuxer gives, or `None`
/// when the picture opens or there is no picture to refuse (a still, a song, a
/// sound-only container). [`probe_media`] lists the file's streams either way:
/// this is the one answer beside them that says what is wrong with the picture.
pub fn picture_refusal(path: &Path) -> Option<String> {
    if crate::is_image(path) || crate::is_audio(path) {
        return None;
    }
    match Demuxer::open(path) {
        Ok(_) => None,
        Err(e) if NoVideoTrack::is_it(&e) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// The container a file is wrapped in. Matroska's `DocType` tells `webm` from
/// the rest, and an mp4-family file's `ftyp` brand tells `mov` and `m4a` apart;
/// anything the header does not settle is the extension, lowercased.
fn container_name(path: &Path) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if is_matroska(path) {
        let mut head = [0u8; 256];
        let read = File::open(path)
            .and_then(|mut f| f.read(&mut head))
            .unwrap_or(0);
        // The header says Matroska, so the name is Matroska's, whatever the extension
        // says; only the audio-only Matroska extensions keep their own names.
        return Some(match matroska_doctype(&head[..read]) {
            Some("webm") => "webm".into(),
            _ => match ext.as_deref() {
                Some(e @ ("mka" | "mks" | "mk3d")) => e.into(),
                _ => "mkv".into(),
            },
        });
    }
    if avi::is_avi(path) {
        return Some("avi".into());
    }
    if dv::is_dv(path) {
        return Some("dv".into());
    }
    if ps::is_mpegps(path) {
        return Some(ps::container_name(path, ext.as_deref()));
    }
    if ts::is_mpegts(path) {
        // The camcorder's and the disc's names are kept; any other is a plain `mpegts`.
        return Some(match ext.as_deref() {
            Some("mts" | "m2ts") => ext.unwrap_or_default(),
            _ => "mpegts".into(),
        });
    }
    match read_top_level(path, b"ftyp") {
        Ok(Some(payload)) if payload.len() >= 4 => Some(container_from_brand(&payload[..4]).into()),
        _ => ext,
    }
}

/// The mp4-family name for an `ftyp` major brand. A brand this does not name is
/// plain mp4, which is what the rest of the family reads as.
fn container_from_brand(brand: &[u8]) -> &'static str {
    match brand {
        b"qt  " => "mov",
        b"M4A " => "m4a",
        b"M4V " | b"M4VH" | b"M4VP" => "m4v",
        brand if brand.starts_with(b"3g") => "3gp",
        _ => "mp4",
    }
}

/// The `DocType` string of an EBML header, read from the first bytes of a
/// file. Element id `0x4282`, and a one-byte size, which every DocType fits.
fn matroska_doctype(head: &[u8]) -> Option<&str> {
    let at = head.windows(2).position(|w| w == [0x42, 0x82])?;
    let size = *head.get(at + 2)?;
    if size & 0x80 == 0 {
        return None;
    }
    let len = usize::from(size & 0x7f);
    std::str::from_utf8(head.get(at + 3..at + 3 + len)?)
        .ok()
        .map(|s| s.trim_end_matches('\0'))
}

/// How many bytes of the file an mp4 track's samples are -- its share of the
/// `mdat`, which over the container's seconds is its share of the byte rate.
///
/// Not the container's declared bitrate, deliberately: an `esds` states what the
/// encoder declared rather than what the file spends, and a sample table divided
/// by the track's own duration is the arithmetic [`MediaBitrate`] exists to not
/// do. Bytes are what every reader agrees on, and the demuxer's sample index
/// holds all of them, fragmented files included.
fn mp4_track_bytes(reader: &ec_mp4::Mp4Demuxer<CancelableReader<BufReader<File>>>, stream: u32) -> u64 {
    reader
        .samples(stream)
        .iter()
        .map(|s| u64::from(s.size))
        .sum()
}

/// The file, whichever container it came in. Which one is decided by the
/// extension at [`Demuxer::open`] and never again: everything downstream --
/// playback, export, the plugin -- speaks access units and display frames.
pub enum Demuxer {
    Mp4(Box<Mp4Demuxer>),
    Mkv(Box<MkvDemuxer>),
    /// An AVI (`.avi`, `.divx`; [`avi`]), an MPEG transport stream (`.ts`, `.mts`, `.m2ts`, `.m2t`) or program stream (`.mpg`,
    /// `.vob`, `.m2v`, ...; [`ps`]), both read by [`ts::TsDemuxer`].
    Ts(Box<ts::TsDemuxer>),
}

impl Demuxer {
    /// The colour tags the file's own container states, unspecified fields left
    /// unspecified ([`Tags`]) -- for the writers that carry a source's samples
    /// as they are, and must not declare more than the source did.
    pub fn colour_tags(&self) -> Tags {
        match self {
            Self::Mp4(mp4) => mp4.colour_tags(),
            Self::Mkv(mkv) => mkv.colour_tags(),
            Self::Ts(ts) => ts.colour_tags(),
        }
    }

    pub fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
        let (mut meta, mut demuxer) = if is_matroska(path) {
            let (meta, mkv) = MkvDemuxer::open(path)?;
            (meta, Self::Mkv(Box::new(mkv)))
        } else if ps::is_mpeg_system(path) {
            let (meta, ts) = ts::TsDemuxer::open(path)?;
            (meta, Self::Ts(Box::new(ts)))
        } else {
            let (meta, mp4) = Mp4Demuxer::open(path)?;
            (meta, Self::Mp4(Box::new(mp4)))
        };
        demuxer.fill_sei_light();
        demuxer.fill_vp9_depth()?;
        demuxer.fill_vp8_profile();
        demuxer.fill_intra(&mut meta, path)?;
        Ok((meta, demuxer))
    }

    /// The HEVC `general_profile_idc` of the picture track, from the `hvcC` the
    /// container holds; `None` for any other codec. Read before the VA-API
    /// plugin is asked for anything, because the vendored decoder has no
    /// answer for the range-extensions profiles.
    pub fn hevc_profile(&self) -> Option<u8> {
        match self {
            Self::Mp4(d) => d.hevc_profile,
            Self::Mkv(d) => (d.codec == Codec::Hevc)
                .then(|| d.private.get(1).map(|b| b & 0x1f))
                .flatten(),
            Self::Ts(d) => d.hevc_profile,
        }
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
            Self::Ts(d) => d.light,
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
            Self::Ts(d) => (d.codec, d.light),
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
            Self::Ts(d) => d.light = found,
        }
    }

    /// The container's FourCC for an MPEG-4 Part 2 picture (`XVID`, `DIVX`, `DX50`...), where
    /// the container states one (an AVI): `ec-mpeg4` picks the encoder's bug workarounds by it.
    pub(crate) fn fourcc(&self) -> Option<[u8; 4]> {
        match self {
            Self::Ts(d) => d.fourcc,
            _ => None,
        }
    }

    /// The picture stream's own codec id, size and `BITMAPINFOHEADER` (an AVI's raw video), where
    /// the container states them: what MS-MPEG4 (which has no header of its own) and raw video
    /// are decoded from.
    pub(crate) fn video_params(&self) -> Option<(ec_core::CodecId, u32, u32, &[u8])> {
        match self {
            Self::Ts(d) => Some((d.codec_id, d.size.0, d.size.1, &d.header)),
            Self::Mp4(d) if d.codec == Codec::Raw => Some((d.codec_id, d.size.0, d.size.1, &d.header)),
            _ => None,
        }
    }

    /// What an intra-only track's (ProRes, DNxHD, MJPEG) first sample states: whether it is
    /// interlaced and the field order. Default (progressive) for every other codec.
    pub(crate) fn intra(&self) -> crate::decode::intra::IntraFacts {
        match self {
            Self::Mp4(d) => d.intra,
            Self::Mkv(d) => d.intra,
            Self::Ts(d) => d.intra,
        }
    }

    /// Reads a ProRes, DNxHD or MJPEG track's first sample (one decode) and fills in what no
    /// container states: bit depth, chroma format, the DNxHD profile, interlace and field
    /// order, and the colour the stream itself states (MJPEG is full range). The cursor is
    /// put back where it opened. A refusal by name (DNxHD alpha, 12-bit JPEG...) fails the
    /// open, as a refused H.264 profile does; a sample that merely fails to decode does not.
    fn fill_intra(&mut self, meta: &mut VideoMeta, path: &Path) -> crate::Result<()> {
        let codec = match self {
            Self::Mp4(d) => d.codec,
            Self::Mkv(d) => d.codec,
            // DV in an AVI or a raw `.dv`: the pictures state their own field order. (The
            // other intra-only codecs in an AVI keep their container-stated defaults.)
            Self::Ts(d) if matches!(d.codec, Codec::Dv | Codec::Raw) => d.codec,
            _ => return Ok(()),
        };
        if !matches!(codec, Codec::ProRes | Codec::Dnxhd | Codec::Mjpeg | Codec::Dv | Codec::Raw) {
            return Ok(());
        }
        let first = self.next_access_unit();
        self.seek_to_sync_at_or_before(0);
        let Ok(Some(sample)) = first else {
            return Ok(());
        };
        let container = crate::deinterlace::container_order(path);
        let Some(p) = crate::decode::intra::probe(codec, &sample, meta, container, self.video_params().map_or(&[][..], |p| p.3))? else {
            return Ok(());
        };
        let mut facts = p.facts;
        if codec == Codec::Mjpeg && facts.interlaced {
            facts.order = container.filter(|o| o.is_interlaced()).unwrap_or(ec_core::Interlace::TopFieldFirst);
        }
        let tags = self.colour_tags();
        meta.color = ColorDescription::resolve_sized(tags, p.tags, meta.width, meta.height);
        meta.matrix_assumed = ColorDescription::matrix_assumed(tags, p.tags);
        match self {
            Self::Mp4(d) => {
                d.bit_depth = p.bit_depth;
                d.chroma = Some(p.chroma);
                d.profile = d.profile.or(p.profile);
                d.intra = facts;
            }
            Self::Mkv(d) => {
                d.bit_depth = p.bit_depth;
                d.chroma = Some(p.chroma);
                d.profile = d.profile.or(p.profile);
                d.intra = facts;
            }
            Self::Ts(d) => {
                d.bit_depth = p.bit_depth;
                d.chroma = Some(p.chroma);
                d.profile = d.profile.or(p.profile);
                d.intra = facts;
            }
        }
        Ok(())
    }

    /// The profile of a VP8 track, from the frame tag of its first access unit
    /// (see [`video_profile::vp8`]). Nothing else in the container states it.
    fn fill_vp8_profile(&mut self) {
        let codec = match self {
            Self::Mp4(d) => d.codec,
            Self::Mkv(d) => d.codec,
            Self::Ts(d) => d.codec,
        };
        if codec != Codec::Vp8 {
            return;
        }
        let first = self.next_access_unit();
        self.seek_to_sync_at_or_before(0);
        let Ok(Some(au)) = first else {
            return;
        };
        let profile = video_profile::vp8(&au);
        match self {
            Self::Mp4(d) => d.profile = profile,
            Self::Mkv(d) => d.profile = profile,
            Self::Ts(d) => d.profile = profile,
        }
    }

    /// The bitstream tier of [`Self::bit_depth`] for VP9, which is *all* of it:
    /// an HEVC track states its depth in an `hvcC` and an AV1 one in an `av1C`,
    /// and a VP9 track states it nowhere either demuxer reads -- a Matroska
    /// `TrackEntry` carries no configuration record at all, and the `vpcC` an mp4
    /// writes is optional and describes the file rather than the frame. So it is
    /// read where it is always true: the uncompressed header of the first
    /// keyframe, which is what [`vp9_keyframe`] parses.
    ///
    /// Assuming 8 instead is how a profile 2 stream would decode into an NV12
    /// pool and come back as garbage -- the plugin picks its surface pool off
    /// this number ([`crate::hw`]).
    fn fill_vp9_depth(&mut self) -> crate::Result<()> {
        let codec = match self {
            Self::Mp4(d) => d.codec,
            Self::Mkv(d) => d.codec,
            Self::Ts(d) => d.codec,
        };
        if codec != Codec::Vp9 {
            return Ok(());
        }
        let first = self.next_access_unit();
        self.seek_to_sync_at_or_before(0);
        let Ok(Some(au)) = first else {
            return Ok(());
        };
        let Some((depth, chroma)) = vp9_keyframe(&au)? else {
            return Ok(());
        };
        let profile = video_profile::vp9(&au);
        match self {
            Self::Mp4(d) => {
                d.bit_depth = depth;
                d.chroma = chroma;
                d.profile = profile;
            }
            Self::Mkv(d) => {
                d.bit_depth = depth;
                d.chroma = chroma;
                d.profile = profile;
            }
            Self::Ts(d) => {
                d.bit_depth = depth;
                d.chroma = chroma;
                d.profile = profile;
            }
        }
        Ok(())
    }

    /// Next access unit in decode order, `None` at end of track.
    pub fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
        match self {
            Self::Mp4(d) => d.next_access_unit(),
            Self::Mkv(d) => d.next_access_unit(),
            Self::Ts(d) => d.next_access_unit(),
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
            Self::Ts(d) => d.seek_to_sync_at_or_before(frame),
        }
    }

    /// Whether the stream is soft telecine (it asks for 3:2 field repeats), so that
    /// its pictures are the 23.976p film frames and the rate states them as that.
    /// Read at open from a transport or program stream's first pictures.
    pub fn soft_pulldown(&self) -> bool {
        matches!(self, Self::Ts(d) if d.soft_pulldown)
    }

    /// The file this demuxer reads, where it kept the name.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Mp4(d) => Some(&d.path),
            Self::Mkv(d) => Some(&d.path),
            Self::Ts(d) => Some(d.path()),
        }
    }

    /// Whether this is MPEG-1/2 video.
    pub(crate) fn is_mpeg2(&self) -> bool {
        matches!(self, Self::Ts(d) if d.codec == Codec::Mpeg2)
    }

    /// Whether this is an H.264 stream whose SPS says it may carry interlaced
    /// pictures (`frame_mbs_only_flag` 0: field pictures or MBAFF). The decode
    /// layer deinterlaces exactly these, and keeps them off the GPU, whose
    /// output order and field handling this engine does not control.
    pub fn h264_interlaced(&self) -> bool {
        let sets = match self {
            Self::Mp4(d) if d.codec == Codec::H264 => &d.parameter_sets,
            Self::Mkv(d) if d.codec == Codec::H264 => &d.config,
            Self::Ts(d) if d.codec == Codec::H264 => &d.config,
            // MPEG-2 video states it in the sequence extension, per sequence; the
            // pictures then say for themselves which of them are interlaced.
            Self::Ts(d) if d.codec == Codec::Mpeg2 => return !crate::colorspace::mpeg2_progressive(&d.config),
            _ => return false,
        };
        video_profile::h264_interlaced(sets)
    }

    /// The profile the stream states, in ffprobe's spelling; see
    /// [`VideoFacts::profile`]. Read at open.
    pub fn profile(&self) -> Option<&'static str> {
        match self {
            Self::Mp4(d) => d.profile,
            Self::Mkv(d) => d.profile,
            Self::Ts(d) => d.profile,
        }
    }

    /// Bits per luma sample the stream is coded at: 8 for everything but an
    /// HEVC Main 10 or a 10-bit AV1 track, which decode into a P010 surface
    /// rather than an NV12 one. Not part of [`VideoMeta`] because the read-back
    /// is 8-bit either way -- the two readers are the plugin's surface pool and
    /// the software AV1 seat's narrowing ([`crate::decode::narrow_av1`]), and
    /// both consume the depth and hand out bytes.
    pub fn bit_depth(&self) -> u8 {
        match self {
            Self::Mp4(d) => d.bit_depth,
            Self::Mkv(d) => d.bit_depth,
            Self::Ts(d) => d.bit_depth,
        }
    }

    /// Whether an HEVC stream's VPS, SPS and PPS all parse under `ec-h265-syntax`,
    /// and at least one SPS does. Read out of the container's `hvcC` or
    /// `CodecPrivate`; a stream that carries none there is read from its first
    /// access unit instead, the cursor put back where it opened. `true` for any
    /// other codec, which this does not look at.
    ///
    /// Parameter sets only, no picture: a stream whose sets the spec forbids
    /// (`sps_max_sub_layers_minus1 = 7`, say) is refused before any decoder is
    /// chosen, because the hardware driver will tolerate what the parser will not.
    pub fn hevc_parameter_sets_parse(&mut self) -> bool {
        let (codec, mut sets) = match self {
            Self::Mp4(d) => (d.codec, d.parameter_sets.clone()),
            Self::Mkv(d) => (d.codec, d.config.clone()),
            Self::Ts(d) => (d.codec, d.config.clone()),
        };
        if codec != Codec::Hevc {
            return true;
        }
        if sets.is_empty() {
            let first = self.next_access_unit();
            self.seek_to_sync_at_or_before(0);
            match first {
                Ok(Some(au)) => sets = au,
                _ => return false,
            }
        }
        let mut sps_parses = false;
        for nal in ec_h265_syntax::split_annex_b(&sets) {
            let rbsp = nal.rbsp();
            let parses = match nal.header.nal_type {
                ec_h265_syntax::NalUnitType::Vps => ec_h265_syntax::Vps::parse(&rbsp).is_ok(),
                ec_h265_syntax::NalUnitType::Sps => {
                    let ok = ec_h265_syntax::Sps::parse(&rbsp).is_ok();
                    sps_parses |= ok;
                    ok
                }
                ec_h265_syntax::NalUnitType::Pps => ec_h265_syntax::Pps::parse(&rbsp).is_ok(),
                _ => true,
            };
            if !parses {
                return false;
            }
        }
        sps_parses
    }

    /// The chroma format of the picture track, read off the parameter sets for
    /// H.264, HEVC and AV1, fixed at 4:2:0 for VP8 and read off the first
    /// keyframe for VP9. `None` where none of those says anything -- a VP9 track
    /// with no keyframe to read, or an H.264 or HEVC one whose sets are missing.
    pub fn chroma(&self) -> Option<Chroma> {
        match self {
            Self::Mp4(d) => d.chroma,
            Self::Mkv(d) => d.chroma,
            Self::Ts(d) => d.chroma,
        }
    }

    /// The scan order of the two fields of an interlaced MJPEG picture, where the container
    /// states one: an AVI does ([`ec_riff::AviDemuxer::field_order`]; ffmpeg's convention,
    /// bottom field first, for every MJPEG stream). For `MjpegDecoder::set_field_order`.
    pub fn field_order(&self) -> Option<ec_core::frame::Interlace> {
        match self {
            Self::Ts(d) => d.field_order(),
            _ => None,
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
    // The bytes say what the container is; the extension only answers when the
    // header does not (an unreadable or too-short file, which then fails at open).
    match sniff_container(path) {
        Some(matroska) => matroska,
        None => path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "mkv" | "mka" | "mks" | "mk3d" | "webm"
            )
        }),
    }
}

/// `Some(true)` for an EBML header (Matroska/WebM), `Some(false)` for an ISO
/// base media `ftyp` box (mp4 family), `None` when neither magic is there.
fn sniff_container(path: &Path) -> Option<bool> {
    let mut head = [0u8; 12];
    let read = File::open(path).and_then(|mut f| f.read(&mut head)).ok()?;
    let head = &head[..read];
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        Some(true)
    } else if head.len() >= 8 && &head[4..8] == b"ftyp" {
        Some(false)
    } else {
        None
    }
}

pub struct Mp4Demuxer {
    reader: ec_mp4::Mp4Demuxer<CancelableReader<BufReader<File>>>,
    /// The picture track's stream index in `reader`.
    stream: u32,
    /// The chroma format the stream states; see [`Demuxer::chroma`].
    chroma: Option<Chroma>,
    /// The video track's `colr` as the file wrote it ([`colr_tags`]), unspecified
    /// fields left unspecified.
    colour: Tags,
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
    /// `general_profile_idc` of an HEVC track; `None` for every other codec.
    hevc_profile: Option<u8>,
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
    /// The stated profile; see [`Demuxer::profile`].
    profile: Option<&'static str>,
    /// How bright the track says it gets; see [`Demuxer::light`].
    light: ContentLight,
    /// What an intra-only track's first sample states; see [`Demuxer::intra`].
    intra: crate::decode::intra::IntraFacts,
    /// A raw video track's synthesized header (`ec_riff::MOV_MARKER`); empty for every other codec.
    header: Vec<u8>,
    codec_id: ec_core::CodecId,
    size: (u32, u32),
    /// The file this reads, for [`Demuxer::path`].
    path: PathBuf,
}

impl Mp4Demuxer {
    /// The video track's `colr` tags as the file wrote them ([`Demuxer::colour_tags`]).
    pub fn colour_tags(&self) -> Tags {
        self.colour
    }

    fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
        refuse_asf(path)?;
        let file = File::open(path)?;
        let reader = ec_mp4::Mp4Demuxer::new(CancelableReader(BufReader::new(file)))
            .map_err(mp4_error)?;

        // The picture track: the first one in *file* order (`moov.traks`, which is
        // what the demuxer lists its tracks in) whose sample entry this engine
        // reads. A `HashMap` walk would make "the video track" of a file with two
        // a different one from one run to the next, and a saved `.edith` would
        // reopen on the other picture. The audio side picks the same way
        // ([`crate::audio::audio_track_ids`]).
        //
        // The fourcc of the sample entry names the codec. An `hvc1`-tagged HEVC
        // track -- what Apple and ffmpeg's mov muxer write in practice -- and an
        // AV1 track both come out of the sample table with their own fourcc, so
        // nothing needs a second parse to find them.
        let (info, stream, codec) = reader
            .tracks()
            .iter()
            .find_map(|t| {
                let stream = t.stream?;
                let codec = mp4_codec(&t.sample_entry)?;
                let video = reader
                    .streams()
                    .iter()
                    .find(|s| s.index == stream)
                    .is_some_and(|s| matches!(s.params.media, ec_core::MediaParameters::Video(_)));
                video.then_some((t.clone(), stream, codec))
            })
            // Named, for the same reason the Matroska door names it: "no video
            // track" is a lie about a file that has one in a codec this does not
            // read, and the fourcc is what tells a user which.
            .ok_or_else(|| mp4_no_video(reader.tracks()))?;
        let Some(ec_core::MediaParameters::Video(video)) = reader
            .streams()
            .iter()
            .find(|s| s.index == stream)
            .map(|s| s.params.media.clone())
        else {
            return Err(mp4_no_video(reader.tracks()));
        };

        let track_id = info.track_id;
        // The turn the file was shot at, off its `tkhd` display matrix -- how
        // every phone records "this is portrait". The coded size is the sample
        // entry's for H.264 and the `tkhd`'s for every other codec, which is what
        // the engine reads as the picture's size. The matrix is `ec-mp4`'s reading
        // of the track; an unsupported one is refused here, by name.
        let rotation = rotation_of_track(info.rotation)?;
        let (coded_w, coded_h) = (video.width, video.height);
        // The same bound the Matroska door applies, on the same values.
        for stated in [coded_w, coded_h] {
            if stated > crate::MAX_RESOLUTION {
                return Err(unsupported_display_size(u64::from(stated)));
            }
        }
        // The ratio one coded pixel is drawn at, off the sample entry's `pasp`
        // box: absent for the square-pixel file, which is the common case.
        let pixel_aspect = pasp_of(path, track_id);
        // The aspect widens the *coded* frame, then the turn swaps that
        // stretched pair the way it swaps the coded one (`pixel_aspect_of`'s
        // twin comment in the Matroska door carries the measured proof). The
        // SAR only ever widens a width, never a height.
        let (width, height) = match rotation.swaps_axes() {
            true => (coded_h, pixel_aspect.width_factor(coded_w)),
            false => (pixel_aspect.width_factor(coded_w), coded_h),
        };
        let frame_rate = video
            .frame_rate
            .map_or(0.0, |tb| tb.num() as f64 / tb.den() as f64);
        let meta = VideoMeta {
            width,
            height,
            coded_width: coded_w,
            coded_height: coded_h,
            pixel_aspect,
            frame_rate,
            frame_count: 0,
            codec,
            // Filled below, once the parameter sets the bitstream tier reads are
            // in hand.
            color: ColorDescription::default(),
            matrix_assumed: false,
            rotation,
        };
        // Where frame 0 is: the sample the edit list starts the presentation on.
        let first_sample = reader.first_frame_sample(stream);
        let mut parameter_sets = Vec::new();
        let mut nal_length = 4;
        let mut bit_depth = 8;
        let mut hevc_profile = None;
        let mut profile = None;
        let mut header = Vec::new();
        match codec {
            Codec::H264 => {
                let (_, entry) = sample_entry(path, track_id)?;
                let avcc = child(entry.get(78..).unwrap_or_default(), b"avcC")
                    .ok_or("an H.264 track without an avcC box")?;
                let (len, sets) = parse_avcc(avcc)?;
                nal_length = len;
                bit_depth = avc_bit_depth(&sets)?;
                parameter_sets = sets;
                profile = video_profile::h264(&parameter_sets);
            }
            // The parameter sets an HEVC decoder cannot start without are read
            // out of `hvcC` by hand, as the one record that carries them.
            Codec::Hevc => {
                let hvcc = hvcc_record(path, track_id)?;
                let (len, sets, depth) = parse_hvcc(&hvcc)?;
                nal_length = len;
                parameter_sets = sets;
                bit_depth = depth;
                // `general_profile_idc`, the low five bits of the byte after the
                // configuration version.
                hevc_profile = hvcc.get(1).map(|b| b & 0x1f);
                profile = video_profile::hevc_record(&hvcc);
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
                profile = video_profile::av1(&parameter_sets);
            }
            // ProRes names its flavour in the sample entry's fourcc and nowhere else.
            Codec::ProRes => profile = video_profile::prores(&info.sample_entry),
            // The Visual Object Layer header out of the `esds`, put back ahead of every sync
            // sample that lacks one. H.263 (`s263`, `h263`) states none: its picture header is
            // the whole configuration.
            Codec::Mpeg4 => {
                parameter_sets = reader
                    .streams()
                    .iter()
                    .find(|s| s.index == stream)
                    .and_then(|s| s.params.extradata.as_ref())
                    .map(|b| b.as_ref().to_vec())
                    .unwrap_or_default();
                profile = video_profile::mpeg4(Some(&info.sample_entry), &parameter_sets);
            }
            // No parameter sets: a VP8 or VP9 sample is self-contained, and so is every
            // intra-only picture (their facts come from the first sample, `fill_intra`).
            Codec::Vp8 | Codec::Vp9 | Codec::Mpeg2 | Codec::Dnxhd | Codec::Mjpeg | Codec::Dv => {}
            // The synthesized header `ec-riff`'s raw decoder reads its layout from.
            Codec::Raw => {
                header = reader
                    .streams()
                    .iter()
                    .find(|s| s.index == stream)
                    .and_then(|s| s.params.extradata.as_ref())
                    .map(|b| b.as_ref().to_vec())
                    .unwrap_or_default();
                profile = Some(video_profile::raw(&header));
            }
            // AVI only.
            Codec::MsMpeg4 => {}
        }
        let chroma = bitstream_chroma(codec, &parameter_sets);
        let samples = reader.samples(stream);
        let sample_count = samples.len() as u32;
        // An `stss` table, as the sample sync flags state it. An empty list means
        // every sample is a sync sample, which is what a track with no `stss` box
        // is, so an all-sync track is stored that way.
        let sync_samples: Vec<u32> = if samples.iter().all(|s| s.sync) {
            Vec::new()
        } else {
            samples
                .iter()
                .enumerate()
                .filter(|(_, s)| s.sync)
                .map(|(i, _)| i as u32 + 1)
                .collect()
        };
        // Where a seek to frame 0 would put the cursor: the samples the edit list
        // trims are still read, they are references for the ones that show.
        let next_sample = sync_at_or_before(&sync_samples, first_sample);
        // Only a track that carries composition offsets *and* sync points stores
        // its samples out of display order; the demuxer answers for that and
        // says nothing otherwise.
        let sync_display = reader.sync_in_display_order(stream);
        let first_display = match sync_display.is_empty() {
            true => 0,
            false => {
                let times: Vec<i64> = samples.iter().map(|s| s.cts).collect();
                display_index(&times, first_sample - 1)
            }
        };
        let (container_tags, bitstream) = (
            colr_tags(path, track_id).unwrap_or_default(),
            bitstream_tags(codec, &parameter_sets),
        );
        Ok((
            VideoMeta {
                // The samples the edit list trims off the front are not frames of
                // the presentation, so they are not counted as ones.
                frame_count: sample_count.saturating_sub(first_sample - 1),
                color: ColorDescription::resolve_sized(
                    container_tags,
                    bitstream,
                    meta.width,
                    meta.height,
                ),
                matrix_assumed: ColorDescription::matrix_assumed(container_tags, bitstream),
                ..meta
            },
            Self {
                reader,
                stream,
                colour: colr_tags(path, track_id).unwrap_or_default(),
                sample_count,
                first_sample,
                codec,
                parameter_sets,
                nal_length,
                hevc_profile,
                sync_samples,
                sync_display,
                first_display,
                next_sample,
                bit_depth,
                profile,
                chroma,
                light: mp4_light(path, track_id),
                intra: crate::decode::intra::IntraFacts::default(),
                header,
                codec_id: ec_core::CodecId::RawVideo,
                size: (coded_w, coded_h),
                path: path.to_path_buf(),
            },
        ))
    }

    /// Next access unit in decode order: Annex-B framed for H.264 and HEVC, the
    /// mp4 sample verbatim for VP9. `None` at end of track.
    fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
        if self.next_sample > self.sample_count {
            return Ok(None);
        }
        let index = self.next_sample.saturating_sub(1) as usize;
        let packet = match self.reader.read_sample(self.stream, index) {
            Ok(packet) => packet,
            Err(ec_core::Error::Eof) => return Ok(None),
            Err(e) => return Err(mp4_error(e)),
        };
        self.next_sample += 1;
        let is_sync = packet.flags.keyframe;
        let bytes: &[u8] = &packet.data;
        // A VP8 or VP9 mp4 sample is one (super)frame the decoder parses on
        // its own: no length prefixes to strip, no parameter sets to re-inject.
        // A ProRes, DNxHD or MJPEG sample is one whole picture, likewise.
        // A DV sample is one whole frame.
        if matches!(self.codec, Codec::Vp8 | Codec::Vp9 | Codec::ProRes | Codec::Dnxhd | Codec::Mjpeg | Codec::Dv | Codec::Raw) {
            return Ok(Some(bytes.to_vec()));
        }
        // An MPEG-4 Part 2 sample is one VOP (or a packed pair); the VOL header goes in front
        // of a sync sample that has none, where the decoder may start.
        if self.codec == Codec::Mpeg4 {
            let mut au = Vec::with_capacity(self.parameter_sets.len() + bytes.len());
            if is_sync && !has_vol(bytes) {
                au.extend_from_slice(&self.parameter_sets);
            }
            au.extend_from_slice(bytes);
            return Ok(Some(au));
        }
        // An AV1 one is a whole temporal unit, likewise unframed -- with the
        // sequence header in front of it where a decoder may start.
        if self.codec == Codec::Av1 {
            let mut au = Vec::with_capacity(self.parameter_sets.len() + bytes.len());
            if is_sync {
                au.extend_from_slice(&self.parameter_sets);
            }
            au.extend_from_slice(bytes);
            return Ok(Some(au));
        }

        let mut au = Vec::with_capacity(self.parameter_sets.len() + bytes.len() + 16);
        if is_sync {
            au.extend_from_slice(&self.parameter_sets);
        }
        append_annex_b(bytes, self.nal_length, &mut au)?;
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

    /// Every frame a decoder may be started from, by the index this engine
    /// counts a source's frames in -- the sample table's own answer, which is
    /// why an mp4 costs no walk to ask ([`sync_points`]). A track with no `stss`
    /// box is every frame, which is what the box's absence means.
    fn sync_frames(&self) -> Vec<u32> {
        let frames = self.sample_count.saturating_sub(self.first_sample - 1);
        if self.sync_samples.is_empty() {
            return (0..frames).collect();
        }
        // With composition offsets in play a sample id is not a frame index, and
        // the display-order table beside it is the one that answers -- the split
        // [`Self::seek_to_sync_at_or_before`] makes, for its reason.
        if !self.sync_display.is_empty() {
            return self
                .sync_display
                .iter()
                .filter_map(|&(display, _)| display.checked_sub(self.first_display))
                .collect();
        }
        self.sync_samples
            .iter()
            .filter_map(|&id| id.checked_sub(self.first_sample))
            .collect()
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
    /// What the track's `Colour` element declared, exactly as the file wrote it
    /// (unspecified fields left unspecified); see [`MkvDemuxer::colour_tags`].
    tags: Tags,
    /// The stated profile; see [`Demuxer::profile`].
    profile: Option<&'static str>,
    /// Nanoseconds a timestamp tick is worth -- `TimestampScale`, a millisecond
    /// in every file anything writes. What turns a block's own timestamp into
    /// the nanoseconds a copy re-times it by.
    timestamp_scale: u64,
    /// Bytes of the NAL length prefix an HEVC or H.264 block is written with;
    /// unused by AV1, whose block is one temporal unit and carries no prefixes.
    nal_length: usize,
    /// Bits per luma sample; see [`Demuxer::bit_depth`].
    bit_depth: u8,
    /// The chroma format the stream states; see [`Demuxer::chroma`].
    chroma: Option<Chroma>,
    /// How bright the track says it gets; see [`Demuxer::light`].
    light: ContentLight,
    /// What an intra-only track's first sample states; see [`Demuxer::intra`].
    intra: crate::decode::intra::IntraFacts,
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
                            "{codec} video in a Matroska file is not supported — AV1, HEVC, H.264, VP9 and VP8 are"
                        ),
                    }
                    .into(),
                    // No picture in it *at all*, which is a fact about the file
                    // and not a fault: [`NoVideoTrack`] says why that is a type.
                    None => crate::Error::from(NoVideoTrack(
                        "this Matroska file has no video track".to_string(),
                    )),
                });
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
        let chroma = bitstream_chroma(video.codec, &video.config);
        let profile = match video.codec {
            Codec::H264 => video_profile::h264(&video.config),
            Codec::Hevc => video_profile::hevc_record(&video.private),
            Codec::Av1 => video_profile::av1(&video.config),
            Codec::Mpeg4 => video_profile::mpeg4(None, &video.config),
            _ => None,
        };
        let mut mkv = Self {
            file,
            path: path.to_path_buf(),
            blocks: Vec::new(),
            codec: video.codec,
            config: video.config,
            private: video.private,
            tags: video.tags,
            timestamp_scale: video.timestamp_scale,
            nal_length: video.nal_length,
            bit_depth: video.bit_depth,
            profile,
            chroma,
            light: video.light,
            intra: crate::decode::intra::IntraFacts::default(),
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
        // The displayed shape, in the order the two facts compose -- the same
        // order the render applies them in
        // ([`crate::decode::Render::frame`]): the aspect widens the *coded*
        // frame (180x320 at SAR 4/3 is drawn 240x320 -- ffprobe's own
        // `display_aspect_ratio` of such a file, measured), then the quarter
        // turn swaps that stretched pair the way it swaps the coded one. The
        // other order is a different picture: 320x180 then widened by 4/3
        // lands at 426x180, and a rotated anamorphic file rendered that way
        // lands a square raster in the 9:16 canvas its probe declares --
        // letterboxed in the preview and in the export both. The SAR only
        // ever widens a width, never a height; the displayed height is the
        // coded one without a turn and the stretched coded width with one.
        // The coded pair stays what the decoder hands back, and carries on to
        // [`VideoMeta::coded_width`]/[`VideoMeta::coded_height`].
        let (width, height) = match video.rotation.swaps_axes() {
            true => (video.height, video.pixel_aspect.width_factor(video.width)),
            false => (video.pixel_aspect.width_factor(video.width), video.height),
        };
        let meta = VideoMeta {
            width,
            height,
            coded_width: video.width,
            coded_height: video.height,
            pixel_aspect: video.pixel_aspect,
            frame_rate,
            frame_count: mkv.frames as u32,
            codec: video.codec,
            // `config` is the parameter sets by now -- Annex-B for H.264 and
            // HEVC, the sequence header OBU for AV1 -- which is what the
            // bitstream tier reads.
            // The height the picture is *shown* at, which is what the mp4 door
            // feeds here and the whole reason both doors resolve their size the
            // same way: the 720-line rule below is a guess about the material, and
            // the material is the picture as displayed -- a device-held `.mkv` is
            // 640 coded but 1280 shown, and the two sides of that rule disagree.
            color: ColorDescription::resolve_sized(
                video.tags,
                bitstream_tags(video.codec, &mkv.config),
                width,
                height,
            ),
            matrix_assumed: ColorDescription::matrix_assumed(
                video.tags,
                bitstream_tags(video.codec, &mkv.config),
            ),
            // The turn off the track's `Projection` element ([`projection_rotation`]),
            // which is where a Matroska muxer states what an mp4 states with its
            // `tkhd` matrix.
            rotation: video.rotation,
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
            *reach = mkv_cluster(file, body, stop, *number, blocks, track_bytes, &mut laced)?;
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
    /// one temporal unit already, and for VP8 and VP9, whose blocks are one
    /// (super)frame each; Annex-B for HEVC and H.264, whose blocks hold the
    /// same length-prefixed NALs an mp4 sample does.
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
        check_extent(file, block.at, block.len)?;
        scratch.resize(block.len, 0);
        read_exact_at(file, block.at, scratch)?;
        // What the muxer stripped or compressed, put back before anything reads
        // the bytes as a codec's: a stripped block is not a NAL and an inflated
        // one is not where it was read from.
        unpack.frame(scratch)?;
        if self.codec == Codec::Mpeg4 {
            let mut au = Vec::with_capacity(head + self.scratch.len());
            if !has_vol(&self.scratch) {
                au.extend_from_slice(&self.config[..head]);
            }
            au.extend_from_slice(&self.scratch);
            return Ok(Some(au));
        }
        if matches!(
            self.codec,
            Codec::Av1 | Codec::Vp8 | Codec::Vp9 | Codec::ProRes | Codec::Dnxhd | Codec::Mjpeg | Codec::Dv
        ) {
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

    /// What the track's `Colour` element says, field by field, as the file
    /// wrote it: a copy writes these back and nothing else, so a file that says
    /// nothing copies out saying nothing, rather than the resolved guess
    /// [`VideoMeta::color`] is.
    pub fn colour_tags(&self) -> Tags {
        self.tags
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
        check_extent(file, block.at, block.len)?;
        scratch.resize(block.len, 0);
        read_exact_at(file, block.at, scratch)?;
        unpack.frame(scratch)?;
        Ok(Some(CodedBlock {
            bytes: &self.scratch,
            key: block.key,
            // Widened and fallible, exactly as the subtitle walk's tick scale
            // is ([`ticks_scaled`]): `block.ts` is whatever the file says, so the
            // multiply overflows an `i64` on a crafted cluster and a copy would
            // write a block time that wrapped negative.
            ts_ns: ticks_scaled(block.ts, self.timestamp_scale, 1)?,
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
/// Empty for a file that will not open, and for a Matroska one whose clusters
/// cannot be walked.
///
/// An mp4 answers off its `stss` table for nothing; Matroska costs the walk,
/// once per file (6.7 s on a 12.9 GB film, then a sidecar read of a few
/// milliseconds -- [`MkvDemuxer::is_sync`]): ask it off a render thread.
pub fn sync_points(path: &Path) -> Vec<u32> {
    if ps::is_mpeg_system(path) && !is_matroska(path) {
        return Vec::new();
    }
    if !is_matroska(path) {
        return match Demuxer::open(path) {
            Ok((_, Demuxer::Mp4(mp4))) => mp4.sync_frames(),
            _ => Vec::new(),
        };
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
    /// The `Audio` element's `Channels`, `None` where the track states none.
    pub channels: Option<u16>,
    /// The `Audio` element's `SamplingFrequency` in Hz, `None` where unstated.
    pub sample_rate: Option<u32>,
    /// The `CodecPrivate` as the file holds it: an AAC track's AudioSpecificConfig.
    /// Empty where the track carries none.
    config: Vec<u8>,
    /// The `Audio` element's `BitDepth`, `None` where unstated.
    bit_depth: Option<u32>,
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
    /// The Matroska codec id these blocks are packets of: `ac3`, `eac3`,
    /// `truehd` or `mlp`.
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
            "A_TRUEHD" => "truehd",
            "A_MLP" => "mlp",
            // DTS has its own door (`crate::extaudio`); its blocks are not read here.
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
        check_extent(&self.file, block.at, block.len)?;
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
    /// The sample aspect ratio the track's `Display*` elements state, square
    /// where they state none ([`PixelAspect::SQUARE`]) -- the same reading
    /// every Matroska reader makes of their absence.
    pixel_aspect: PixelAspect,
    /// The quarter turn the track's `Projection` asks for;
    /// [`Rotation::None`] where it states none.
    rotation: Rotation,
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
                *buf = ec_inflate::inflate_zlib(buf, LIMIT)
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
// How the picture is drawn: Matroska carries the aspect as a `Display*` pair
// in *display units* (0x54B2) -- 0 pixels, 1 centimetres, 2 inches, 3 a display
// aspect ratio, 4 the sample aspect ratio itself. Absent, the pixels are the
// coded ones: every square-pixel file states none of these, which is what makes
// their absence the square answer rather than an error.
const DISPLAY_WIDTH: u32 = 0x54B0;
const DISPLAY_HEIGHT: u32 = 0x54BA;
const DISPLAY_UNIT: u32 = 0x54B2;
// The track's projection: how the flat picture sits in space. A roll on the
// rectangular projection (type 0) is how a Matroska muxer states what an mp4
// states with its `tkhd` matrix -- mkvmerge has translated display matrices
// into roll since v95, and ffmpeg's Matroska muxer writes a `Projection` for
// rotated input -- so the same quarter turns are read off it. Anything else (a
// spherical or mesh projection, a yaw/pitch pose) is not a turn this engine can
// make, and is refused by name like the mp4 door refuses a matrix that is not a
// quarter turn; a spherical one's pose is the sphere's orientation rather than
// the picture's and is left alone ([`projection_rotation`]).
const PROJECTION: u32 = 0x7670;
const PROJECTION_TYPE: u32 = 0x7671;
const PROJECTION_POSE_YAW: u32 = 0x7673;
const PROJECTION_POSE_PITCH: u32 = 0x7674;
const PROJECTION_POSE_ROLL: u32 = 0x7675;
// The `Colour` element and the three of its children this reads. Byte-verified
// against real files rather than taken off a spec table: published tables that
// list Range as 0x55B3 are describing ChromaSubsamplingHorz.
const COLOUR: u32 = 0x55B0;
const MATRIX_COEFFICIENTS: u32 = 0x55B1;
const RANGE: u32 = 0x55B9;
const PRIMARIES: u32 = 0x55BB;
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
const AUDIO: u32 = 0xE1;
const CHANNELS: u32 = 0x9F;
const SAMPLING_FREQUENCY: u32 = 0xB5;
const BIT_DEPTH: u32 = 0x6264;
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
/// `DiscardPadding`, inside a `BlockGroup`: signed nanoseconds the decoder
/// drops from the block's end.
const DISCARD_PADDING: u32 = 0x75A2;
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

/// What [`mkv_tracks`] yields, in order: the video track, every audio track,
/// the codec of a further video track (if any), the timestamp scale, and the
/// duration in seconds.
type MkvTracks = (
    Option<MkvVideo>,
    Vec<MkvAudioTrack>,
    Option<String>,
    u64,
    Option<f64>,
);

/// The video track of `segment`, **every** audio track in file order, the codec
/// id of a video track this cannot read, and the segment's `TimestampScale` in
/// nanoseconds. The audio list is what [`MkvAudio`] reads a sound track from and
/// what [`matroska_audio_tracks`] hands a stream picker; it and the other codec
/// id are what tell a user why a file plays silent
/// ([`crate::audio::AudioSession::unsupported`]) or refuses to open at all.
///
/// Header only: the walk stops at the first `Cluster`, so this costs a handful
/// of seeks whatever the file weighs.
fn mkv_tracks(file: &mut File, segment: (u64, u64)) -> crate::Result<MkvTracks> {
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
pub(crate) const ISO_639_1_TO_2: &[(&str, &str)] = &[
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
    // The `Display*` elements: unstated for the square-pixel file, which is
    // most of them.
    let (mut display_width, mut display_height, mut display_unit) = (0u64, 0u64, 0u64);
    let mut rotation = Rotation::None;
    let (mut language, mut name, mut bcp47) = (String::new(), String::new(), String::new());
    let (mut channels, mut sample_rate, mut bit_depth) = (None, None, None);
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
            AUDIO => {
                let mut at = body;
                while let Some(e) = ebml_element(file, at, stop)? {
                    match e.0 {
                        CHANNELS => channels = Some(ebml_uint(file, e.1, e.2)?).filter(|c| *c > 0),
                        BIT_DEPTH => {
                            bit_depth = u32::try_from(ebml_uint(file, e.1, e.2)?)
                                .ok()
                                .filter(|d| *d > 0)
                        }
                        SAMPLING_FREQUENCY => {
                            sample_rate = Some(ebml_float(file, e.1, e.2)?)
                                .filter(|f| *f > 0.0)
                                .map(|f| f.round() as u32)
                        }
                        _ => {}
                    }
                    at = e.2;
                }
            }
            VIDEO => {
                let mut at = body;
                while let Some(e) = ebml_element(file, at, stop)? {
                    match e.0 {
                        // A container may state any size at all, and these
                        // two reach a per-frame allocation directly
                        // (`decode.rs` sizes its planes as
                        // `display_width * height`). The engine already has
                        // the bound for this -- [`crate::MAX_RESOLUTION`],
                        // 8K -- and it was applied to a keystroke and to a
                        // `.edith` line but never to a header, so a
                        // 65535x65535 track opened and asked for a
                        // gigabyte-scale buffer. A size past the bound is
                        // refused by name here, where the file can be
                        // refused, rather than discovered as a capacity
                        // overflow one layer down.
                        PIXEL_WIDTH => {
                            let stated = ebml_uint(file, e.1, e.2)?;
                            if stated > u64::from(crate::MAX_RESOLUTION) {
                                return Err(unsupported_display_size(stated));
                            }
                            width = stated as u32;
                        }
                        PIXEL_HEIGHT => {
                            let stated = ebml_uint(file, e.1, e.2)?;
                            if stated > u64::from(crate::MAX_RESOLUTION) {
                                return Err(unsupported_display_size(stated));
                            }
                            height = stated as u32;
                        }
                        DISPLAY_WIDTH => display_width = ebml_uint(file, e.1, e.2)?,
                        DISPLAY_HEIGHT => display_height = ebml_uint(file, e.1, e.2)?,
                        DISPLAY_UNIT => display_unit = ebml_uint(file, e.1, e.2)?,
                        PROJECTION => {
                            let (mut kind, mut yaw, mut pitch, mut roll) =
                                (0u64, 0.0f64, 0.0f64, 0.0f64);
                            let mut at = e.1;
                            while let Some(c) = ebml_element(file, at, e.2)? {
                                match c.0 {
                                    PROJECTION_TYPE => kind = ebml_uint(file, c.1, c.2)?,
                                    PROJECTION_POSE_YAW => yaw = ebml_float(file, c.1, c.2)?,
                                    PROJECTION_POSE_PITCH => pitch = ebml_float(file, c.1, c.2)?,
                                    PROJECTION_POSE_ROLL => roll = ebml_float(file, c.1, c.2)?,
                                    // ProjectionPrivate: the spherical/mesh
                                    // payload, which only matters where the
                                    // type is one of those -- and those refuse
                                    // below before it could be read.
                                    _ => {}
                                }
                                at = c.2;
                            }
                            rotation = projection_rotation(kind, yaw, pitch, roll)?;
                        }
                        // `Colour`: what the file says its picture's numbers
                        // mean, in the same H.273 code points the bitstream
                        // uses. An element the file leaves out stays 0, which
                        // is "unspecified" in every one of those tables and
                        // falls to the tier below (see [`crate::colorspace`]).
                        COLOUR => {
                            let (mut matrix, mut transfer, mut range, mut primaries) =
                                (0, 0, 0, 0);
                            let mut at = e.1;
                            while let Some(c) = ebml_element(file, at, e.2)? {
                                match c.0 {
                                    MATRIX_COEFFICIENTS => matrix = ebml_uint(file, c.1, c.2)?,
                                    TRANSFER_CHARACTERISTICS => {
                                        transfer = ebml_uint(file, c.1, c.2)?
                                    }
                                    RANGE => range = ebml_uint(file, c.1, c.2)?,
                                    PRIMARIES => primaries = ebml_uint(file, c.1, c.2)?,
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
                            tags = Tags::from_codes(matrix, transfer, range)
                                .with_primaries(primaries);
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
        // Matroska has no codec id for Speex: a muxer writes `A_MS/ACM` with the
        // WAVEFORMATEX tag 0xA109 in the `CodecPrivate`. Name it by what it is,
        // so it is refused (and warned about) as Speex rather than as "ACM".
        if codec == "A_MS/ACM" && config.starts_with(&[0x09, 0xA1]) {
            codec = String::from("A_SPEEX");
        }
        return Ok(MkvEntry::Audio(MkvAudioTrack {
            number,
            codec,
            // Whichever element the file states it in, and `und` for one that
            // states neither ([`mkv_language`]) -- the same answer the subtitle
            // walk gets, so a dual-audio remux and its subtitles agree.
            language: mkv_language(&language, &bcp47),
            name,
            channels: channels.and_then(|c| u16::try_from(c).ok()),
            sample_rate,
            config,
            bit_depth,
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
    let Some(known) = mkv_codec(&codec).or_else(|| mkv_private_codec(&codec, &config)) else {
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
        // the SPS/PPS out of the record. The depth is the SPS's own ([`avc_bit_depth`]).
        Codec::H264 => {
            let (nal_length, sets) = parse_avcc(&config)?;
            let bit_depth = avc_bit_depth(&sets)?;
            (Codec::H264, nal_length, sets, bit_depth)
        }
        // A VP9 block is one self-contained (super)frame: no length prefixes to
        // strip and no configuration record to re-inject -- a `.webm` writes no
        // `CodecPrivate` at all, and the `vpcC` an mp4 writes is not bitstream
        // and would corrupt the frame if it were prepended. The depth is not in
        // the container either, so it is read off the first keyframe by
        // [`Demuxer::fill_vp9_depth`]; 8 here is what that probe starts from.
        Codec::Vp9 => (Codec::Vp9, 0, Vec::new(), 8),
        // A VP8 block is one self-contained frame, exactly as a VP9 one: no
        // length prefixes to strip, no configuration record to re-inject -- a
        // `.webm` writes no `CodecPrivate` for it, and the `vp08` an mp4
        // carries no record beside. 8 is not an assumption here but the
        // format itself: VP8 has no depth but 8.
        Codec::Vp8 => (Codec::Vp8, 0, Vec::new(), 8),
        // Not a Matroska codec here: `V_MPEG2` is a named gap in [`UNSUPPORTED`].
        Codec::Mpeg2 => return Err("MPEG-2 video is read from a transport stream only".into()),
        // One whole picture per block, no configuration record to re-inject: Motion JPEG
        // (`V_MJPEG`) and Avid DNxHD/DNxHR (`V_QUICKTIME` / `V_MS/VFW/FOURCC` with an
        // `AVdn`/`AVdh` tag in `CodecPrivate`, which `private` keeps). Their depth and
        // chroma are read off the first sample by [`Demuxer::fill_intra`].
        Codec::Mjpeg => (Codec::Mjpeg, 0, Vec::new(), 8),
        Codec::Dnxhd => (Codec::Dnxhd, 0, Vec::new(), 8),
        // Not a Matroska codec here: `V_PRORES` is a named gap in [`UNSUPPORTED`].
        Codec::ProRes => return Err("ProRes is read from QuickTime/MP4 only".into()),
        // The VOL header: `CodecPrivate` of `V_MPEG4/ISO/*`, or the bytes after the
        // BITMAPINFOHEADER of a `V_MS/VFW/FOURCC` XviD / DivX track.
        Codec::Mpeg4 => {
            let vol = if codec == "V_MS/VFW/FOURCC" { config.get(40..).unwrap_or_default().to_vec() } else { config };
            (Codec::Mpeg4, 0, vol, 8)
        }
        // DV is not a Matroska codec here.
        Codec::Dv => return Err("DV is read from QuickTime/MP4, AVI and raw .dv files only".into()),
        Codec::MsMpeg4 | Codec::Raw => return Err("MS-MPEG4 and raw video are read from AVI only".into()),
    };
    Ok(MkvEntry::Video(MkvVideo {
        number,
        width,
        height,
        pixel_aspect: pixel_aspect_of(
            display_width,
            display_height,
            display_unit,
            width,
            height,
        ),
        rotation,
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

/// The refusal for an H.264 shape above 8 bits that edith does not carry, in
/// [`hevc_refusal`]'s words, with the conversion.
fn avc_refusal(what: &str, profile: &str) -> crate::Error {
    format!(
        "{what} is not supported — edith decodes H.264 in 4:2:0, 4:2:2, 4:4:4 and monochrome at 8 and 9/10 bits (High, High 10, High 4:2:2, High 4:4:4 Predictive; progressive or interlaced) only ({profile}; convert to 4:2:0 8/10-bit with ffmpeg -pix_fmt yuv420p10le -c:v libx264 -profile:v high10)"
    )
    .into()
}

/// The luma bit depth the first SPS of an H.264 stream states: 8 for every
/// profile but the High ones, whose SPS carries it. Above 8 bits only the shapes
/// `ec-h264` decodes open (9 or 10 bits, one depth for luma and chroma); more
/// than 10 bits, a luma/chroma depth mismatch and FMO are refused by name at
/// open, where the 8-bit path is left exactly as it was. An SPS that does not parse reads as 8-bit.
fn avc_bit_depth(sets: &[u8]) -> crate::Result<u8> {
    let Some(nal) = AnnexBIter::new(sets).find(|n| n.first().is_some_and(|b| b & 0x1f == 7)) else {
        return Ok(8);
    };
    let mut rbsp = Vec::new();
    unescape_rbsp(&nal[1..], &mut rbsp);
    let Ok(sps) = Sps::parse(&rbsp) else {
        return Ok(8);
    };
    let (luma, chroma) = (sps.bit_depth_luma, sps.bit_depth_chroma);
    let profile = format!("H.264 profile_idc {}", sps.profile_idc);
    // FMO (several slice groups) needs the macroblock-to-slice-group map of
    // clause 8.2.2, which `ec-h264` does not carry: named here, at open, instead
    // of failing on the first picture.
    for nal in AnnexBIter::new(sets).filter(|n| n.first().is_some_and(|b| b & 0x1f == 8)) {
        let mut pps_rbsp = Vec::new();
        unescape_rbsp(&nal[1..], &mut pps_rbsp);
        if let Ok(pps) = Pps::parse(&pps_rbsp, |_| Some(&sps))
            && pps.num_slice_groups > 1
        {
            return Err(avc_refusal(
                &format!("H.264 FMO (flexible macroblock ordering, {} slice groups)", pps.num_slice_groups),
                &profile,
            ));
        }
    }
    // Every shape `ec-h264` decodes opens: 4:2:0, 4:2:2, 4:4:4 (also with
    // separate colour planes), monochrome (handed over as 4:2:0 with neutral
    // chroma) and lossless, at 8 and 9/10 bits. Chroma depth is only stated for
    // streams that have chroma.
    let has_chroma = sps.chroma_format_idc != 0 && !sps.separate_colour_plane;
    let chroma = if has_chroma { chroma } else { luma };
    if luma <= 8 && chroma <= 8 {
        return Ok(8);
    }
    if luma != chroma {
        return Err(avc_refusal("H.264 with chroma at a different bit depth from luma", &profile));
    }
    if luma > 10 {
        return Err(avc_refusal(&format!("{luma}-bit H.264"), &profile));
    }
    // Interlaced (PAFF and MBAFF) High 10 4:2:0 decodes on `ec-h264` like the
    // progressive kind; only the shapes refused above are still refused.
    Ok(10)
}

/// AVCDecoderConfigurationRecord (ISO 14496-15 §5.3.3.1) -> the NAL length
/// prefix width and the SPS/PPS as one Annex-B blob, which is [`parse_hvcc`]'s
/// job for HEVC. The mp4 path and a Matroska file carry the identical record.
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
/// 8, 10 or 12. The software seat narrows all three to the 8-bit planes the
/// renderer takes ([`crate::decode::narrow_av1`]); the 10-bit and 12-bit ones
/// stay at their depth for a 10-bit export.
/// 4:2:2 and 4:4:4 are taken too (profiles 2 and 1): the seats average their
/// chroma down to 4:2:0. Only the impossible 4:4:0 is refused, by name.
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
    // The seq_profile is the top three bits of byte 1 (AV1-ISOBMFF §2.3.3): 0 is
    // 4:2:0 and monochrome, 1 is 4:4:4, 2 is 4:2:2 and 12-bit. Named in the refusal.
    let profile = rec[1] >> 5;
    let bit_depth = match (flags & 0x40 != 0, flags & 0x20 != 0) {
        (true, true) => 12,
        (true, false) => 10,
        _ => 8,
    };
    // The chroma format, out of the same byte: `monochrome`, then the two
    // subsampling bits (AV1-ISOBMFF §2.3.3). 4:4:0 (x=0, y=1) is no AV1 format.
    let (mono, sub_x, sub_y) = (flags & 0x10 != 0, flags >> 3 & 1, flags >> 2 & 1);
    if !mono && (sub_x, sub_y) == (0, 1) {
        return Err(av1_refusal("AV1 4:4:0", profile));
    }
    Ok((rec.get(4..).unwrap_or_default().to_vec(), bit_depth))
}

/// The refusal for an AV1 format that does not exist, in the words a user is
/// shown: what the stream is, then the profile it came in. Raised at open, so
/// probe and import both show it before any export starts.
fn av1_refusal(what: &str, profile: u8) -> crate::Error {
    format!(
        "{what} is not supported — AV1 has no such format (AV1 profile {profile})"
    )
    .into()
}

/// The quarter turn an mp4 `tkhd` display matrix asks for, as `ec-mp4` read it
/// off the track, or an [`UnsupportedRotation`] where it asks for something else.
///
/// `ec-mp4` does the reading (exact integer arithmetic, [`ec_core::Rotation`]),
/// so this only names the turn and states the refusal, with the angle the matrix
/// states: `-atan2(b, a)` over the turned x-axis, counter-clockwise as ffprobe
/// reports it (the same convention [`rotation_of_roll`] states), or none for a
/// mirror or a shear, where 0.0 would be a claim rather than a reading.
///
/// The direction is `ec-mp4`'s: `-display_rotation 90` writes `a=0, b=-65536`,
/// which is [`Rotation::Cw270`] here, the quarter turn that puts the coded left
/// edge at the top. This is the quarter count
/// [`crate::scale::rotate_i420_90s`] applies.
fn rotation_of_track(rotation: ec_core::Rotation) -> crate::Result<Rotation> {
    let (a, b) = match rotation {
        ec_core::Rotation::None => return Ok(Rotation::None),
        ec_core::Rotation::Cw90 => return Ok(Rotation::Cw90),
        ec_core::Rotation::Cw180 => return Ok(Rotation::Cw180),
        ec_core::Rotation::Cw270 => return Ok(Rotation::Cw270),
        ec_core::Rotation::Other { a, b, .. } => (a, b),
    };
    let fixed = |raw: i32| f64::from(raw) / 65536.0;
    let (a, b) = (fixed(a), fixed(b));
    Err(UnsupportedRotation {
        // Counter-clockwise, as ffprobe's `rotation` side data states it. `+ 0.0`
        // turns a negative zero into a zero, so the sentence never reads "-0.0".
        degrees: (a.hypot(b) > 0.0).then(|| -b.atan2(a).to_degrees() + 0.0),
    }
    .into())
}

/// The quarter turn a Matroska `ProjectionPoseRoll` asks for -- the mkv door's
/// half of the display-matrix reading (the mp4 door's is [`rotation_of_track`]),
/// with the same half-degree tolerance and the same refusal by name. The angle it
/// states in that refusal is the roll itself: counter-clockwise, as ffprobe
/// reports the same turn, so the mp4 and mkv sentences for one matrix agree.
///
/// **Measured against ffmpeg's own autorotate**, the same arbiter the mp4 door's
/// matrix was measured against: `test_rotation{90,180,270}.mkv`
/// (`scripts/gen_fixtures.sh`) are the very quadrant source their `.mp4` twins
/// are, remuxed with `-display_rotation` asked for, and ffmpeg 8.1.2's matroska
/// muxer writes this element for it -- `+90.0`, `+180.0` and **`-90.0`**, an
/// 8-byte big-endian float, with no `ProjectionType` element at all (RFC 9559's
/// default of 0, rectangular). Its demuxer reads each of those back as the very
/// same `tkhd` matrix the mp4 twin carries, so both files autorotate to the same
/// four quadrants -- which is what `tests/rotation.rs` asserts, corner by corner.
///
/// Hence the negation below: the element counts **counter-clockwise** degrees
/// about the viewing axis (RFC 9559 says so, and the measured layouts confirm
/// it), while the matrix this engine counts its quarter turns off is the
/// clockwise one `-display_rotation 90` writes -- so a roll of +90 is
/// [`Rotation::Cw270`] here, exactly as `a=0, b=-65536, c=65536, d=0` is.
fn rotation_of_roll(degrees: f64) -> crate::Result<Rotation> {
    let steps = -(degrees / 90.0).round();
    if !degrees.is_finite() || (degrees + steps * 90.0).abs() > 0.5 {
        return Err(UnsupportedRotation {
            degrees: degrees.is_finite().then_some(degrees),
        }
        .into());
    }
    Ok(match steps.rem_euclid(4.0) as u8 {
        0 => Rotation::None,
        1 => Rotation::Cw90,
        2 => Rotation::Cw180,
        _ => Rotation::Cw270,
    })
}

/// The sample aspect ratio a Matroska `TrackEntry`'s `Video` element states,
/// from its `DisplayWidth`/`DisplayHeight`/`DisplayUnit` children and the coded
/// size beside them.
///
/// The units decide what the pair *is*: 3 spells a display aspect ratio (16 and
/// 9 over 1440x1080 is what ffmpeg writes for a `setsar=4/3` source, measured),
/// 4 spells the sample aspect ratio itself, and **0 spells pixels** -- which is
/// the ordinary case and the one a `Display*` pair is *for*: the spec has the
/// pair in pixels, so `1920`x`1080` beside a `1440`x`1080` coded track is
/// exactly the 4/3 stretch, stated the way this door below reads it. An absent
/// pair is the file saying nothing, which is also square; a pair that *is*
/// present and equal to the coded one is square by arithmetic, not by
/// assumption.
///
/// 1 and 2 (centimetres, inches) are physical sizes a picture this engine
/// shows on a screen cannot mean anything by, and read as square rather than
/// guessed. Every unstated or unusable shape is [`PixelAspect::SQUARE`], which
/// is also the answer the spec's own default gives: DisplayWidth and
/// DisplayHeight "SHOULD be set to the PixelWidth and PixelHeight" when they
/// mean the same thing, so a file that means square most often says nothing
/// at all.
fn pixel_aspect_of(
    display_width: u64,
    display_height: u64,
    display_unit: u64,
    coded_width: u32,
    coded_height: u32,
) -> PixelAspect {
    // A zero pair (unstated, or a header that lies about it) and a zero coded
    // size are all square: there is nothing to divide by and nothing to stretch.
    if display_width == 0 || display_height == 0 || coded_width == 0 || coded_height == 0 {
        return PixelAspect::SQUARE;
    }
    // A stated pair is judged by what it *means*, never by how it was spelled,
    // and the three units mean three different things:
    //
    //   4 -- the pair IS the sample aspect ratio. It is read as a ratio and
    //        judged by its terms, because a `pasp`-shaped claim is a claim
    //        about a ratio and chosen bytes do not reduce small.
    //   3 -- the pair is a display aspect ratio, which has to be brought onto
    //        the coded frame to become one. That derivation multiplies by the
    //        coded size, so what reduces afterwards carries the *sizes*: a
    //        239:100 DAR over a 1920x1080 coded track derives 2151:1600, over
    //        3840x2160 the same, and both are past any term bound. So the
    //        width is computed directly from the ratio instead and only the
    //        degenerate case is refused.
    //   0 -- the pair is a display pixel size, taken as the display width.
    //
    // Units 3 and 0 are the same rule: neither is a ratio, so neither may be
    // judged by the terms its derived pair reduces to. What bounds both is the
    // raster [`PixelAspect::width_factor`] produces, which cannot refuse a real
    // ratio because it never looks at the ratio's terms at all.
    let coded = (coded_width, coded_height);
    match display_unit {
        // 4: the pair *is* the sample aspect ratio.
        4 => PixelAspect::stated(
            display_width.min(u64::from(u32::MAX)) as u32,
            display_height.min(u64::from(u32::MAX)) as u32,
        )
        .unwrap_or(PixelAspect::SQUARE),
        // 3: a display aspect ratio against the coded frame, taken unreduced so
        // the term bound never judges it -- `width_factor` multiplies the coded
        // width by this and bounds the raster it produces, and a bound on the
        // picture has no conflict with a real ratio.
        3 => PixelAspect::stated_ratio(display_width, display_height, coded)
            .unwrap_or(PixelAspect::SQUARE),
        // 0: a display pixel size -- the width the picture is drawn at.
        // 1920x1080 over 1440x1080 is the 4/3 stretch; 1920x803 is a 2.39:1
        // scope. Bounded by the raster, not by the terms it reduces to.
        0 => PixelAspect::stated_size(display_width, display_height, coded)
            .unwrap_or(PixelAspect::SQUARE),
        // A physical unit this engine cannot interpret: square.
        _ => PixelAspect::SQUARE,
    }
}

/// The turn a track's `Projection` asks for, or [`Rotation::None`] where it does
/// not ask for a turn of the picture at all.
///
/// Only a **rectangular** projection has a pose that turns the *picture*, and
/// rectangular (0) is what an absent `ProjectionType` means. On an
/// equirectangular or cubemap one those same three elements state how the sphere
/// is oriented, which is not a turn of anything 2D: the pose is left alone, as a
/// flat player shows a 360° file, rather than refused -- refusing would cost a
/// file that plays right now and would not show it any righter.
///
/// A rectangular projection whose **yaw or pitch** is off zero is refused by
/// name, though, and that is mkvmerge's spelling to watch for: since v95 it
/// translates an mp4 display matrix into roll *and* yaw, using yaw for a mirror.
/// A picture mirrored, or turned out of its own plane, is the one thing no
/// quarter turn here can make -- the same missing capability the mp4 door
/// refuses a 45-degree matrix for, so it gets the same refusal rather than a
/// sideways picture.
fn projection_rotation(kind: u64, yaw: f64, pitch: f64, roll: f64) -> crate::Result<Rotation> {
    if kind != 0 {
        return Ok(Rotation::None);
    }
    let off_plane = [yaw, pitch]
        .into_iter()
        .any(|degrees| !degrees.is_finite() || degrees.abs() > 0.5);
    match off_plane {
        // No angle to name: a pose out of the picture's own plane -- a mirror
        // through it, or a tilt of it -- is not a rotation in that plane whatever
        // number it states, which is what the sentence without one says.
        true => Err(UnsupportedRotation { degrees: None }.into()),
        false => rotation_of_roll(roll),
    }
}

/// Why an mp4 came back with no picture, by the fourcc of the video track it
/// does carry: `avc3` and `mp4v` are files with video in them, and telling their
/// owner there is none sends them looking for a fault that is not in the file.
/// [`UNSUPPORTED`]'s reason where the table has one, the fourcc itself where it
/// does not.
fn mp4_no_video(tracks: &[ec_mp4::TrackInfo]) -> crate::Error {
    let found = tracks
        .iter()
        .find(|t| matches!(t.media, Some(ec_core::MediaType::Video)))
        .map(|t| String::from_utf8_lossy(&t.sample_entry).trim().to_string());
    match found {
        Some(kind) if MP4_ONLY.iter().any(|(c, f)| *c == Codec::Raw && String::from_utf8_lossy(*f).trim() == kind) => format!(
            "uncompressed {kind} video in this mp4 is not supported in this layout — raw RGB (16, 24 and 32 bit), 2vuy, yuvs, yuv2, v210, v410 and b16g are; convert it with ffmpeg -i in.mov -c:v libx264 -c:a aac out.mp4"
        )
        .into(),
        Some(kind) => match UNSUPPORTED.iter().find(|(id, _)| *id == kind) {
            Some((_, why)) => format!("{kind} video in this mp4 is not supported — {why}"),
            None => {
                format!("{kind} video in this mp4 is not supported — H.264, HEVC, VP9, AV1 and VP8 are")
            }
        }
        .into(),
        // Nothing with a picture in it that this reads -- an audio-only mp4,
        // which is a thing a person really does hand an editor. The Matroska
        // door's own answer, and the same type for the same reason.
        None => crate::Error::from(NoVideoTrack(
            "no H.264, HEVC, VP9, AV1 or VP8 video track in file".to_string(),
        )),
    }
}

/// `count` bits, MSB first, from bit offset `at`. Up to 24 at a time, which is
/// the frame sync code and the widest field read here.
fn vp9_bits(au: &[u8], at: &mut usize, count: usize) -> Option<u32> {
    let mut value = 0;
    for _ in 0..count {
        let byte = *au.get(*at / 8)?;
        value = value << 1 | u32::from(byte >> (7 - *at % 8) & 1);
        *at += 1;
    }
    Some(value)
}

/// The bit depth and chroma format of a VP9 keyframe, out of its uncompressed
/// header (VP9 bitstream spec §6.2 `uncompressed_header` and §6.2.2
/// `color_config`) -- the one place a VP9 stream states either. See
/// [`Demuxer::fill_vp9_depth`] for why neither container is asked.
///
/// `None` when this access unit cannot answer -- not a keyframe, a
/// `show_existing_frame`, a truncated block -- which leaves the caller's 8-bit
/// default standing rather than guessing; profile 0 and 1 *are* 8-bit by
/// definition, and they are all a `.webm` off the web ever is. The chroma is
/// `None` on its own where the header ends inside the colour config, or where
/// it names a subsampling [`Chroma`] has no name for.
fn vp9_keyframe(au: &[u8]) -> crate::Result<Option<(u8, Option<Chroma>)>> {
    let at = &mut 0;
    // frame_marker, then the profile as two bits written low one first; profile 3
    // spends a reserved bit before the rest.
    if vp9_bits(au, at, 2) != Some(2) {
        return Ok(None);
    }
    let (Some(low), Some(high)) = (vp9_bits(au, at, 1), vp9_bits(au, at, 1)) else {
        return Ok(None);
    };
    let profile = high << 1 | low;
    if profile == 3 && vp9_bits(au, at, 1).is_none() {
        return Ok(None);
    }
    // show_existing_frame: such a frame is a reference already decoded and its
    // header stops right here. Then frame_type (0 is a keyframe), show_frame and
    // error_resilient_mode, and only a keyframe carries the colour config.
    if vp9_bits(au, at, 1) != Some(0) || vp9_bits(au, at, 1) != Some(0) {
        return Ok(None);
    }
    if vp9_bits(au, at, 2).is_none() {
        return Ok(None);
    }
    if vp9_bits(au, at, 24) != Some(0x49_8342) {
        return Ok(None);
    }
    let depth = match profile < 2 {
        true => 8,
        // A 12-bit stream is refused by name here, exactly as a 12-bit AV1 is in
        // `parse_av1c`: the plugin has an NV12 pool and a P010 one and no third.
        false => match vp9_bits(au, at, 1) {
            Some(0) => 10,
            Some(_) => {
                return Err(
                    "12-bit VP9 is not supported — 8- and 10-bit are what the decoder carries"
                        .into(),
                );
            }
            None => return Ok(None),
        },
    };
    Ok(Some((depth, vp9_chroma(au, at, profile))))
}

/// The chroma format of a VP9 keyframe's `color_config` from the colour space
/// on (§6.2.2): sRGB is 4:4:4 and writes nothing further; any other space is a
/// colour range, and then the subsampling bits only profiles 1 and 3 code.
/// Profiles 0 and 2 are 4:2:0 by definition.
fn vp9_chroma(au: &[u8], at: &mut usize, profile: u32) -> Option<Chroma> {
    const SRGB: u32 = 7;
    if vp9_bits(au, at, 3)? == SRGB {
        return Some(Chroma::C444);
    }
    vp9_bits(au, at, 1)?; // color_range
    if profile != 1 && profile != 3 {
        return Some(Chroma::C420);
    }
    match (vp9_bits(au, at, 1)?, vp9_bits(au, at, 1)?) {
        (1, 1) => Some(Chroma::C420),
        (1, 0) => Some(Chroma::C422),
        (0, 0) => Some(Chroma::C444),
        _ => None,
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
                // of it naming another track's clusters: one AV1 remux writes
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
        // The whole segment is the walk a cancel exists for -- twenty seconds of
        // disk on a cold 25 GB remux -- and this is where a Cancel beside an
        // import, or a scrub step that superseded this open two hundred steps
        // ago, stops the *read* instead of dropping its answer.
        if cancelled() {
            return Err(CANCELLED.into());
        }
        at = stop;
        if id != CLUSTER {
            continue;
        }
        at = mkv_cluster(
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
/// Whether an element is one a Cluster's children never hold: a sibling of the
/// Cluster, so the Cluster is over where it starts. An unknown-length Cluster
/// has no end of its own -- a live recording writes none -- and is over where
/// the next of these begins, which is how it is read.
fn ends_cluster(id: u32) -> bool {
    matches!(id, CLUSTER | CUES | TRACKS | INFO | SEEK_HEAD | SEGMENT)
}

fn mkv_cluster(
    file: &mut File,
    body: u64,
    end: u64,
    number: u64,
    blocks: &mut Vec<Block>,
    track_bytes: &mut Vec<(u64, u64)>,
    laced: &mut Vec<(usize, usize)>,
) -> crate::Result<u64> {
    let mut cluster_ts = 0i64;
    let mut child = body;
    while let Some((id, body, stop)) = ebml_element(file, child, end)? {
        if ends_cluster(id) {
            return Ok(child);
        }
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
        let ts = block_ticks(cluster_ts, block.rel)?;
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
    Ok(end)
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
const INDEX_MAGIC: &[u8; 8] = b"EDMKVIX3";

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
        let dir = cache_dir(
            crate::envcfg::var_os("XDG_CACHE_HOME"),
            std::env::var_os("HOME"),
            "mkvindex",
        )?;
        Some(dir.join(format!(
            "{:016x}-{}.idx",
            fnv1a(self.path.as_os_str().as_encoded_bytes()),
            self.number
        )))
    }
}

/// The path rule for one of this engine's cache directories -- `sub` names
/// which (`mkvindex` here, `proxies` for [`crate::proxy`]) -- with the
/// environment handed in so it can be checked, as `keymap::config_path_in` is
/// on the config side. An empty `XDG_CACHE_HOME` is one the spec says to
/// ignore; with no `HOME` either there is nowhere to put a cache, and `None` --
/// always walk -- beats littering the working directory.
pub(crate) fn cache_dir(
    xdg: Option<OsString>,
    home: Option<OsString>,
    sub: &str,
) -> Option<PathBuf> {
    xdg.filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .map(|dir| dir.join("edith").join(sub))
}

/// FNV-1a, 64-bit: what names a sidecar and what checks it. Not a cryptographic
/// hash and not asked to be -- nothing here is adversarial. It is the guard that
/// turns a half-written or scrambled file into a miss; a *name* collision is
/// caught by the key inside the file, which is compared field by field.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
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
            if !rest.is_multiple_of(count) {
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

/// Refuses a `len`-byte block at `at` that runs past the end of `file`, before a
/// buffer of that size is made: a length in the file is a claim the file may not
/// back, and the allocation is the thing a crafted one is after.
pub(crate) fn check_extent(file: &File, at: u64, len: usize) -> crate::Result<()> {
    let end = file.metadata()?.len();
    match at.checked_add(len as u64) {
        Some(stop) if stop <= end => Ok(()),
        _ => Err(format!("a {len}-byte block at {at} runs past the {end}-byte file").into()),
    }
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
    Some(Tags::from_codes(code(8)?, code(6)?, range).with_primaries(code(4).unwrap_or(0)))
}

/// The `pasp` box of `track_id`'s sample entry (ISO/IEC 14496-12 §6.2.3, the
/// pixel aspect ratio box a visual sample entry may carry): two 32-bit
/// big-endian integers, hSpacing then vSpacing, the ratio one coded pixel is
/// drawn at. ffmpeg writes it for a `setsar=4/3` source and writes 1/1 for a
/// square one (both measured); a box absent -- most mp4s there are -- is square
/// pixels, the same answer Matroska's absent `Display*` elements give.
///
/// [`None`] rather than an error for a file with no such box, which is most of
/// them -- an untagged mp4 is not a broken one.
fn pasp_of(path: &Path, track_id: u32) -> PixelAspect {
    let Ok((_, entry)) = sample_entry(path, track_id) else {
        return PixelAspect::SQUARE;
    };
    // The same fixed 78-byte VisualSampleEntry header `colr` and `hvcC` sit
    // behind; `pasp` is one of their siblings.
    let Some(pasp) = child(entry.get(78..).unwrap_or_default(), b"pasp") else {
        return PixelAspect::SQUARE;
    };
    let (Some(h), Some(v)) = (
        pasp.get(..4).map(|b| u32::from_be_bytes(b.try_into().unwrap())),
        pasp.get(4..8).map(|b| u32::from_be_bytes(b.try_into().unwrap())),
    ) else {
        return PixelAspect::SQUARE;
    };
    // Eight bytes a file chose, read into a ratio that sizes every raster
    // downstream, so they are judged by [`PixelAspect::stated`] here: the
    // aspect this engine *reports* is one it can draw, not merely one it
    // survived.
    PixelAspect::stated(h, v).unwrap_or(PixelAspect::SQUARE)
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

/// The refusal for an HEVC shape edith does not carry, in the words
/// [`av1_refusal`] uses for AV1's, with the profile and the conversion.
fn hevc_refusal(what: &str, profile: u8) -> crate::Error {
    format!(
        "{what} is not supported — edith decodes HEVC Main, Main 10 and range extensions (4:0:0, 4:2:0, 4:2:2, 4:4:4 at 8 to 12 bits) only (HEVC profile {profile}; convert to 4:2:0 8/10-bit with ffmpeg -pix_fmt yuv420p)"
    )
    .into()
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
    // 17 is bit_depth_luma_minus8 in its low 3 bits, 18 the chroma one. The
    // software seat carries 8, 10 and 12 bits, luma and chroma apart; the
    // VA-API plugin only ever gets the 8-bit NV12 and 10-bit P010 ones
    // (Main and Main 10), the rest are routed to software before it is asked.
    let bit_depth = 8 + (rec[17] & 0x7);
    let chroma_depth = 8 + (rec[18] & 0x7);
    if bit_depth > 12 || chroma_depth > 12 {
        return Err(hevc_refusal(
            &format!("{}-bit HEVC", bit_depth.max(chroma_depth)),
            rec[1] & 0x1f,
        ));
    }
    // general_profile_idc in the low five bits of byte 1: 4 is RExt, which
    // carries 4:0:0, 4:2:2 and 4:4:4 at up to 12 bits; 5 and up (high
    // throughput, multiview, scalable, 3D and SCC) are not carried.
    let profile = rec[1] & 0x1f;
    if profile >= 5 {
        return Err(hevc_refusal(&format!("HEVC profile {profile}"), profile));
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
            if matches!(head & 0x3f, 32..=34) {
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
        // `size - at` rather than `at + size`: a 64-bit size near u64::MAX wraps
        // the sum back into the file and the walk never advances.
        if size < header_len || size > end - at {
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
    /// reads: the `S_TEXT/*` ones, WebVTT (`D_WEBVTT/*`) and `S_HDMV/PGS`, whose
    /// blocks are display sets of run-length pictures rather than lines. A codec
    /// neither of those -- VobSub off a DVD -- comes back declared and empty.
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

/// The Matroska codec id of DVD bitmap subtitles: one subpicture unit per block,
/// the `.idx` text (palette, size) in `CodecPrivate`.
pub const VOBSUB: &str = "S_VOBSUB";

/// The Matroska codec id of DVB bitmap subtitles: one display set per block.
pub const DVBSUB: &str = "S_DVBSUB";

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
        .filter(|t| {
            (t.codec.starts_with("S_TEXT") || t.codec.starts_with("D_WEBVTT/") || t.codec == PGS
                || t.codec == VOBSUB
                || t.codec == DVBSUB)
                && t.unsupported.is_none()
        })
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
/// The samples come out of `ec-mp4`'s sample index, the same for any track type;
/// the *title* is read by hand out of the `moov` ([`mp4_track_names`]), the way
/// [`sample_entry`] reads an `stsd` entry.
///
/// A file with no timed-text track comes back empty, never an error; a file that
/// is not an mp4 at all is an error, as [`matroska_subtitles`] is for a
/// non-Matroska.
pub fn mp4_subtitles(path: &Path) -> crate::Result<Vec<Mp4Subtitle>> {
    let file = File::open(path)?;
    let mut reader =
        ec_mp4::Mp4Demuxer::new(BufReader::new(file)).map_err(mp4_error)?;
    // By track id, which is the order the tracks were written in: a row picks a
    // track by its number out of a list a user saw.
    let mut infos: Vec<ec_mp4::TrackInfo> = reader
        .tracks()
        .iter()
        .filter(|t| t.media == Some(ec_core::MediaType::Subtitle) && t.stream.is_some())
        .cloned()
        .collect();
    infos.sort_unstable_by_key(|t| t.track_id);
    let names = mp4_track_names(path)?;
    let mut tracks = Vec::with_capacity(infos.len());
    for info in infos {
        let Some(stream) = info.stream else {
            continue;
        };
        // Ticks to microseconds, through the stream's own time base.
        let (num, den) = reader
            .streams()
            .iter()
            .find(|s| s.index == stream)
            .map_or((1, 1000), |s| (s.time_base.num(), s.time_base.den()));
        let us = move |ticks: i64| {
            (i128::from(ticks) * i128::from(num) * 1_000_000 / i128::from(den.max(1))) as i64
        };
        let count = reader.samples(stream).len();
        let mut samples = Vec::with_capacity(count);
        for i in 0..count {
            // A sample the tables cannot reach is skipped rather than raised: the
            // other cues of the track are still the words a viewer needs.
            let sample = reader.samples(stream)[i];
            let packet = match reader.read_sample(stream, i) {
                Ok(packet) => packet,
                Err(ec_core::Error::Eof) => continue,
                Err(e) => return Err(mp4_error(e)),
            };
            samples.push(Mp4TextSample {
                start_us: us(sample.dts),
                end_us: us(sample.dts + i64::from(sample.duration)),
                payload: packet.data.to_vec(),
            });
        }
        tracks.push(Mp4Subtitle {
            number: u64::from(info.track_id),
            language: info.language.clone().unwrap_or_else(|| "und".to_string()),
            name: names
                .iter()
                .find(|(track_id, _)| *track_id == info.track_id)
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
    // [`ticks_scaled`] does the arithmetic: it is widened and fallible, and a
    // corrupt cluster timestamp is refused rather than wrapped into a negative
    // cue time -- see the test beside it.
    let us = |ticks: i64| ticks_scaled(ticks, timestamp_scale, 1_000);
    let mut at = segment.0;
    while let Some((id, body, stop)) = ebml_element(file, at, segment.1)? {
        // The import's second walk, stopped by the same Cancel as its first.
        if cancelled() {
            return Err(CANCELLED.into());
        }
        at = stop;
        if id != CLUSTER {
            continue;
        }
        let mut cluster_ts = 0i64;
        let mut child = body;
        while let Some((id, body, stop)) = ebml_element(file, child, stop)? {
            if ends_cluster(id) {
                at = child;
                break;
            }
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
                    // The cluster's timestamp plus the block's own signed
                    // offset, checked by [`block_ticks`]: the pair is where a
                    // corrupt file's two numbers can still overflow what an
                    // `i64` holds, one step before the scale above would refuse
                    // the result.
                    start_us: us(block_ticks(cluster_ts, block.rel)?)?,
                    duration_us: duration.map(us).transpose()?,
                    payload,
                });
            }
        }
    }
    Ok(())
}

/// A block's absolute timestamp in ticks: the cluster's own, plus the block's
/// signed offset from it -- refused where the sum leaves what an `i64` holds.
///
/// Both walks need it: the video block index a copy reads and the subtitle cues.
/// Both get `cluster_ts` from an EBML uint the file writes with no bound this
/// reader applies, so a crafted cluster near `i64::MAX` overflows the add one
/// step *before* [`ticks_scaled`] could refuse the product -- and the block would
/// be taken at a time that wrapped negative.
fn block_ticks(cluster_ts: i64, rel: i16) -> crate::Result<i64> {
    cluster_ts
        .checked_add(i64::from(rel))
        .ok_or_else(|| "a Matroska block at no timestamp a clock can hold".into())
}

/// A tick count scaled out of the file's own clock, or the refusal a corrupt one
/// earns: `ticks * TimestampScale / per_tick`, which is microseconds for the
/// subtitle walk (`per_tick` 1_000) and nanoseconds for the block walk a copy
/// reads (`per_tick` 1).
///
/// The multiply is widened to `i128` rather than done in `i64`, and a result that
/// does not fit is refused rather than saturated: both numbers are the file's --
/// a cluster or block timestamp is an EBML uint nothing bounds, and
/// `TimestampScale` is a scale it chooses -- so `ticks * timestamp_scale`
/// overflows on a crafted file, which is a panic in debug and a *negative*
/// timestamp in release, silently. Widened no product can overflow; a timestamp
/// that then does not fit its unit is the same class of corrupt as the oversized
/// block refused above, and gets the same loud refusal rather than a time no
/// clock can hold.
fn ticks_scaled(ticks: i64, timestamp_scale: u64, per_tick: u64) -> crate::Result<i64> {
    i64::try_from(i128::from(ticks) * i128::from(timestamp_scale) / i128::from(per_tick))
        .map_err(|_| "a Matroska timestamp no clock can hold".into())
}

/// A string element, without the trailing NULs a `Name` may be padded with.
fn string_of(file: &mut File, body: u64, stop: u64) -> crate::Result<String> {
    let bytes = ebml_bytes(file, body, stop)?;
    Ok(String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .to_owned())
}

#[cfg(test)]
mod zlib_tests;

#[cfg(test)]
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

#[cfg(test)]
/// 1-based id of the sample the presentation starts on, for a track trimmed by
/// `trim` media ticks ([`trim_ticks`]). `None` (no edit list) and zero are both
/// "no trim", i.e. the first sample.
fn first_frame_sample(entries: impl IntoIterator<Item = (u32, u32)>, trim: Option<u64>) -> u32 {
    trim.map_or(1, |t| crate::audio::packet_at(entries, t, 0).0)
}

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// MSB-first bit writer for the hand-made H.264 parameter sets below.
    #[derive(Default)]
    struct Bits(Vec<bool>);

    impl Bits {
        fn bits(&mut self, value: u32, n: u32) -> &mut Self {
            (0..n).rev().for_each(|i| self.0.push(value >> i & 1 == 1));
            self
        }
        fn ue(&mut self, value: u32) -> &mut Self {
            let v = value + 1;
            let len = 32 - v.leading_zeros();
            self.bits(0, len - 1).bits(v, len)
        }
        fn se(&mut self, value: i32) -> &mut Self {
            self.ue(if value > 0 { 2 * value as u32 - 1 } else { (-2 * value) as u32 })
        }
        /// The NAL (header byte `head`) with the stop bit, zero padding and
        /// emulation prevention, behind an Annex-B start code.
        fn nal(&mut self, head: u8) -> Vec<u8> {
            self.0.push(true);
            while !self.0.len().is_multiple_of(8) {
                self.0.push(false);
            }
            let mut out = vec![0, 0, 0, 1, head];
            let mut zeros = 0;
            for byte in self.0.chunks(8).map(|c| c.iter().fold(0u8, |a, &b| a << 1 | u8::from(b))) {
                if zeros >= 2 && byte <= 3 {
                    out.push(3);
                    zeros = 0;
                }
                zeros = if byte == 0 { zeros + 1 } else { 0 };
                out.push(byte);
            }
            out
        }
    }

    /// A High 4:4:4 Predictive SPS (progressive, 16x16 macroblocks) at `depth` bits.
    fn hi444_sps(depth: u32) -> Vec<u8> {
        let mut b = Bits::default();
        b.bits(244, 8).bits(0, 8).bits(40, 8).ue(0); // profile, flags, level, sps id
        b.ue(3).bits(0, 1).ue(depth - 8).ue(depth - 8).bits(0, 1).bits(0, 1);
        b.ue(0).ue(2).ue(1).bits(0, 1); // frame num bits, poc type 2, refs, gaps
        b.ue(0).ue(0).bits(1, 1).bits(1, 1).bits(0, 1).bits(0, 1); // size, frame_mbs_only, direct, crop, vui
        b.nal(0x67)
    }

    fn pps(slice_groups: u32) -> Vec<u8> {
        let mut b = Bits::default();
        b.ue(0).ue(0).bits(0, 2).ue(slice_groups - 1);
        if slice_groups > 1 {
            b.ue(0); // slice_group_map_type 0: interleaved
            (0..slice_groups).for_each(|_| {
                b.ue(0);
            });
        }
        b.ue(0).ue(0).bits(0, 1).bits(0, 2).se(0).se(0).se(0).bits(0, 3);
        b.nal(0x68)
    }

    #[test]
    fn avc_refuses_more_than_ten_bits_and_fmo_by_name_and_opens_the_rest() {
        let sets = |depth, groups| [hi444_sps(depth), pps(groups)].concat();
        assert_eq!(avc_bit_depth(&sets(8, 1)).expect("4:4:4 8-bit opens"), 8);
        assert_eq!(avc_bit_depth(&sets(10, 1)).expect("4:4:4 10-bit opens"), 10);
        for depth in [12, 14] {
            let refused = avc_bit_depth(&sets(depth, 1)).expect_err("above ten bits").to_string();
            assert!(refused.starts_with(&format!("{depth}-bit H.264 is not supported")), "{refused}");
        }
        let refused = avc_bit_depth(&sets(8, 3)).expect_err("FMO").to_string();
        assert!(refused.starts_with("H.264 FMO (flexible macroblock ordering, 3 slice groups) is not supported"), "{refused}");
        assert!(refused.contains("ffmpeg"), "{refused}");
    }

    #[test]
    fn a_block_declared_past_the_file_is_refused_before_it_is_allocated() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fuzz/huge_block.mkv");
        let file = File::open(&path).expect("the fixture");
        // Within the file: fine. A declared 1 TiB: refused at once, with the sizes named.
        check_extent(&file, 588, 1000).expect("a block the file backs");
        let started = std::time::Instant::now();
        let err = check_extent(&file, 588, 1 << 40).expect_err("past the end of the file");
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert!(err.to_string().contains("runs past"), "{err}");
        // An offset that wraps when added to the length is refused too.
        assert!(check_extent(&file, u64::MAX - 4, 16).is_err());
    }

    #[test]
    fn aac_profile_reads_the_config_the_way_the_spec_states_it() {
        // AOT 2, 44.1 kHz, stereo: the plain LC config ffmpeg writes.
        assert_eq!(aac_profile_of_config(&[0x12, 0x10]), Some("LC"));
        // A plain AOT 2 config (mono core): LC, its SBR being implicit in the bitstream.
        assert_eq!(aac_profile_of_config(&[0x15, 0x08]), Some("LC"));
        // AOT 2 whose sync extensions name SBR (0x2b7), then SBR and PS (0x548).
        assert_eq!(aac_profile_of_config(&[0x13, 0x90, 0x56, 0xe5, 0xa0]), Some("HE-AAC"));
        assert_eq!(
            aac_profile_of_config(&[0x13, 0x88, 0x56, 0xe5, 0xa5, 0x48, 0x80]),
            Some("HE-AACv2")
        );
        // AOT 5, explicit SBR: the HE-AAC v1 fixture's own config.
        assert_eq!(aac_profile_of_config(&[0x2b, 0x92, 0x08, 0x00]), Some("HE-AAC"));
        // AOT 29, explicit SBR and PS.
        assert_eq!(aac_profile_of_config(&[0xeb, 0x8a, 0x10, 0x00]), Some("HE-AACv2"));
        // An object type the list does not name, and an empty config.
        assert_eq!(aac_profile_of_config(&[0x00, 0x00]), None);
        assert_eq!(aac_profile_of_config(&[]), None);
    }

    #[test]
    fn mkv_aac_profile_prefers_the_config_then_the_codec_id() {
        assert_eq!(mkv_aac_profile("A_AAC", &[0x12, 0x10]), Some("LC"));
        assert_eq!(mkv_aac_profile("A_AAC/MPEG4/LC", &[]), Some("LC"));
        assert_eq!(mkv_aac_profile("A_AAC/MPEG4/LC/SBR", &[]), Some("HE-AAC"));
        assert_eq!(mkv_aac_profile("A_AAC/MPEG2/MAIN", &[]), Some("Main"));
        assert_eq!(mkv_aac_profile("A_AAC/MPEG4/LTP", &[]), Some("LTP"));
        assert_eq!(mkv_aac_profile("A_OPUS", &[0x12, 0x10]), None);
    }

    #[test]
    fn esds_config_reads_four_byte_descriptor_lengths() {
        // ES_Descriptor (3) with a 4-byte length, flags 0; DecoderConfig (4) with
        // 13 fixed bytes; DecoderSpecificInfo (5) carrying the two-byte config.
        let dsi = [0x05, 0x80, 0x80, 0x80, 0x02, 0x12, 0x10];
        let mut dc = vec![0x04, 0x80, 0x80, 0x80, (13 + dsi.len()) as u8];
        dc.extend([0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        dc.extend(dsi);
        let mut es = vec![0x03, 0x80, 0x80, 0x80, (3 + dc.len()) as u8, 0, 1, 0];
        es.extend(dc);
        let mut esds = vec![0, 0, 0, 0];
        esds.extend(es);
        assert_eq!(esds_config(&esds), Some(&[0x12, 0x10][..]));
    }

    /// Both walks that scale a file's own clock: the subtitle one
    /// (microseconds, `per_tick` 1_000) and the block one a copy reads
    /// (nanoseconds, `per_tick` 1). The product is where an `i64` overflows, so
    /// these are the assertions that fail -- a panic in debug, a negative
    /// timestamp in release -- without the widening.
    #[test]
    fn a_timestamp_no_clock_can_hold_is_refused() {
        // A millisecond scale, which is every file anything writes: 1000 ticks
        // is one second, in either unit.
        assert_eq!(ticks_scaled(1_000, 1_000_000, 1_000).unwrap(), 1_000_000);
        assert_eq!(ticks_scaled(0, 1_000_000, 1_000).unwrap(), 0);
        assert_eq!(ticks_scaled(1_000, 1_000_000, 1).unwrap(), 1_000_000_000);
        // A nanosecond scale and the largest tick count an EBML uint can cast
        // to: tens of thousands of years of nanoseconds, which no `i64` holds.
        assert!(
            ticks_scaled(i64::MAX, 1_000_000_000, 1).is_err(),
            "the block walk's product must not wrap into a negative time"
        );
        assert!(
            ticks_scaled(i64::MAX, 1_000_000_000, 1_000).is_err(),
            "the subtitle walk's product must not wrap into a negative cue time"
        );
        assert!(ticks_scaled(i64::MIN, 1_000_000_000, 1_000).is_err());
        // ...and the add that comes before either scale: a cluster timestamp near
        // the top of what an `i64` holds plus a block's own offset still has to
        // fit before any product is computed at all.
        assert_eq!(block_ticks(1_000, -5).unwrap(), 995);
        assert_eq!(block_ticks(0, 32_767).unwrap(), 32_767);
        assert!(
            block_ticks(i64::MAX, 1).is_err(),
            "the add must not wrap negative"
        );
        assert!(block_ticks(i64::MIN, -1).is_err());
    }

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

    /// **A cancelled walk stops walking.** The flag the import's Cancel and a
    /// superseded audio open both set ([`with_cancel`]) is read by the segment
    /// walk itself, so the answer is an abandoned read rather than a verdict
    /// about the file -- and the same walk without it still opens.
    #[test]
    fn a_cancelled_walk_gives_up_and_says_so() {
        // The first fixture the *video* demuxer accepts: the sweep also finds
        // audio-only `.mka`, which `MkvDemuxer::open` rightly refuses.
        let Some((path, lazy)) = matroska_fixtures()
            .into_iter()
            .find_map(|p| MkvDemuxer::open(&p).ok().map(|(_, lazy)| (p, lazy)))
        else {
            eprintln!("no Matroska fixture: the cancel walk is not measured");
            return;
        };
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (segment, number) = (lazy.segment, lazy.number);
        let mut file = File::open(&path).expect(&name);

        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let stopped = with_cancel(&flag, || mkv_walk_blocks(&mut file, segment, number));
        let error = stopped.expect_err("a cancelled walk must not answer");
        assert!(
            is_cancelled(&error),
            "{name}: a cancelled walk reported {error} -- a verdict about the file"
        );

        // ...and nothing is cancelled by having been near a flag: the walk that
        // was not asked to stop is the walk it always was.
        flag.store(false, Ordering::Release);
        let walked = with_cancel(&flag, || mkv_walk_blocks(&mut file, segment, number));
        assert!(
            walked.is_ok_and(|(blocks, ..)| !blocks.is_empty()),
            "{name}: the uncancelled walk found nothing"
        );
    }

    /// **A cancelled mp4 open stops reading the header too.** The `mp4` crate
    /// has no cancel of its own -- [`CancelableReader`] is what stands in --
    /// so this pre-sets the flag before [`Mp4Demuxer::open`] ever calls
    /// `Mp4Reader::read_header` and asserts the open comes back [`is_cancelled`]
    /// rather than a parsed header. On the reader that only wraps `BufReader`
    /// (pre-[`CancelableReader`]) the flag is never polled and this test fails
    /// with `Ok(_)`.
    #[test]
    fn a_cancelled_mp4_open_gives_up_and_says_so() {
        let path = asset("test_av.mp4");
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let opened = with_cancel(&flag, || Mp4Demuxer::open(&path));
        let error = opened.err().expect("a cancelled open must not parse a header");
        assert!(
            is_cancelled(&error),
            "a cancelled mp4 open reported {error} -- a verdict about the file"
        );
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
            cache_dir(xdg.map(OsString::from), home.map(OsString::from), "mkvindex")
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
        // precedence: some films carry a Bulgarian legacy code beside an English
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
        // An mp4 code that is not three letters says no language, as the Matroska
        // path does for an untagged track: zero, and the 0x7fff some muxers write
        // (which decodes to DELs, not letters).
        assert_eq!(unpack_language(0), "und");
        assert_eq!(unpack_language(0x7fff), "und");
        assert_eq!(unpack_language(0x15c7), "eng");
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
        for codec in [Codec::H264, Codec::Hevc, Codec::Vp9, Codec::Vp8, Codec::Av1, Codec::Mjpeg, Codec::Mpeg4] {
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
        // The intra-only professional codecs: every sample entry is reachable from an mp4,
        // and the Matroska half is a row, a `CodecPrivate` rule, or a documented gap.
        for (codec, fourcc) in MP4_ONLY {
            assert_eq!(mp4_codec(fourcc), Some(*codec), "{fourcc:?}");
        }
        // DNxHD in Matroska: a generic id, named by the tag in its private data.
        let mut quicktime = vec![0u8; 8];
        quicktime[4..8].copy_from_slice(b"AVdn");
        assert_eq!(mkv_private_codec("V_QUICKTIME", &quicktime), Some(Codec::Dnxhd));
        let mut vfw = vec![0u8; 24];
        vfw[16..20].copy_from_slice(b"AVdh");
        assert_eq!(mkv_private_codec("V_MS/VFW/FOURCC", &vfw), Some(Codec::Dnxhd));
        vfw[16..20].copy_from_slice(b"xvid");
        assert_eq!(mkv_private_codec("V_MS/VFW/FOURCC", &vfw), Some(Codec::Mpeg4));
        vfw[16..20].copy_from_slice(b"DIV3");
        assert_eq!(mkv_private_codec("V_MS/VFW/FOURCC", &vfw), None);
        assert_eq!(mkv_private_codec("V_VP9", &vfw), None);
        // DV (QuickTime entries) and H.263 (3GPP) are mp4 sample entries with no Matroska id; MPEG-4
        // Part 2 has `mp4v` and three Matroska ids.
        assert_eq!(mp4_codec(b"dvc "), Some(Codec::Dv));
        assert_eq!(mp4_codec(b"dv5p"), Some(Codec::Dv));
        assert_eq!(mp4_codec(b"dvh5"), Some(Codec::Dv));
        assert_eq!(mp4_codec(b"s263"), Some(Codec::Mpeg4));
        assert_eq!(mp4_codec(b"mp4v"), Some(Codec::Mpeg4));
        assert!(has_vol(&[0, 0, 1, 0xB0, 1, 0, 0, 1, 0xB6]));
        assert!(!has_vol(&[0, 0, 1, 0xB6, 0, 0, 1, 0x20]));
        // ProRes has no Matroska route: a gap row, not a silent fall-through.
        assert!(MP4_ONLY.iter().any(|(c, _)| *c == Codec::ProRes));
        assert!(UNSUPPORTED.iter().any(|(id, _)| *id == "V_PRORES"));

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
        assert_eq!(mkv_codec("V_MPEG4/MS/V3"), None);
        assert_eq!(mkv_codec("V_MPEG4/ISO/ASP"), Some(Codec::Mpeg4));
        assert_eq!(mkv_codec("V_MPEG4/ISO/SP"), Some(Codec::Mpeg4));
        // MJPEG-B and the QuickTime raw entries are mp4 sample entries with no Matroska id.
        assert_eq!(mp4_codec(b"mjpb"), Some(Codec::Mjpeg));
        assert_eq!(mp4_codec(b"v210"), Some(Codec::Raw));
        assert_eq!(mp4_codec(b"2vuy"), Some(Codec::Raw));
    }

    /// Packs a string of `0` and `1` into bytes, MSB first, zero-padded at the
    /// end: the way a header reads on the page, without the arithmetic.
    fn packed(bits: &str) -> Vec<u8> {
        bits.as_bytes()
            .chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0u8, |byte, (i, &b)| byte | (u8::from(b == b'1') << (7 - i)))
            })
            .collect()
    }

    /// The depth alone, as the tests below read it.
    fn vp9_depth(au: &[u8]) -> crate::Result<Option<u8>> {
        Ok(vp9_keyframe(au)?.map(|(depth, _)| depth))
    }

    /// The one place a VP9 stream states its own depth. Byte-aligned by
    /// construction: the fields ahead of the frame sync code are exactly 8 bits
    /// for profiles 0-2.
    #[test]
    fn a_vp9_keyframe_states_its_own_bit_depth() {
        // frame_marker 10, profile bits, show_existing 0, frame_type 0 (key),
        // show_frame 1, error_resilient 0, then the sync code 49 83 42.
        let profile0 = [0b1000_0010, 0x49, 0x83, 0x42];
        assert_eq!(vp9_depth(&profile0).unwrap(), Some(8));
        // profile 2 (low 0, high 1), then ten_or_twelve_bit = 0.
        let profile2 = [0b1001_0010, 0x49, 0x83, 0x42, 0b0000_0000];
        assert_eq!(vp9_depth(&profile2).unwrap(), Some(10));
        // ...and with it set: 12-bit, which has no surface pool here and is
        // refused by name rather than decoded into garbage.
        let twelve = [0b1001_0010, 0x49, 0x83, 0x42, 0b1000_0000];
        let refused = vp9_depth(&twelve).unwrap_err().to_string();
        assert!(refused.contains("12-bit VP9"), "{refused}");
        // Nothing to answer with is not an answer: an inter frame, a
        // show_existing_frame, a truncated block and a non-VP9 payload all leave
        // the caller's default standing.
        assert_eq!(vp9_depth(&[0b1000_0110, 0x49, 0x83, 0x42]).unwrap(), None);
        assert_eq!(vp9_depth(&[0b1000_1010, 0x49, 0x83, 0x42]).unwrap(), None);
        assert_eq!(vp9_depth(&profile2[..3]).unwrap(), None);
        assert_eq!(vp9_depth(&[0, 0, 0, 1, 0x65]).unwrap(), None);
        assert_eq!(vp9_depth(&[]).unwrap(), None);
    }

    /// The chroma format a VP9 keyframe's colour config codes, against the
    /// profiles that spell it out: profile 0 and 2 are 4:2:0, profile 1 codes
    /// its subsampling bits, and sRGB is 4:4:4 whatever the profile.
    #[test]
    fn a_vp9_keyframe_states_its_own_chroma_format() {
        // The sync code 49 83 42 as bits, then the keyframe's fields: show_existing
        // 0, frame_type 0, show_frame 1, error_resilient 0.
        let sync = format!("{:024b}", 0x49_8342u32);
        let key = "0010";
        let chroma = |head: &str, colour: &str| {
            let au = packed(&format!("{head}{key}{sync}{colour}"));
            vp9_keyframe(&au).unwrap().map(|(_, c)| c)
        };
        // profile 0 (low 0, high 0): BT.601 (space 001), range 0 -- nothing more.
        assert_eq!(chroma("1000", "0010"), Some(Some(Chroma::C420)));
        // profile 2 (low 0, high 1): ten_or_twelve_bit 0, space 001, range 0.
        assert_eq!(chroma("1001", "00010"), Some(Some(Chroma::C420)));
        // profile 1 (low 1, high 0): space 001, range 0, then subsampling_x and
        // subsampling_y, then a reserved bit. 1,0 is 4:2:2; 0,0 is 4:4:4.
        assert_eq!(chroma("1010", "001010"), Some(Some(Chroma::C422)));
        assert_eq!(chroma("1010", "001000"), Some(Some(Chroma::C444)));
        // sRGB (space 111) in profile 1: no range, no subsampling bits, 4:4:4.
        assert_eq!(chroma("1010", "111"), Some(Some(Chroma::C444)));
        // A header that ends inside the colour config keeps the depth and
        // leaves the chroma unanswered.
        let au = packed(&format!("{}{key}{sync}", "1000"));
        assert_eq!(vp9_keyframe(&au).unwrap(), Some((8, None)));
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
        rec[16] = 0xfc | 1; // chroma_format_idc 1: 4:2:0
        rec[18] = 0xf8; // bit_depth_chroma_minus8 0: 8-bit chroma, as luma
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
        main10[18] = 0xf8 | 2;
        assert_eq!(parse_hvcc(&main10).unwrap().2, 10);
        // RExt shapes open: 12-bit, 4:4:4, 4:0:0 and mixed depths, with the
        // depth the luma states.
        let mut main12 = rec.clone();
        main12[1] = 4;
        main12[17] = 0xf8 | 4;
        main12[18] = 0xf8 | 4;
        assert_eq!(parse_hvcc(&main12).unwrap().2, 12, "12-bit opens");
        let mut s422 = rec.clone();
        s422[1] = 4;
        s422[16] = 0xfc | 2;
        assert_eq!(parse_hvcc(&s422).unwrap().2, 8, "4:2:2 8-bit opens");
        s422[17] = 0xf8 | 2;
        s422[18] = 0xf8 | 2;
        assert_eq!(parse_hvcc(&s422).unwrap().2, 10, "4:2:2 10-bit opens");
        let mut s444 = rec.clone();
        s444[1] = 4;
        s444[16] = 0xfc | 3;
        assert_eq!(parse_hvcc(&s444).unwrap().2, 8, "4:4:4 opens");
        let mut s400 = rec.clone();
        s400[1] = 4;
        s400[16] = 0xfc;
        assert_eq!(parse_hvcc(&s400).unwrap().2, 8, "4:0:0 opens");
        let mut mixed = rec.clone();
        mixed[1] = 4;
        mixed[18] = 0xf8 | 2;
        assert_eq!(parse_hvcc(&mixed).unwrap().2, 8, "mixed depths open");
        let mut deep = rec.clone();
        deep[1] = 4;
        deep[17] = 0xf8 | 6;
        let refused = parse_hvcc(&deep).unwrap_err().to_string();
        assert!(refused.contains("14-bit"), "{refused}");
        let mut high = rec.clone();
        high[1] = 5;
        let refused = parse_hvcc(&high).unwrap_err().to_string();
        assert!(refused.contains("profile 5"), "{refused}");
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
    /// 10-bit reads as 10 and 12-bit as 12.
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
        assert_eq!(parse_av1c(&twelve).unwrap().1, 12);
        // `twelve_bit` without `high_bitdepth` says nothing, and an 8-bit
        // record reads as one whatever that bit holds.
        let mut stray = rec.clone();
        stray[2] |= 0x20;
        assert_eq!(parse_av1c(&stray).unwrap().1, 8);

        // An encoder that repeats its sequence header in-band writes no record
        // at all, which is not an error; a truncated one is.
        assert!(parse_av1c(&[]).unwrap().0.is_empty());
        assert!(parse_av1c(&[0x81, 0x05]).is_err());

        // The chroma format rides the same byte. Every real AV1 format is taken,
        // 4:2:2 and 4:4:4 at 8, 10 and 12 bits included; only 4:4:0 is refused.
        let read = |flags: u8| parse_av1c(&[0x81, 0x05, flags, 0x00]);
        for flags in [0x08, 0x00, 0x40, 0x48, 0x60, 0x68, 0x6C] {
            assert!(read(flags).is_ok(), "flags {flags:#x}");
        }
        assert_eq!(read(0x68).unwrap().1, 12, "12-bit 4:2:2");
        let refused = read(0x04).unwrap_err().to_string();
        assert!(refused.contains("AV1 4:4:0"), "{refused}");
        assert_eq!(read(0x0C).unwrap().1, 8, "4:2:0 at 8-bit");
        assert_eq!(read(0x4C).unwrap().1, 10, "4:2:0 at 10-bit");
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

    /// Four bytes of an mp4 `pasp` box are four bytes of untrusted input, and
    /// every one of them reaches the width an allocation is sized from. A
    /// header that claims `h == 0`, or one whose ratio widens the coded width
    /// past the whole `u32` range, must read as the picture it really is --
    /// square, drawn at the coded size -- rather than as a zero-column or a
    /// wrapped raster the decode worker tries to allocate.
    #[test]
    fn an_untrusted_pixel_aspect_cannot_size_a_zero_or_wrapped_raster() {
        // Two contracts, held separately, because they are enforced in two
        // places and conflating them is what would break a real ratio.
        //
        // 1. At the READ, a ratio is refused: a zero half, or a *reduced*
        //    pair carrying a term past the bound. This is where an mp4 `pasp`
        //    is judged, and it is right there because a `pasp` pair IS the
        //    ratio.
        for hostile in [
            PixelAspect { h: 0, v: 1 },
            PixelAspect { h: 1, v: 0 },
            PixelAspect { h: 0, v: 0 },
            // Four chosen bytes, not a ratio: reduced terms stay enormous.
            PixelAspect { h: 0xFFFF_FFFF, v: 0xFFFF_FFFF - 1 },
            PixelAspect { h: 1, v: 0x00FF_FFFF },
        ] {
            assert_eq!(
                PixelAspect::stated(hostile.h, hostile.v),
                None,
                "{hostile:?} is not a ratio this engine can draw"
            );
        }
        // A `DisplayUnit 0` pixel pair is NOT a ratio, so it is judged by
        // `stated_size` instead -- and the two most common letterbox ratios
        // must survive that arm, because its reduced form carries sizes:
        for (dw, dh) in [(1920u64, 1080u64), (1920, 803), (1920, 1037)] {
            assert!(
                PixelAspect::stated_size(dw, dh, (1440, 1080)).is_some(),
                "a {dw}x{dh} Display pair over a 1440x1080 coded track is a real picture"
            );
        }

        // 2. In the ARITHMETIC, whatever ratio arrived -- read or built --
        //    cannot produce a zero column or a raster past the bound. This is
        //    the contract that holds for every door, including the one that
        //    builds its ratio from a pixel size rather than reading one.
        let wrong: Vec<String> = [
            PixelAspect { h: 0, v: 1 },
            PixelAspect { h: 1, v: 0 },
            PixelAspect { h: 0, v: 0 },
            // 1920 * 0x00FF_FFFF is 32_212_252_800: it wraps a u32 to
            // 2_147_483_648, which is a plausible-looking "width" and an
            // impossible one to allocate.
            PixelAspect { h: 0x00FF_FFFF, v: 1 },
        ]
        .into_iter()
        .filter_map(|hostile| {
            let factor = hostile.width_factor(1920);
            (factor == 0 || factor > PixelAspect::MAX_DISPLAY_WIDTH)
                .then(|| format!("{hostile:?} -> {factor} (a zero column or an oversized raster)"))
        })
        .collect();
        assert!(
            wrong.is_empty(),
            "an unusable ratio must not size a raster:\n{}",
            wrong.join("\n")
        );
        // The wrap case lands on the coded width rather than on the wrapped
        // number, which is the specific refusal the defect turned on.
        assert_eq!(
            PixelAspect { h: 0x00FF_FFFF, v: 1 }.width_factor(1920),
            1920,
            "a wrapped multiply is drawn at the coded width, not at 2_147_481_728"
        );

        // The sane ratios still stretch, bit for bit as they always did: the
        // hardening refuses the untrusted, it does not round the ordinary.
        assert_eq!(PixelAspect { h: 4, v: 3 }.width_factor(1440), 1920);
        assert_eq!(PixelAspect { h: 1, v: 1 }.width_factor(1920), 1920);
        assert_eq!(PixelAspect::SQUARE.width_factor(1920), 1920);
        // An unreduced spelling of the same 4:3 lands on the same picture.
        assert_eq!(PixelAspect { h: 16, v: 12 }.width_factor(1440), 1920);
        // And a 2.39:1 scope built from a pixel size widens the coded frame
        // by its own ratio rather than being refused for carrying sizes.
        assert_eq!(
            PixelAspect::stated_size(1920, 803, (1440, 1080))
                .expect("a scope is a picture")
                .width_factor(1440),
            1920,
            "a 1920-wide scope over 1440 coded columns widens to 1920"
        );
    }

    /// The rule the two `Display*` units that are not ratios share, in one
    /// place: bringing a pair onto the coded frame makes what reduces carry the
    /// coded *sizes*, so no term bound can judge it. A 2.39:1 scope is the case
    /// that proves it -- the same DAR over three coded frames, which a term
    /// bound reads three different ways.
    #[test]
    fn a_derived_pair_is_read_by_its_raster_on_every_coded_frame() {
        for (coded, display) in [
            ((1920u32, 1080u32), 2582u32),
            ((3840, 2160), 5162),
            ((720, 576), 1376),
        ] {
            let derived = PixelAspect::stated_ratio(239, 100, coded).expect("a scope is a picture");
            assert_eq!(
                derived.width_factor(coded.0),
                display,
                "a 239:100 DAR over {coded:?} displays {display}, as base did"
            );
            // The trap this exists to hold, measured rather than described: the
            // same DAR derives pairs that reduce on *both* sides of the bound,
            // so a term bound judges the same ratio differently depending only
            // on the frame it happens to sit on.
            let (mut a, mut b) = (derived.h, derived.v);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            let reduced = (derived.h / a, derived.v / a);
            let past = reduced.0 > PixelAspect::MAX_ASPECT_TERM;
            assert_eq!(
                past,
                coded.0 != 720,
                "{coded:?}: a 239:100 DAR derives {reduced:?}, {} the {} bound -- \
                 the same ratio, on both sides of it",
                if past { "past" } else { "inside" },
                PixelAspect::MAX_ASPECT_TERM
            );
        }
        // And the 1440x1080 frame -- the one this lane's own fixture sits on --
        // reduces to 717:400, which the bound admits. So the same ratio is
        // refused on two frames and accepted on a third, which is why the
        // bound cannot be tuned instead of replaced: any threshold catching
        // 2151:1600 would be chosen by the frame rather than by the picture.
        let over_1440 = PixelAspect::stated_ratio(239, 100, (1440, 1080)).expect("a picture");
        let (mut a, mut b) = (over_1440.h, over_1440.v);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        assert!(
            over_1440.h / a <= PixelAspect::MAX_ASPECT_TERM,
            "the same DAR over a 1440x1080 frame reduces to {}:{}, which the \
             bound admits -- the same ratio, judged either way by its frame",
            over_1440.h / a,
            over_1440.v / a
        );
    }

    #[test]
    fn rejects_misparse() {
        assert!(append_annex_b(&[0, 0, 0, 9, 0x65], 4, &mut Vec::new()).is_err());
        assert!(append_annex_b(&[0, 0, 0], 4, &mut Vec::new()).is_err());
        assert!(append_annex_b(&[0, 0, 0, 0], 4, &mut Vec::new()).is_err());
    }
}

#[cfg(test)]
mod container_names {
    use super::{container_from_brand, matroska_doctype};

    #[test]
    fn an_ftyp_brand_names_its_mp4_family_member() {
        assert_eq!(container_from_brand(b"qt  "), "mov");
        assert_eq!(container_from_brand(b"M4A "), "m4a");
        assert_eq!(container_from_brand(b"M4V "), "m4v");
        assert_eq!(container_from_brand(b"3gp5"), "3gp");
        assert_eq!(container_from_brand(b"isom"), "mp4");
        assert_eq!(container_from_brand(b"mp42"), "mp4");
    }

    #[test]
    fn a_matroska_doctype_is_read_from_the_ebml_header() {
        // EBML header with a DocType element: id 42 82, size 0x84, "webm".
        let webm = [
            0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x82, 0x84, b'w', b'e', b'b', b'm',
        ];
        assert_eq!(matroska_doctype(&webm), Some("webm"));
        let mkv = [
            0x1A, 0x45, 0xDF, 0xA3, 0x8B, 0x42, 0x82, 0x88, b'm', b'a', b't', b'r', b'o', b's',
            b'k', b'a',
        ];
        assert_eq!(matroska_doctype(&mkv), Some("matroska"));
    }

    #[test]
    fn a_doctype_that_is_not_there_or_cut_short_is_none() {
        assert_eq!(matroska_doctype(&[0x1A, 0x45, 0xDF, 0xA3]), None);
        // The size claims four bytes and the file ends after two.
        assert_eq!(matroska_doctype(&[0x42, 0x82, 0x84, b'w', b'e']), None);
    }
}
