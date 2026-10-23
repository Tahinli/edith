//! MPEG program streams -- `.mpg`, `.mpeg`, `.vob`, `.m2p`, a camcorder's `.mod`/`.tod`
//! -- and the raw MPEG-1/2 video streams `.m1v`, `.m2v`, `.mpv`, over the family's own
//! [`ec_mpegts::PsDemuxer`].
//!
//! A program stream is read by the very readers a transport stream is ([`super::ts`]):
//! [`ts::TsDemuxer`](super::ts::TsDemuxer) for the picture, [`ts::TsAudio`](super::ts::TsAudio)
//! for each sound track, the same time model (source time 0 is the first picture's earliest
//! presentation time), the same refusal wording. What differs lives here: how a file is
//! recognised, how its container is named, and what is said about a track that is refused.
//!
//! Picture: MPEG-1 and MPEG-2 video (the `ec-mpeg2` software seat, deinterlace honoured),
//! H.264 and HEVC. Sound: MPEG audio layers I-III, AC-3 (also a DVD's), DVD LPCM at 16, 20
//! and 24 bits. Refused by name, with the ffmpeg line: DTS, DVD subtitles (VobSub) and any
//! other private-stream substream.
//!
//! **Known limits.** A raw `.m2v` has no index, so `ec-mpegts` reads the whole file once at
//! open: the picture is opened on the thread that asks, so a UI that opens a clip should
//! do it off its render thread (every open in the engine already is). 20 and 24 bit DVD LPCM
//! may seek to the middle of a two-frame group (a click, not drift). A `.mod` is only taken for
//! a program stream if its bytes are one: the tracker-module spelling is never claimed.

use super::*;

/// The extensions that name a program stream or a raw MPEG video stream outright.
const OWN_EXTENSIONS: &[&str] = &["m1v", "m2v", "mpv", "vob", "m2p", "tod"];

/// `.mpg` and `.mpeg` are also the names of bare MPEG audio, and `.mod` of a tracker module:
/// only the content decides these.
const SHARED_EXTENSIONS: &[&str] = &["mpg", "mpeg", "mod"];

pub(crate) fn ps_error(e: ec_core::Error) -> crate::Error {
    format!("MPEG program stream: {e}").into()
}

fn head_of(path: &Path, len: usize) -> Vec<u8> {
    let mut head = vec![0u8; len];
    let read = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    head.truncate(read);
    head
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Whether `path` is an MPEG program stream or a raw MPEG video stream: its extension
/// where that is unambiguous, its bytes otherwise (any name at all, `.mpg`/`.mpeg`/`.mod`
/// included). A container whose magic is Matroska's or an mp4's, or a transport stream, is
/// never one.
pub fn is_mpegps(path: &Path) -> bool {
    if sniff_container(path).is_some() {
        return false;
    }
    let ext = extension(path);
    let ext = ext.as_deref();
    if matches!(ext, Some("ts" | "mts" | "m2ts" | "m2t" | "tp" | "trp")) {
        return false;
    }
    if ext.is_some_and(|e| OWN_EXTENSIONS.contains(&e)) {
        return true;
    }
    let shared = ext.is_some_and(|e| SHARED_EXTENSIONS.contains(&e));
    if !shared && (crate::is_audio(path) || crate::is_image(path)) {
        return false;
    }
    let head = head_of(path, 8 * 1024);
    ec_mpegts::is_mpegps(&head) || ec_mpegts::is_mpeg_video_es(&head)
}

/// Whether `path` is a transport stream, a program stream, an AVI or a raw DV file: the files [`ts`] reads.
pub fn is_mpeg_system(path: &Path) -> bool {
    ts::is_mpegts(path) || is_mpegps(path) || super::avi::is_avi(path) || super::dv::is_dv(path)
}

/// Whether an `.mpg`/`.mpeg` file carries a *picture*, so that it belongs on a video lane and
/// not with the MPEG audio streams those spellings also name. Looks at the first 64 KiB for a
/// program stream with a video PES (or a raw video stream); a file that does not exist, or is
/// anything else, is not one.
pub fn carries_picture(path: &Path) -> bool {
    let head = head_of(path, 64 * 1024);
    if ec_mpegts::is_mpeg_video_es(&head) {
        return true;
    }
    ec_mpegts::is_mpegps(&head)
        && head
            .windows(4)
            .any(|w| w[..3] == [0, 0, 1] && (0xE0..=0xEF).contains(&w[3]))
}

/// ffprobe's name for the container: `mpeg` for a program stream, `mpegvideo` for a raw
/// video stream; the camcorder's and the disc's own spellings are kept.
pub(crate) fn container_name(path: &Path, ext: Option<&str>) -> String {
    match ext {
        Some("vob" | "mod" | "tod" | "m2p") => ext.unwrap_or_default().into(),
        _ => match ec_mpegts::is_mpegps(&head_of(path, 8 * 1024)) {
            true => "mpeg".into(),
            false => "mpegvideo".into(),
        },
    }
}
