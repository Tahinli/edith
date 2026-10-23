//! The pieces of a ProRes export that are not the picture walk: the chroma
//! upsampling that takes the compositor's 4:2:0 to the 4:2:2 / 4:4:4 a ProRes
//! profile codes, and the QuickTime `.mov` writer.
//!
//! # Chroma upsampling
//!
//! The compositor and every decoder here hand the export 4:2:0 (8-bit, or 10-bit
//! where the span passes a 10-bit source through untouched). ProRes 422 codes
//! 4:2:2 and ProRes 4444 codes 4:4:4, so the chroma is upsampled -- not by
//! duplicating samples, which would double every chroma edge into a blocky
//! staircase, but by the linear filter the siting of 4:2:0 asks for:
//!
//! * vertically, 4:2:0 chroma sits half way between two luma rows
//!   (MPEG-2 / H.264 "left" siting), so the two luma rows of a chroma row take
//!   `3/4 * own + 1/4 * neighbour` (the row above for the upper, the row below
//!   for the lower), edges clamped;
//! * horizontally (4:4:4 only), chroma is co-sited with the even luma column, so
//!   even columns copy and odd columns average their two neighbours.
//!
//! An 8-bit picture is widened first to 10 bits with the same `x << 2 | x >> 6`
//! scale the HEVC and AV1 seats use (255 maps to 1023), so the encoder always
//! sees 10-bit samples and the filter runs at 10-bit precision. Nothing here can
//! recover chroma detail the 4:2:0 picture never had: a ProRes 422 file of a
//! 4:2:0 timeline is a 4:2:2 *container* of 4:2:0 information.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, MediaParameters, StreamInfo};
use ec_core::{ChannelLayout, Muxer as _, Packet, TimeBase};

/// Little-endian bytes of 16-bit samples.
fn le_bytes(samples: &[u16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// An 8-bit sample at 10 bits: the same scale as the HEVC/AV1 widening.
fn widen(s: u8) -> u16 {
    u16::from(s) << 2 | u16::from(s) >> 6
}

/// Vertical 4:2:0 -> 4:2:2: a `cw` x `ch` chroma plane to `cw` x `h` rows.
fn up_vertical(src: &[u16], cw: usize, ch: usize, h: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(cw * h);
    for y in 0..h {
        let own = (y / 2).min(ch - 1);
        let other = if y % 2 == 0 { own.saturating_sub(1) } else { (own + 1).min(ch - 1) };
        let (a, b) = (&src[own * cw..own * cw + cw], &src[other * cw..other * cw + cw]);
        out.extend(a.iter().zip(b).map(|(&a, &b)| ((3 * u32::from(a) + u32::from(b) + 2) >> 2) as u16));
    }
    out
}

/// Horizontal 4:2:2 -> 4:4:4: `cw` columns to `w`, even columns copied and odd
/// columns the mean of their two neighbours.
fn up_horizontal(src: &[u16], cw: usize, rows: usize, w: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(w * rows);
    for r in 0..rows {
        let line = &src[r * cw..r * cw + cw];
        for x in 0..w {
            let i = (x / 2).min(cw - 1);
            out.push(if x % 2 == 0 {
                line[i]
            } else {
                ((u32::from(line[i]) + u32::from(line[(i + 1).min(cw - 1)]) + 1) >> 1) as u16
            });
        }
    }
    out
}

/// Planes of an `w` x `h` 4:2:0 picture at 10 bits as the 4:2:2 (`I210`) or
/// 4:4:4 (`I410`) frame the encoder takes.
pub(crate) fn upsampled_frame(
    y: &[u16],
    u: &[u16],
    v: &[u16],
    w: usize,
    h: usize,
    full_chroma: bool,
) -> crate::Result<VideoFrame> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let chroma = |p: &[u16]| {
        let tall = up_vertical(&p[..cw * ch], cw, ch, h);
        if full_chroma { up_horizontal(&tall, cw, h, w) } else { tall }
    };
    let (cb, cr) = (chroma(u), chroma(v));
    let cstride = if full_chroma { w } else { cw } * 2;
    let planes = vec![
        Plane::new(le_bytes(&y[..w * h]), w * 2),
        Plane::new(le_bytes(&cb), cstride),
        Plane::new(le_bytes(&cr), cstride),
    ];
    let format = if full_chroma { PixelFormat::I410 } else { PixelFormat::I210 };
    VideoFrame::try_new(format, w as u32, h as u32, planes)
        .map_err(|e| format!("ProRes frame: {e}").into())
}

/// [`upsampled_frame`] from 8-bit planes, widened to 10 bits first.
pub(crate) fn upsampled_frame_8(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    w: usize,
    h: usize,
    full_chroma: bool,
) -> crate::Result<VideoFrame> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let wide = |p: &[u8], n: usize| p[..n].iter().map(|&s| widen(s)).collect::<Vec<u16>>();
    upsampled_frame(&wide(y, w * h), &wide(u, cw * ch), &wide(v, cw * ch), w, h, full_chroma)
}

/// PCM frames per packet the sound is written in: about a second at 48 kHz.
const PCM_CHUNK: usize = 48_000;

/// A QuickTime `.mov`: one ProRes picture track and, where the timeline has
/// sound, one 16-bit little-endian PCM track (`sowt`).
pub(crate) struct MovWriter {
    writer: ec_mp4::Mp4Muxer<BufWriter<File>>,
    stream: u32,
    tb: TimeBase,
    ticks: i64,
    frames: i64,
}

impl MovWriter {
    /// Opens `out`. `params` are the encoder's codec parameters (the sample-entry
    /// fourcc rides in its extradata); `(timescale, ticks)` is the frame clock
    /// ([`crate::mux::frame_timing`]).
    pub(crate) fn create(out: &Path, params: &CodecParameters, (timescale, ticks): (u32, u32)) -> crate::Result<Self> {
        let file = BufWriter::new(File::create(out)?);
        let mut writer = ec_mp4::Mp4Muxer::with_brands(file, *b"qt  ", &[*b"qt  "]).map_err(crate::demux::mp4_error)?;
        let tb = TimeBase::new(1, i64::from(timescale));
        let mut info = StreamInfo::new(0, tb, params.clone());
        info.default = true;
        let stream = writer.add_stream(info).map_err(crate::demux::mp4_error)?;
        Ok(Self { writer, stream, tb, ticks: i64::from(ticks), frames: 0 })
    }

    /// One coded frame.
    pub(crate) fn write_frame(&mut self, bytes: &[u8]) -> crate::Result<()> {
        let mut p = Packet::new(self.stream, self.tb, bytes.to_vec());
        p.pts = Some(self.frames * self.ticks);
        p.dts = p.pts;
        p.duration = Some(self.ticks);
        p.flags.keyframe = true;
        self.writer.write_packet(&p).map_err(crate::demux::mp4_error)?;
        self.frames += 1;
        Ok(())
    }

    /// The sound: interleaved `bits`-wide (16 or 24) samples in `i32`s, written after the picture in
    /// packets of about a second. `frames` PCM frames per channel are written
    /// exactly (the caller has already padded or trimmed to the picture's length).
    pub(crate) fn write_sound(&mut self, samples: &[i32], rate: u32, channels: u16, bits: u8) -> crate::Result<()> {
        let channels = usize::from(channels.max(1));
        // 24-bit is big-endian `in24` (ffmpeg `pcm_s24be`), what a ProRes master carries.
        let mut params = CodecParameters::new(if bits == 16 { CodecId::PcmS16Le } else { CodecId::PcmS24Be });
        params.media = MediaParameters::Audio(AudioParameters {
            sample_rate: rate,
            layout: ChannelLayout::from_count(channels),
            ..Default::default()
        });
        let tb = TimeBase::from_rate(rate);
        let stream = self.writer.add_stream(StreamInfo::new(1, tb, params)).map_err(crate::demux::mp4_error)?;
        let mut at = 0i64;
        for chunk in samples.chunks(PCM_CHUNK * channels) {
            let bytes: Vec<u8> = if bits == 16 {
                chunk.iter().flat_map(|&s| (s.clamp(-32768, 32767) as i16).to_le_bytes()).collect()
            } else {
                chunk
                    .iter()
                    .flat_map(|&s| {
                        let b = s.clamp(-8_388_608, 8_388_607).to_be_bytes();
                        [b[1], b[2], b[3]]
                    })
                    .collect()
            };
            let n = (chunk.len() / channels) as i64;
            let mut p = Packet::new(stream, tb, bytes);
            p.pts = Some(at);
            p.dts = Some(at);
            p.duration = Some(n);
            p.flags.keyframe = true;
            self.writer.write_packet(&p).map_err(crate::demux::mp4_error)?;
            at += n;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> crate::Result<()> {
        if self.frames == 0 {
            return Err("export produced no coded pictures".into());
        }
        self.writer.finish().map_err(crate::demux::mp4_error)?;
        self.writer.into_inner().flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(plane: &Plane) -> Vec<u16> {
        plane.data.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect()
    }

    #[test]
    fn flat_chroma_stays_flat_in_both_upsamplings() {
        let (w, h) = (6, 4);
        let y = vec![500u16; w * h];
        let c = vec![600u16; (w / 2) * (h / 2)];
        for full in [false, true] {
            let f = upsampled_frame(&y, &c, &c, w, h, full).unwrap();
            assert!(le(&f.planes[1]).iter().all(|&s| s == 600), "full={full}");
        }
    }

    #[test]
    fn vertical_filter_is_three_quarters_own_plus_a_quarter_neighbour() {
        // Two chroma rows, 400 and 800: rows come out 400, 500, 700, 800 (edges clamp).
        let up = up_vertical(&[400, 800], 1, 2, 4);
        assert_eq!(up, vec![400, 500, 700, 800]);
    }

    #[test]
    fn horizontal_filter_copies_even_and_averages_odd() {
        let up = up_horizontal(&[100, 300], 2, 1, 4);
        assert_eq!(up, vec![100, 200, 300, 300]);
    }

    #[test]
    fn eight_bit_white_widens_to_1023() {
        assert_eq!(widen(255), 1023);
        assert_eq!(widen(0), 0);
    }
}
