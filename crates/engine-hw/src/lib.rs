//! VA-API decode (H.264 and VP9) and H.264 encode, shipped as a `dlopen`-able
//! plugin so the main binary never gets a DT_NEEDED on libva/gbm/drm. Every
//! entry point is
//! `extern "C"`, catches unwinds and reports failure as a null pointer or a
//! negative code: the caller's contract is "any failure means use the software
//! codec".
//!
//! ponytail: that guarantee rests on `panic = "unwind"`. Building this crate with
//! `panic = "abort"` would turn a driver bug into a killed app; the upgrade path
//! is running the decoder in a child process instead of in-process.

use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use cros_codecs::backend::vaapi::decoder::VaapiBackend;
use cros_codecs::backend::vaapi::encoder::VaapiBackend as VaapiEncBackend;
use cros_codecs::codec::h264::parser::{Level, Profile as H264Profile};
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::vp9::Vp9;
use cros_codecs::decoder::stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder};
use cros_codecs::decoder::{BlockingMode, DecodedHandle, DecoderEvent, StreamInfo};
use cros_codecs::encoder::h264::EncoderConfig as H264Config;
use cros_codecs::encoder::stateless::h264::StatelessEncoder;
use cros_codecs::encoder::{
    FrameMetadata, PredictionStructure, RateControl, Tunings, VideoEncoder,
};
use cros_codecs::image_processing::nv12_to_i420;
use cros_codecs::libva::{
    Display, Image, Surface, VA_FOURCC_NV12, VAEntrypoint, VAImageFormat, VAProfile,
};
use cros_codecs::video_frame::VideoFrame;
use cros_codecs::video_frame::frame_pool::{FramePool, PooledVideoFrame};
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::video_frame::{UV_PLANE, Y_PLANE};
use cros_codecs::{Fourcc, FrameLayout, PlaneLayout, Resolution};
use gbm::{BufferObjectFlags, Format as GbmFormat};

use engine::demux::{Codec, Demuxer};
use engine::hw::{VhFrame, VhMeta};

type PooledFrame = PooledVideoFrame<GenericDmaVideoFrame>;
type Dec<C> = StatelessDecoder<C, VaapiBackend<PooledFrame>>;
type Handle = <Dec<H264> as StatelessVideoDecoder>::Handle;

/// One decoder per codec the demuxer can hand us. Both share the VA-API backend
/// and therefore the same [`Handle`], so everything past `decode` is common;
/// which one exists is decided by the container, never by the caller -- which is
/// why the plugin's C ABI is unchanged and an older `libengine_hw.so` still
/// loads and still decodes H.264 (it simply refuses VP9 files at `Demuxer`).
///
/// ponytail: VP9 profile 0 (8-bit 4:2:0) only. Profile 2 decodes 10-bit, which
/// the NV12 pool and the `vaGetImage` read-back here cannot carry; the upgrade
/// path is a P010 pool selected from the stream info.
enum Decoder {
    H264(Dec<H264>),
    Vp9(Dec<Vp9>),
}

impl Decoder {
    fn get(&mut self) -> &mut dyn StatelessVideoDecoder<Handle = Handle> {
        match self {
            Self::H264(d) => d,
            Self::Vp9(d) => d,
        }
    }
}

type Encoder = StatelessEncoder<
    GenericDmaVideoFrame,
    VaapiEncBackend<GenericDmaVideoFrame, Surface<GenericDmaVideoFrame>>,
>;

/// Ceiling on decode/drain iterations that make no progress. VA-API in blocking
/// mode should always either hand back an event or accept input; this only
/// exists so a misbehaving driver returns an error instead of hanging the app.
const STALL_LIMIT: u32 = 10_000;

struct Session {
    decoder: Decoder,
    /// NV12 descriptor for `vaGetImage`, queried once at open.
    nv12_format: VAImageFormat,
    pool: FramePool<GenericDmaVideoFrame>,
    demuxer: Demuxer,
    meta: VhMeta,
    ready: VecDeque<Handle>,
    /// Remaining bytes of the access unit currently being fed, if any.
    pending: Vec<u8>,
    timestamp: u64,
    flushed: bool,
    /// Pictures still to be decoded and thrown away to land on a seek target.
    skip: u32,
    /// Tightly packed I420 handed out through [`VhFrame`]; reused every frame,
    /// which is exactly why the pointers are only valid until the next call.
    out: Vec<u8>,
    luma: usize,
    chroma: usize,
}

/// Mesa's GBM cannot allocate an NV12 buffer object at all (measured on
/// radeonsi: every flag/modifier combination returns NULL), so we allocate one
/// linear single-plane R8 buffer big enough for luma + chroma and describe the
/// NV12 layout inside it ourselves. VA-API imports it as one DRM-PRIME object
/// with two planes at known offsets, which is exactly what the descriptor in
/// `GenericDmaVideoFrame` emits.
fn alloc_nv12(
    gbm: &gbm::Device<std::fs::File>,
    coded: Resolution,
) -> Result<GenericDmaVideoFrame, String> {
    let rows = coded.height + coded.height.div_ceil(2);
    let bo = gbm
        .create_buffer_object::<()>(coded.width, rows, GbmFormat::R8, BufferObjectFlags::LINEAR)
        .map_err(|e| format!("gbm R8 {}x{rows} allocation failed: {e}", coded.width))?;
    let pitch = bo.stride().map_err(|e| e.to_string())? as usize;
    let fd = bo.fd().map_err(|e| e.to_string())?;
    GenericDmaVideoFrame::new(vec![std::fs::File::from(fd)], nv12_layout(coded, pitch))
}

/// Two NV12 planes packed one after the other in a single buffer object.
fn nv12_layout(coded: Resolution, pitch: usize) -> FrameLayout {
    FrameLayout {
        format: (Fourcc::from(b"NV12"), 0 /* DRM_FORMAT_MOD_LINEAR */),
        size: coded,
        planes: vec![
            PlaneLayout {
                buffer_index: 0,
                offset: 0,
                stride: pitch,
            },
            PlaneLayout {
                buffer_index: 0,
                offset: pitch * coded.height as usize,
                stride: pitch,
            },
        ],
    }
}

/// First render node that gives us both a VA display and a GBM device. Which
/// node is the right one is a per-machine question, so we probe rather than
/// hardcode renderD128.
fn open_devices() -> Option<(Rc<Display>, gbm::Device<std::fs::File>)> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes.into_iter().find_map(|node| {
        let display = Display::open_drm_display(&node).ok()?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node)
            .ok()?;
        let gbm = gbm::Device::new(file).ok()?;
        Some((display, gbm))
    })
}

impl Session {
    fn open(path: &Path) -> Option<Self> {
        let (meta, demuxer) = Demuxer::open(path).ok()?;
        let (display, gbm) = open_devices()?;
        let nv12_format = display
            .query_image_formats()
            .ok()?
            .into_iter()
            .find(|f| f.fourcc == VA_FOURCC_NV12)?;
        let decoder = match meta.codec {
            Codec::H264 => {
                Decoder::H264(Dec::<H264>::new_vaapi(display, BlockingMode::Blocking).ok()?)
            }
            Codec::Vp9 => {
                Decoder::Vp9(Dec::<Vp9>::new_vaapi(display, BlockingMode::Blocking).ok()?)
            }
        };
        let pool = FramePool::new(move |info: &StreamInfo| {
            alloc_nv12(&gbm, info.coded_resolution).expect("output frame allocation failed")
        });
        Some(Self {
            decoder,
            nv12_format,
            pool,
            demuxer,
            meta: VhMeta {
                width: meta.width,
                height: meta.height,
                frame_rate: meta.frame_rate,
                frame_count: meta.frame_count,
            },
            ready: VecDeque::new(),
            pending: Vec::new(),
            timestamp: 0,
            flushed: false,
            skip: 0,
            out: Vec::new(),
            luma: 0,
            chroma: 0,
        })
    }

    /// As [`Session::open`], but positioned so the first picture handed out is
    /// sample `target_sample` (1-based): decode restarts at the sync sample at
    /// or before it and the pictures in between are dropped unread.
    fn open_at(path: &Path, target_sample: u32) -> Option<Self> {
        let mut session = Self::open(path)?;
        // The ABI still speaks 1-based sample ids, the demuxer speaks 0-based
        // display frames -- and answers with a signed one, since a sync sample
        // inside what the edit list trims sits before frame 0.
        let target_frame = target_sample.saturating_sub(1);
        let first = session.demuxer.seek_to_sync_at_or_before(target_frame);
        session.skip = (i64::from(target_frame) - first).max(0) as u32;
        Some(session)
    }

    /// Pumps the decoder until one picture is ready. `Ok(false)` is clean EOF.
    fn pump(&mut self) -> Result<bool, String> {
        let mut stalls = 0u32;
        loop {
            // Stop draining as soon as we have a picture: every handle we hold
            // keeps a pool buffer out of circulation.
            while self.ready.is_empty() {
                match self.decoder.get().next_event() {
                    Some(DecoderEvent::FrameReady(handle)) => self.ready.push_back(handle),
                    Some(DecoderEvent::FormatChanged) => {
                        let info = self
                            .decoder
                            .get()
                            .stream_info()
                            .ok_or("format changed without stream info")?
                            .clone();
                        self.pool.resize(&info);
                    }
                    None => break,
                }
            }
            if let Some(handle) = self.ready.pop_front() {
                // Seek discards: dropping the handle skips the `vaGetImage`
                // read-back (~1.2 ms/picture) and frees its pool buffer.
                if self.skip > 0 {
                    self.skip -= 1;
                    continue;
                }
                self.emit(handle);
                return Ok(true);
            }
            if self.flushed {
                return Ok(false);
            }

            if self.pending.is_empty() {
                match self.demuxer.next_access_unit() {
                    Ok(Some(au)) => self.pending = au,
                    Ok(None) => {
                        self.decoder.get().flush().map_err(|e| e.to_string())?;
                        self.flushed = true;
                        continue;
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }

            let Self {
                decoder,
                pool,
                pending,
                timestamp,
                ..
            } = self;
            match decoder
                .get()
                .decode(*timestamp, pending, &mut || pool.alloc())
            {
                Ok(0) => return Err("decoder consumed no input".into()),
                Ok(n) => {
                    pending.drain(..n.min(pending.len()));
                    if pending.is_empty() {
                        *timestamp += 1;
                    }
                    stalls = 0;
                }
                // Both mean "handle pending events, then resubmit the same data".
                Err(DecodeError::CheckEvents) | Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    stalls += 1;
                    if stalls > STALL_LIMIT {
                        return Err("decoder stalled waiting for output buffers".into());
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    /// Copies a decoded NV12 surface into `self.out` as packed I420.
    fn emit(&mut self, handle: Handle) {
        let res = handle.display_resolution();
        let (w, h) = (res.width as usize, res.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        if self.luma != w * h || self.chroma != cw * ch {
            self.luma = w * h;
            self.chroma = cw * ch;
            self.out = vec![0u8; self.luma + 2 * self.chroma];
        }
        self.meta.width = res.width;
        self.meta.height = res.height;

        // Read back with vaGetImage rather than mapping our own DMA-BUF: on a
        // discrete GPU that buffer lives in VRAM and a CPU read of it runs at
        // ~44 MB/s over the PCIe BAR (measured), which costs 350 ms/frame. The
        // driver copies into host-visible memory for us instead.
        // Cheap no-op in blocking mode, but pixels must never be read early.
        let _ = DecodedHandle::sync(&handle);
        let borrowed = handle.borrow();
        let size = (w as u32, h as u32);
        let image = Image::create_from(borrowed.surface(), self.nv12_format, size, size)
            .expect("vaGetImage failed");
        let va = *image.image();
        let data: &[u8] = image.as_ref();

        let (dst_y, rest) = self.out.split_at_mut(self.luma);
        let (dst_u, dst_v) = rest.split_at_mut(self.chroma);
        nv12_to_i420(
            &data[va.offsets[Y_PLANE] as usize..],
            va.pitches[Y_PLANE] as usize,
            dst_y,
            w,
            &data[va.offsets[UV_PLANE] as usize..],
            va.pitches[UV_PLANE] as usize,
            dst_u,
            cw,
            dst_v,
            cw,
            w,
            h,
        );
    }

    fn fill(&self, out: &mut VhFrame) {
        let w = self.meta.width as usize;
        let cw = w.div_ceil(2);
        out.y = self.out.as_ptr();
        // SAFETY: `out` was sized to luma + 2 * chroma just above in `emit`.
        unsafe {
            out.u = self.out.as_ptr().add(self.luma);
            out.v = self.out.as_ptr().add(self.luma + self.chroma);
        }
        out.y_stride = w;
        out.u_stride = cw;
        out.v_stride = cw;
        out.width = self.meta.width;
        out.height = self.meta.height;
    }
}

/// Opens `path` for hardware decode, positioned so the first
/// [`vh_next_frame`] returns sample `target_sample` (1-based; 0 and 1 both mean
/// the start of the stream). Returns null on any failure at all: no libva
/// runtime, no render node, unsupported profile, unreadable file.
///
/// # Safety
/// `path` must be a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_open_at(path: *const c_char, target_sample: u32) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if path.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: caller-guaranteed NUL-terminated string, read before return.
        let path = unsafe { CStr::from_ptr(path) };
        let Ok(path) = path.to_str() else {
            return std::ptr::null_mut();
        };
        match Session::open_at(Path::new(path), target_sample) {
            Some(session) => Box::into_raw(Box::new(session)) as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Fills `out` with stream metadata. 0 on success, negative on failure.
///
/// # Safety
/// `session` must come from [`vh_open_at`] and `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_meta(session: *mut c_void, out: *mut VhMeta) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() || out.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session and writable destination.
        unsafe {
            let session = &*(session as *const Session);
            (*out).width = session.meta.width;
            (*out).height = session.meta.height;
            (*out).frame_rate = session.meta.frame_rate;
            (*out).frame_count = session.meta.frame_count;
        }
        0
    }))
    .unwrap_or(-1)
}

/// 1 when `out` holds the next picture in display order, 0 at clean end of
/// stream, negative on error. Plane pointers stay valid until the next call.
///
/// # Safety
/// `session` must come from [`vh_open_at`] and `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_next_frame(session: *mut c_void, out: *mut VhFrame) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() || out.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session and writable destination.
        let session = unsafe { &mut *(session as *mut Session) };
        match session.pump() {
            Ok(true) => {
                // SAFETY: `out` is non-null and writable per the contract.
                session.fill(unsafe { &mut *out });
                1
            }
            Ok(false) => 0,
            Err(e) => {
                eprintln!("engine_hw: {e}");
                -2
            }
        }
    }))
    .unwrap_or(-1)
}

/// Releases a session. Safe to call with null.
///
/// # Safety
/// `session` must come from [`vh_open_at`] and must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_close(session: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !session.is_null() {
            // SAFETY: pointer came from `Box::into_raw` in `vh_open_at`.
            drop(unsafe { Box::from_raw(session as *mut Session) });
        }
    }));
}

fn align16(v: u32) -> u32 {
    v.div_ceil(16) * 16
}

/// Fills in slice NAL header bytes the driver left at zero.
///
/// Mesa's radeonsi encoder writes `forbidden_zero_bit`, `nal_ref_idc` and
/// `nal_unit_type` from state it only ever populates out of a *packed* slice
/// header, and cros-codecs supplies none (`h264/vaapi.rs` says as much in its
/// "use packed headers" TODO). Every other bit of the slice is correct, so the
/// whole damage is one byte per slice: measured on radeonsi, patching it turns
/// a stream neither ffmpeg nor `rusty_h264` will touch into one both decode.
///
/// An all-zero byte after a start code is never valid H.264, so drivers that do
/// write the header (Intel's, which is what cros-codecs was built against) fall
/// straight through this.
///
/// ponytail: the real fix is a `VAEncPackedHeaderH264_Slice` buffer in the
/// vendored cros-codecs backend, which means synthesizing the entire slice
/// header there rather than correcting two fields here.
fn fix_slice_nal_headers(au: &mut [u8]) {
    // cros-codecs prepends SPS+PPS to the access unit at every IDR and only
    // there, so an SPS seen earlier in this unit is what makes the slice one.
    let mut idr = false;
    let mut i = 0;
    while i + 3 < au.len() {
        if au[i..i + 3] != [0, 0, 1] {
            i += 1;
            continue;
        }
        let nal = &mut au[i + 3];
        if *nal == 0 {
            // nal_ref_idc 3 + IDR, or 2 + non-IDR: every picture this encoder
            // emits is a short-term reference (no B-frames, no disposables).
            *nal = if idr { 0x65 } else { 0x41 };
        } else if *nal & 0x1f == 7 {
            idr = true;
        }
        i += 3;
    }
}

/// One hardware H.264 encode session.
///
/// Input goes through a single reusable NV12 buffer object: the caller's I420
/// planes are interleaved into it and the encoder imports it as a VA surface.
/// Reuse is safe because the encoder runs in blocking mode and every entry
/// point polls it before returning, so the GPU is finished with the buffer
/// before the next picture is written into it.
struct EncSession {
    encoder: Encoder,
    /// Owns the DRM node the buffer object below was allocated from.
    _gbm: gbm::Device<std::fs::File>,
    frame: GenericDmaVideoFrame,
    layout: FrameLayout,
    /// Macroblock-aligned surface size; `width`/`height` are what is encoded.
    coded: Resolution,
    width: usize,
    height: usize,
    timestamp: u64,
    drained: bool,
    /// Coded access units the encoder has finished but the caller has not
    /// collected yet, in encode order (which is display order: no B-frames).
    ready: VecDeque<Vec<u8>>,
    /// The access unit currently handed out, kept alive until the next call.
    out: Vec<u8>,
}

impl EncSession {
    fn open(width: u32, height: u32, fps_num: u32, fps_den: u32, bitrate: u64) -> Option<Self> {
        // NV12 chroma is half resolution, so odd dimensions have no packing;
        // and radeonsi refuses encode contexts below 64x64 (measured).
        if width < 64 || height < 64 || width % 2 != 0 || height % 2 != 0 {
            return None;
        }
        if fps_num == 0 || fps_den == 0 || bitrate == 0 {
            return None;
        }
        let (display, gbm) = open_devices()?;
        let coded = Resolution {
            width: align16(width),
            height: align16(height),
        };
        let framerate = ((fps_num as f64 / fps_den as f64).round() as i64).clamp(1, 240) as u32;
        let low_power = display
            .query_config_entrypoints(VAProfile::VAProfileH264Main)
            .ok()?
            .contains(&VAEntrypoint::VAEntrypointEncSliceLP);
        let config = H264Config {
            resolution: Resolution { width, height },
            profile: H264Profile::Main,
            level: Level::L4,
            // No B-frames: coded order stays display order, which is what the
            // muxer's duration-only timing assumes.
            pred_structure: PredictionStructure::LowDelay {
                limit: (framerate * 2) as u16,
            },
            initial_tunings: Tunings {
                rate_control: RateControl::ConstantBitrate(bitrate),
                framerate,
                ..Default::default()
            },
        };
        let encoder = Encoder::new_vaapi(
            display,
            config,
            Fourcc::from(b"NV12"),
            coded,
            low_power,
            BlockingMode::Blocking,
        )
        .ok()?;
        // Allocated at the aligned size, so the buffer's pitch is never smaller
        // than a padded row.
        let frame = alloc_nv12(&gbm, coded).ok()?;
        let layout = nv12_layout(coded, frame.get_plane_pitch()[0]);
        Some(Self {
            encoder,
            _gbm: gbm,
            frame,
            layout,
            coded,
            width: width as usize,
            height: height as usize,
            timestamp: 0,
            drained: false,
            ready: VecDeque::new(),
            out: Vec::new(),
        })
    }

    /// Interleaves one packed I420 picture into the NV12 input buffer.
    ///
    /// # Safety
    /// `src`'s planes must cover `height` (resp. `height / 2`) rows of their
    /// declared stride.
    unsafe fn upload(&mut self, src: &VhFrame) -> Result<(), String> {
        let (w, h) = (self.width, self.height);
        let (cw, ch) = (w / 2, h / 2);
        let (aw, ah) = (self.coded.width as usize, self.coded.height as usize);
        let pitch = self.frame.get_plane_pitch();
        let mapping = self.frame.map_mut()?;
        let planes = mapping.get();
        {
            let mut dst = planes[0].borrow_mut();
            for row in 0..ah {
                // SAFETY: caller contract; rows past the picture repeat the last.
                let s = unsafe {
                    std::slice::from_raw_parts(src.y.add(row.min(h - 1) * src.y_stride), w)
                };
                let dst = &mut dst[row * pitch[0]..row * pitch[0] + aw];
                dst[..w].copy_from_slice(s);
                // Macroblock padding: replicate the edge rather than leave the
                // buffer's garbage there, which would cost bitrate for nothing.
                dst[w..].fill(s[w - 1]);
            }
        }
        let mut dst = planes[1].borrow_mut();
        for row in 0..ah / 2 {
            let src_row = row.min(ch - 1);
            // SAFETY: as above, for the two chroma planes.
            let (u, v) = unsafe {
                (
                    std::slice::from_raw_parts(src.u.add(src_row * src.u_stride), cw),
                    std::slice::from_raw_parts(src.v.add(src_row * src.v_stride), cw),
                )
            };
            let dst = &mut dst[row * pitch[1]..row * pitch[1] + aw];
            for x in 0..cw {
                dst[2 * x] = u[x];
                dst[2 * x + 1] = v[x];
            }
            for x in w..aw {
                dst[x] = dst[x - 2];
            }
        }
        Ok(())
    }

    /// # Safety
    /// As [`EncSession::upload`].
    unsafe fn encode(&mut self, src: &VhFrame, force_keyframe: bool) -> Result<(), String> {
        // SAFETY: caller contract, forwarded.
        unsafe { self.upload(src)? };
        let meta = FrameMetadata {
            timestamp: self.timestamp,
            layout: self.layout.clone(),
            force_keyframe,
        };
        self.timestamp += 1;
        self.encoder
            .encode(meta, self.frame.clone())
            .map_err(|e| e.to_string())?;
        self.collect()
    }

    /// Moves every access unit the encoder has finished into `ready`.
    fn collect(&mut self) -> Result<(), String> {
        while let Some(coded) = self.encoder.poll().map_err(|e| e.to_string())? {
            let mut au = coded.bitstream;
            fix_slice_nal_headers(&mut au);
            self.ready.push_back(au);
        }
        Ok(())
    }

    /// Hands the oldest finished access unit to the caller, or reports that
    /// there is none.
    fn take(&mut self, out: *mut *const u8, out_len: *mut usize) -> i32 {
        let Some(au) = self.ready.pop_front() else {
            return 0;
        };
        self.out = au;
        // SAFETY: caller-guaranteed writable destinations.
        unsafe {
            *out = self.out.as_ptr();
            *out_len = self.out.len();
        }
        1
    }
}

/// Opens a hardware H.264 encoder for `width`x`height` at `fps_num / fps_den`,
/// constant bitrate `bitrate` bits per second. Returns null on any failure at
/// all -- no VA-API runtime, no render node, no encode entrypoint, dimensions
/// the driver will not take -- and the caller falls back to software.
#[unsafe(no_mangle)]
pub extern "C" fn vh_enc_open(
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    bitrate: u64,
) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        match EncSession::open(width, height, fps_num, fps_den, bitrate) {
            Some(session) => Box::into_raw(Box::new(session)) as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Encodes one tightly-strided I420 picture. 1 when `out`/`out_len` describe one
/// Annex-B access unit, 0 when the encoder has nothing coded yet, negative on
/// error. The bytes stay valid until the next call on this session.
///
/// # Safety
/// `session` must come from [`vh_enc_open`], `frame`'s planes must cover the
/// picture at their declared strides, and `out`/`out_len` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_enc_frame(
    session: *mut c_void,
    frame: *const VhFrame,
    force_key: i32,
    out: *mut *const u8,
    out_len: *mut usize,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() || frame.is_null() || out.is_null() || out_len.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session and readable frame.
        let (session, frame) = unsafe { (&mut *(session as *mut EncSession), &*frame) };
        if frame.y.is_null() || frame.u.is_null() || frame.v.is_null() {
            return -1;
        }
        if frame.width as usize != session.width
            || frame.height as usize != session.height
            || frame.y_stride < session.width
            || frame.u_stride < session.width / 2
            || frame.v_stride < session.width / 2
        {
            return -1;
        }
        // SAFETY: caller contract on the plane pointers, sizes checked above.
        match unsafe { session.encode(frame, force_key != 0) } {
            Ok(()) => session.take(out, out_len),
            Err(e) => {
                eprintln!("engine_hw: {e}");
                -2
            }
        }
    }))
    .unwrap_or(-1)
}

/// Flushes the encoder: 1 while `out`/`out_len` describe another access unit,
/// 0 once every picture fed has been handed back, negative on error. Same
/// lifetime rule as [`vh_enc_frame`].
///
/// # Safety
/// `session` must come from [`vh_enc_open`]; `out`/`out_len` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_enc_drain(
    session: *mut c_void,
    out: *mut *const u8,
    out_len: *mut usize,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() || out.is_null() || out_len.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session.
        let session = unsafe { &mut *(session as *mut EncSession) };
        if !session.drained {
            if let Err(e) = session.encoder.drain().map_err(|e| e.to_string()) {
                eprintln!("engine_hw: {e}");
                return -2;
            }
            session.drained = true;
        }
        match session.collect() {
            Ok(()) => session.take(out, out_len),
            Err(e) => {
                eprintln!("engine_hw: {e}");
                -2
            }
        }
    }))
    .unwrap_or(-1)
}

/// Releases an encode session. Safe to call with null.
///
/// # Safety
/// `session` must come from [`vh_enc_open`] and must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_enc_close(session: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !session.is_null() {
            // SAFETY: pointer came from `Box::into_raw` in `vh_enc_open`.
            drop(unsafe { Box::from_raw(session as *mut EncSession) });
        }
    }));
}
