//! The encode seats, on [`ec_hw::Encoder`]: H.264 (Main profile), HEVC and AV1.
//!
//! A picture reaches the encoder one of two ways. Pixels the caller holds are
//! packed I420 and go through [`ec_hw::Encoder::encode`], which uploads them
//! into a pooled surface. A picture the decoder left on the GPU arrives as a
//! DRM-PRIME descriptor ([`DmaFrame`]); its file descriptors are duplicated into
//! an [`ec_va::PrimeSurface`], imported with [`ec_va::import_prime`]
//! and read in place by [`ec_hw::Encoder::encode_surface`], so no copy is made.

use std::collections::VecDeque;
use std::os::fd::{AsFd, OwnedFd};
use std::rc::Rc;

use ec_hw::enc::{EncCodec, EncoderConfig, FrameMetadata, RateControlMode};
use ec_hw::{Encoder, H264Profile, I420};
use ec_va::{PrimeLayer, PrimeObject, PrimeSurface};

use engine::hw::{DmaFrame, VhDma};

/// `DRM_FORMAT_NV12`, which is also `VA_FOURCC_NV12`: the four characters.
const FOURCC_NV12: u32 = u32::from_le_bytes(*b"NV12");

/// One hardware encode session on `ec_hw`.
pub(crate) struct EcEncSession {
    display: Rc<ec_va::Display>,
    encoder: Encoder,
    /// Display size: what is encoded.
    width: usize,
    height: usize,
    /// Macroblock- or block-aligned size the driver codes at.
    coded: (u32, u32),
    timestamp: i64,
    /// Access units the encoder has finished but the caller has not taken yet,
    /// in encode order (which is display order: no B-frames).
    ready: VecDeque<Vec<u8>>,
    /// The access unit currently handed out, kept alive until the next call.
    out: Vec<u8>,
}

/// The `open` arguments, for any of the three codecs.
pub(crate) struct EcOpen {
    pub codec: EncCodec,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub bitrate: u64,
    /// Every picture an IDR: what a proxy is coded with (see `gop_limit`).
    pub intra_only: bool,
    /// Colour description for the VUI as `(primaries, transfer, matrix, full)`.
    pub colour: Option<(u8, u8, u8, bool)>,
}

impl EcEncSession {
    pub(crate) fn open(req: EcOpen) -> Option<Self> {
        let EcOpen {
            codec,
            width,
            height,
            fps_num,
            fps_den,
            bitrate,
            intra_only,
            colour,
        } = req;
        // NV12 chroma is half resolution, so odd dimensions have no packing;
        // radeonsi refuses encode contexts below 64x64 (measured).
        if width < 64 || height < 64 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return None;
        }
        // The HEVC entrypoint's own floor: the same driver that takes a
        // 128x128 H.264 context answers `VaError(19)` to an HEVC one below
        // 384x384 (measured 2026-08-13 on radeonsi).
        if codec == EncCodec::H265 && (width < 384 || height < 384) {
            return None;
        }
        if fps_num == 0 || fps_den == 0 || bitrate == 0 {
            return None;
        }
        let framerate = ((fps_num as f64 / fps_den as f64).round() as i64).clamp(1, 240) as u32;
        let mut config = EncoderConfig::new(codec, width, height);
        config.framerate = (framerate, 1);
        config.bitrate = u32::try_from(bitrate).unwrap_or(u32::MAX);
        config.gop_size = match codec {
            // Every HEVC picture is an IDR: a GOP's P slices come out of this
            // driver undecodable (measured 2026-08-13 on radeonsi), so the seat
            // is intra-only exactly as the software one is.
            EncCodec::H265 => 1,
            _ => u32::from(crate::gop_limit(framerate, intra_only)),
        };
        config.rate_control = match codec {
            // Constant quality at 128 of 255, the AV1 rate control measured.
            EncCodec::Av1 => RateControlMode::ConstantQp { qp: 128 },
            _ => RateControlMode::ConstantBitrate,
        };
        if codec == EncCodec::H264 {
            // Main, not the High default: the seat's tests pin the profile.
            config = config.h264_profile(H264Profile::Main);
        }
        if let Some((p, t, m, full)) = colour {
            config = config.colour(p, t, m, full);
        }
        let display = ec_va::open().ok()?;
        let encoder = Encoder::new(&display, config).ok()?;
        let coded = encoder.coded_size();
        Some(Self {
            display,
            encoder,
            width: width as usize,
            height: height as usize,
            coded,
            timestamp: 0,
            ready: VecDeque::new(),
            out: Vec::new(),
        })
    }

    /// The surface size the driver codes at, as the caller must describe it.
    pub(crate) fn coded_size(&self) -> (u32, u32) {
        self.coded
    }

    /// Codes one packed I420 picture (`y`, `u`, `v` at the session's size) and
    /// queues the access unit it finishes.
    pub(crate) fn encode(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        force_keyframe: bool,
    ) -> Result<(), String> {
        let (w, h) = (self.width, self.height);
        let (cw, ch) = (w / 2, h / 2);
        if y.len() < w * h || u.len() < cw * ch || v.len() < cw * ch {
            return Err("encode input is not a packed I420 frame".into());
        }
        let picture = I420 {
            y: y[..w * h].to_vec(),
            u: u[..cw * ch].to_vec(),
            v: v[..cw * ch].to_vec(),
            width: w as u32,
            height: h as u32,
        };
        let meta = self.meta(force_keyframe);
        let coded = self
            .encoder
            .encode(&picture, meta)
            .map_err(|e| e.to_string())?;
        self.ready.push_back(coded.data);
        Ok(())
    }

    /// Codes one picture the decoder left on the GPU, read in place.
    ///
    /// The descriptors stay owned by `frame`: they are duplicated into the
    /// imported surface, which owns its copies until it is dropped at the end
    /// of this call.
    pub(crate) fn encode_dma(&mut self, frame: &DmaFrame, force_keyframe: bool) -> Result<(), String> {
        let prime = prime_from_vhdma(&frame.desc, &frame.fds)?;
        let surface = Rc::new(ec_va::import_prime(&self.display, &prime).map_err(|e| e.to_string())?);
        let meta = self.meta(force_keyframe);
        let coded = self
            .encoder
            .encode_surface(&surface, (self.width as u32, self.height as u32), meta)
            .map_err(|e| e.to_string())?;
        self.ready.push_back(coded.data);
        Ok(())
    }

    fn meta(&mut self, force_keyframe: bool) -> FrameMetadata {
        let meta = FrameMetadata {
            timestamp: self.timestamp,
            force_keyframe,
        };
        self.timestamp += 1;
        meta
    }

    /// The oldest finished access unit, or `None` when the encoder has nothing
    /// queued. It stays valid until the next call on this session.
    pub(crate) fn next_au(&mut self) -> Option<&[u8]> {
        let au = self.ready.pop_front()?;
        self.out = au;
        Some(&self.out)
    }
}

/// Builds the PRIME descriptor for an NV12 picture the decoder holds.
///
/// Luma and chroma are either two objects (two descriptors) or one object
/// holding both at their own offsets, as the decoder's descriptor says.
fn prime_from_vhdma(desc: &VhDma, fds: &[OwnedFd]) -> Result<PrimeSurface, String> {
    if fds.is_empty() || fds.len() > 2 || desc.fourcc != FOURCC_NV12 {
        return Err("DMA-BUF is not an NV12 picture on the GPU".into());
    }
    let (w, h) = (desc.coded_width, desc.coded_height);
    let rows = [h, h / 2];
    let two_objects = fds.len() == 2;
    let mut objects = Vec::with_capacity(fds.len());
    for fd in fds {
        objects.push(PrimeObject {
            fd: fd.as_fd().try_clone_to_owned().map_err(|e| e.to_string())?,
            size: 0,
            modifier: desc.modifier,
        });
    }
    // The size each object must cover, luma rows then chroma rows.
    for (plane, &plane_rows) in rows.iter().enumerate() {
        let index = if two_objects { plane } else { 0 };
        let end = desc.offset[plane] as u64 + u64::from(desc.stride[plane]) * u64::from(plane_rows);
        let size = &mut objects[index].size;
        *size = (*size).max(u32::try_from(end).map_err(|_| "DMA-BUF too large")?);
    }
    let layer = PrimeLayer {
        drm_format: FOURCC_NV12,
        planes: vec![
            (0, desc.offset[0], desc.stride[0]),
            (
                if two_objects { 1 } else { 0 },
                desc.offset[1],
                desc.stride[1],
            ),
        ],
    };
    Ok(PrimeSurface {
        fourcc: FOURCC_NV12,
        width: w,
        height: h,
        objects,
        layers: vec![layer],
    })
}
