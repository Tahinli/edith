//! The software VP8 and VP9 seats on the owner's own files, frame for frame
//! against ffmpeg's `rawvideo` output of the same file. Every shown picture is
//! cropped to the display size, serialised as 8-bit I420 (Y, then U, then V,
//! chroma at `div_ceil(2)`), and compared byte for byte with the next frame ffmpeg
//! writes -- a byte-identical frame has an identical MD5. Frame count must agree
//! too, and ffmpeg must have nothing left over.
//!
//! Named by `real_library.toml` (gitignored): `vp8_webm` and `vp9_opus_mp4`.
//! Skips where the key is absent or ffmpeg is not installed.

use crate::reallib;

use std::io::Read;
use std::path::Path;

use ec_vp8::decode::Decoder as Vp8Decoder;
use ec_vp9::decode::Decoder as Vp9Decoder;
use engine::demux::Demuxer;
use reallib::{ffmpeg_pipe, fill, have};

/// The display-size I420 bytes of one picture, given its plane rows.
fn i420<T: Copy>(
    y: &[T],
    u: &[T],
    v: &[T],
    (w, h): (usize, usize),
    (stride, uv_stride): (usize, usize),
) -> Vec<u8>
where
    u16: From<T>,
{
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = Vec::with_capacity(w * h + 2 * cw * ch);
    let mut plane = |src: &[T], pitch: usize, pw: usize, ph: usize| {
        for row in 0..ph {
            out.extend(
                src[row * pitch..row * pitch + pw]
                    .iter()
                    .map(|s| u16::from(*s) as u8),
            );
        }
    };
    plane(y, stride, w, h);
    plane(u, uv_stride, cw, ch);
    plane(v, uv_stride, cw, ch);
    out
}

/// Decodes every shown frame of the file with `next`, comparing each against
/// ffmpeg's `rawvideo` output as it arrives. Returns the frame count.
fn compare_with_ffmpeg(
    key: &str,
    path: &Path,
    mut next: impl FnMut() -> Option<(Vec<u8>, (usize, usize))>,
) -> usize {
    let (mut child, mut ff) = ffmpeg_pipe(
        &[
            "-map",
            "0:v:0",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-fps_mode",
            "passthrough",
            "-",
        ],
        path,
    );
    let mut frames = 0usize;
    let mut want = Vec::new();
    while let Some((got, (w, h))) = next() {
        let size = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
        want.resize(size, 0);
        let n = fill(&mut ff, &mut want);
        assert_eq!(
            n, size,
            "{key}: ffmpeg ended before frame {frames} ({n} of {size} bytes)"
        );
        assert!(
            got == want,
            "{key}: frame {frames} ({w}x{h}) differs from ffmpeg's rawvideo"
        );
        frames += 1;
    }
    let mut rest = Vec::new();
    ff.read_to_end(&mut rest).expect("drain ffmpeg");
    assert!(
        rest.is_empty(),
        "{key}: ffmpeg has {} bytes of frames ec did not decode",
        rest.len()
    );
    let status = child.wait().expect("wait for ffmpeg");
    assert!(status.success(), "{key}: ffmpeg failed");
    frames
}

fn vp8_frames(path: &Path) -> impl FnMut() -> Option<(Vec<u8>, (usize, usize))> {
    let (_, mut demuxer) = Demuxer::open(path).expect("open the VP8 file");
    let mut decoder = Vp8Decoder::new();
    move || loop {
        let au = demuxer.next_access_unit().expect("demux")?;
        if let Some(pic) = decoder.decode(&au).expect("ec-vp8 decode") {
            let dims = (usize::from(pic.width), usize::from(pic.height));
            let bytes = i420(&pic.y, &pic.u, &pic.v, dims, (pic.stride, pic.uv_stride));
            return Some((bytes, dims));
        }
    }
}

fn vp9_frames(path: &Path) -> impl FnMut() -> Option<(Vec<u8>, (usize, usize))> {
    let (_, mut demuxer) = Demuxer::open(path).expect("open the VP9 file");
    let mut decoder = Vp9Decoder::new();
    move || loop {
        let au = demuxer.next_access_unit().expect("demux")?;
        if let Some(pic) = decoder.decode(&au).expect("ec-vp9 decode") {
            let dims = (usize::from(pic.width), usize::from(pic.height));
            let bytes = i420(&pic.y, &pic.u, &pic.v, dims, (pic.stride, pic.uv_stride));
            return Some((bytes, dims));
        }
    }
}

/// The VP8 WebM: a 2560x1321 VP8 stream in Matroska, odd height so the chroma
/// rounding is exercised.
#[test]
fn real_vp8_webm_frames_match_ffmpeg() {
    let Some(path) = engine::real_library::film("vp8_webm") else {
        return;
    };
    if !have("ffmpeg") {
        eprintln!("skipped: no ffmpeg to decode the reference frames with");
        return;
    }
    let frames = compare_with_ffmpeg("vp8_webm", &path, vp8_frames(&path));
    eprintln!("vp8_webm: {frames} frames byte-identical to ffmpeg's rawvideo");
    assert!(frames > 0, "no frames decoded");
}

/// The VP9 Profile 0 stream in mp4, with Opus beside it (the audio is not this
/// test's concern).
#[test]
fn real_vp9_opus_mp4_frames_match_ffmpeg() {
    let Some(path) = engine::real_library::film("vp9_opus_mp4") else {
        return;
    };
    if !have("ffmpeg") {
        eprintln!("skipped: no ffmpeg to decode the reference frames with");
        return;
    }
    let frames = compare_with_ffmpeg("vp9_opus_mp4", &path, vp9_frames(&path));
    eprintln!("vp9_opus_mp4: {frames} frames byte-identical to ffmpeg's rawvideo");
    assert!(frames > 0, "no frames decoded");
}
