//! The raw video software seat: uncompressed pictures in an AVI (`BI_RGB` 8 bit palette, RGB555,
//! 24 and 32 bit, YUY2, UYVY, YVYU, I420, YV12, NV12, Y800, v210) or a QuickTime mov (`raw `,
//! `2vuy`, `yuvs`, `yuv2`, `v210`, `v410`, `b16g`), unpacked and flipped upright by `ec-riff`'s [`RawVideoDecoder`] and handed
//! to the shared intra-only stream and span loop ([`super::intra`]). Every picture is a sync
//! sample, so a seek is one sample read.
//!
//! YUV layouts keep their own chroma (4:2:2 averaged down to 4:2:0 as every 4:2:2 source is) and
//! are untagged like the rest of an AVI's pictures. RGB is turned into 4:4:4 YCbCr with the
//! BT.709 matrix over the full range at 8 bit, and tagged as exactly that, so the one conversion
//! back to RGB every source takes undoes it (to within a code or two, the chroma being averaged
//! down to 4:2:0 on the way, as for any 4:4:4 source). The alpha of a 32-bit DIB is dropped: the
//! engine composites a clip as opaque.

use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_core::registry::{CodecId, CodecParameters};
use ec_riff::RawVideoDecoder;

use super::intra::{IntraFacts, Seat};

pub(crate) struct RawSeat(pub(super) RawVideoDecoder);

impl Seat for RawSeat {
    const NAME: &'static str = "raw video";
    fn open(_: IntraFacts, header: &[u8]) -> Result<Self, String> {
        let mut params = CodecParameters::new(CodecId::RawVideo);
        params.extradata = Some(header.to_vec().into());
        RawVideoDecoder::new(params).map(Self).map_err(|e| e.to_string())
    }
    fn decode(&mut self, sample: &[u8]) -> Result<VideoFrame, String> {
        self.0.decode(sample).map(yuv).map_err(|e| e.to_string())
    }
}

pub(crate) type Stream = super::intra::Stream<RawSeat>;

/// An upright picture as the planar YCbCr the engine takes: RGB (any of `Bgr8`, `Rgb8`, `Bgra8`)
/// becomes full-range BT.709 4:4:4, NV12 is split into I420, 16-bit gray is narrowed to 10-bit
/// (`I010`, neutral chroma), and every other layout is left as it is (`Gray8` is given neutral
/// chroma by the shared fold, 4:2:2 / 4:4:4 at 10 bit are carried as they are).
pub(crate) fn yuv(pic: VideoFrame) -> VideoFrame {
    let (bpp, swap) = match pic.format {
        PixelFormat::Bgr8 => (3, false),
        PixelFormat::Rgb8 => (3, true),
        PixelFormat::Bgra8 => (4, false),
        PixelFormat::Nv12 => return nv12(&pic),
        PixelFormat::Gray16 => return gray16(&pic),
        _ => return pic,
    };
    let (w, h) = (pic.width as usize, pic.height as usize);
    let src = &pic.planes[0];
    let (mut y, mut u, mut v) = (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
    for row in 0..h {
        let line = &src.data[row * src.stride..row * src.stride + w * bpp];
        for (col, px) in line.chunks_exact(bpp).enumerate() {
            let (b, g, r) = if swap {
                (i32::from(px[2]), i32::from(px[1]), i32::from(px[0]))
            } else {
                (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]))
            };
            let at = row * w + col;
            let luma = (13933 * r + 46871 * g + 4732 * b + 32768) >> 16;
            let cb = ((-7509 * r - 25259 * g + 32768 * b + 32768) >> 16) + 128;
            let cr = ((32768 * r - 29764 * g - 3004 * b + 32768) >> 16) + 128;
            y[at] = luma.clamp(0, 255) as u8;
            u[at] = cb.clamp(0, 255) as u8;
            v[at] = cr.clamp(0, 255) as u8;
        }
    }
    let planes = vec![Plane::new(y, w), Plane::new(u, w), Plane::new(v, w)];
    let mut out = VideoFrame::try_new(PixelFormat::I444, pic.width, pic.height, planes).expect("three planes of the picture's size");
    out.pts = pic.pts;
    out
}

/// NV12 (luma, then interleaved CbCr) as I420.
fn nv12(pic: &VideoFrame) -> VideoFrame {
    let (w, h) = (pic.width as usize, pic.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let uv = &pic.planes[1];
    let (mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for row in 0..ch {
        for pair in uv.data[row * uv.stride..row * uv.stride + 2 * cw].as_chunks::<2>().0 {
            u.push(pair[0]);
            v.push(pair[1]);
        }
    }
    let planes = vec![pic.planes[0].clone(), Plane::new(u, cw), Plane::new(v, cw)];
    let mut out = VideoFrame::try_new(PixelFormat::I420, pic.width, pic.height, planes).expect("three planes of the picture's size");
    out.pts = pic.pts;
    out
}

/// 16-bit little-endian gray as `I010`: luma rounded down to 10 bit, chroma neutral (512).
fn gray16(pic: &VideoFrame) -> VideoFrame {
    let (w, h) = (pic.width as usize, pic.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let src = &pic.planes[0];
    let mut luma = Vec::with_capacity(w * h * 2);
    for row in 0..h {
        for px in src.data[row * src.stride..row * src.stride + 2 * w].as_chunks::<2>().0 {
            let v = (u32::from(u16::from_le_bytes([px[0], px[1]])) + 32) >> 6;
            luma.extend_from_slice(&(v.min(1023) as u16).to_le_bytes());
        }
    }
    let neutral: Vec<u8> = std::iter::repeat_n(512u16.to_le_bytes(), cw * ch).flatten().collect();
    let planes = vec![Plane::new(luma, 2 * w), Plane::new(neutral.clone(), 2 * cw), Plane::new(neutral, 2 * cw)];
    let mut out = VideoFrame::try_new(PixelFormat::I010, pic.width, pic.height, planes).expect("three planes of the picture's size");
    out.pts = pic.pts;
    out
}
