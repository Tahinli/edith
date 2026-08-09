//! VA-API H.264 decode, shipped as a `dlopen`-able plugin so the main binary
//! never gets a DT_NEEDED on libva/gbm/drm. Every entry point is `extern "C"`,
//! catches unwinds and reports failure as a null pointer or a negative code:
//! the caller's contract is "any failure means use the software decoder".
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
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder};
use cros_codecs::decoder::{BlockingMode, DecodedHandle, DecoderEvent, StreamInfo};
use cros_codecs::image_processing::nv12_to_i420;
use cros_codecs::libva::{Display, Image, VA_FOURCC_NV12, VAImageFormat};
use cros_codecs::video_frame::frame_pool::{FramePool, PooledVideoFrame};
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::video_frame::{UV_PLANE, Y_PLANE};
use cros_codecs::{Fourcc, FrameLayout, PlaneLayout, Resolution};
use gbm::{BufferObjectFlags, Format as GbmFormat};

use engine::demux::Demuxer;
use engine::hw::{VhFrame, VhMeta};

type PooledFrame = PooledVideoFrame<GenericDmaVideoFrame>;
type Decoder = StatelessDecoder<H264, VaapiBackend<PooledFrame>>;
type Handle = <Decoder as StatelessVideoDecoder>::Handle;

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
    GenericDmaVideoFrame::new(
        vec![std::fs::File::from(fd)],
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
        },
    )
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
        let decoder = Decoder::new_vaapi(display, BlockingMode::Blocking).ok()?;
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
            out: Vec::new(),
            luma: 0,
            chroma: 0,
        })
    }

    /// Pumps the decoder until one picture is ready. `Ok(false)` is clean EOF.
    fn pump(&mut self) -> Result<bool, String> {
        let mut stalls = 0u32;
        loop {
            // Stop draining as soon as we have a picture: every handle we hold
            // keeps a pool buffer out of circulation.
            while self.ready.is_empty() {
                match self.decoder.next_event() {
                    Some(DecoderEvent::FrameReady(handle)) => self.ready.push_back(handle),
                    Some(DecoderEvent::FormatChanged) => {
                        let info = self
                            .decoder
                            .stream_info()
                            .ok_or("format changed without stream info")?
                            .clone();
                        self.pool.resize(&info);
                    }
                    None => break,
                }
            }
            if let Some(handle) = self.ready.pop_front() {
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
                        self.decoder.flush().map_err(|e| e.to_string())?;
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
            match decoder.decode(*timestamp, pending, &mut || pool.alloc()) {
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

/// Opens `path` for hardware decode. Returns null on any failure at all: no
/// libva runtime, no render node, unsupported profile, unreadable file.
///
/// # Safety
/// `path` must be a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_open(path: *const c_char) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if path.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: caller-guaranteed NUL-terminated string, read before return.
        let path = unsafe { CStr::from_ptr(path) };
        let Ok(path) = path.to_str() else {
            return std::ptr::null_mut();
        };
        match Session::open(Path::new(path)) {
            Some(session) => Box::into_raw(Box::new(session)) as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Fills `out` with stream metadata. 0 on success, negative on failure.
///
/// # Safety
/// `session` must come from [`vh_open`] and `out` must be writable.
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
/// `session` must come from [`vh_open`] and `out` must be writable.
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
/// `session` must come from [`vh_open`] and must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_close(session: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !session.is_null() {
            // SAFETY: pointer came from `Box::into_raw` in `vh_open`.
            drop(unsafe { Box::from_raw(session as *mut Session) });
        }
    }));
}
