//! VA-API decode (H.264, HEVC, VP9 and AV1) and encode (H.264 and HEVC, and AV1
//! where the GPU has an entrypoint for it), shipped as a
//! `dlopen`-able plugin so the main binary never gets a DT_NEEDED on
//! libva/gbm/drm. Every entry point is
//! `extern "C"`, catches unwinds and reports failure as a null pointer or a
//! negative code: the caller's contract is "any failure means use the software
//! codec".
//!
//! corner-cut: that guarantee rests on `panic = "unwind"`. Building this crate with
//! `panic = "abort"` would turn a driver bug into a killed app; the upgrade path
//! is running the decoder in a child process instead of in-process.

use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_void};
use std::os::fd::IntoRawFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use cros_codecs::backend::vaapi::decoder::VaapiBackend;
use cros_codecs::backend::vaapi::encoder::VaapiBackend as VaapiEncBackend;
use cros_codecs::codec::av1::parser::Profile as Av1Profile;
use cros_codecs::codec::h264::parser::{Level, Profile as H264Profile};
use cros_codecs::codec::h265::parser::{Level as H265Level, Profile as H265Profile};
use cros_codecs::decoder::stateless::av1::Av1;
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::h265::H265;
use cros_codecs::decoder::stateless::vp9::Vp9;
use cros_codecs::decoder::stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder};
use cros_codecs::decoder::{BlockingMode, DecodedHandle, DecoderEvent, StreamInfo};
use cros_codecs::encoder::av1::EncoderConfig as Av1Config;
use cros_codecs::encoder::h264::EncoderConfig as H264Config;
use cros_codecs::encoder::h265::EncoderConfig as H265Config;
use cros_codecs::encoder::stateless::av1::StatelessEncoder as Av1StatelessEncoder;
use cros_codecs::encoder::stateless::h264::StatelessEncoder;
use cros_codecs::encoder::stateless::h265::StatelessEncoder as H265StatelessEncoder;
use cros_codecs::encoder::{
    FrameMetadata, PredictionStructure, RateControl, Tunings, VideoEncoder,
};
use cros_codecs::image_processing::nv12_to_i420;
use cros_codecs::libva::{
    Display, Image, Surface, VA_FOURCC_NV12, VA_FOURCC_P010, VAEntrypoint, VAImageFormat,
    VAProfile,
};
use cros_codecs::video_frame::VideoFrame;
use cros_codecs::video_frame::frame_pool::{FramePool, PooledVideoFrame};
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::video_frame::{UV_PLANE, Y_PLANE};
use cros_codecs::{Fourcc, FrameLayout, PlaneLayout, Resolution};
use gbm::{BufferObjectFlags, Format as GbmFormat};

use engine::demux::{Codec, Demuxer};
use engine::hw::{CAP_AV1, CAP_H264, CAP_HEVC, CAP_VP9, VhCaps, VhDma, VhFrame, VhMeta};

type PooledFrame = PooledVideoFrame<GenericDmaVideoFrame>;
type Dec<C> = StatelessDecoder<C, VaapiBackend<PooledFrame>>;
type Handle = <Dec<H264> as StatelessVideoDecoder>::Handle;

/// One decoder per codec the demuxer can hand us. They all share the VA-API
/// backend and therefore the same [`Handle`], so everything past `decode` is
/// common; which one exists is decided by the container, never by the caller --
/// which is why the plugin's C ABI is unchanged and an older `libengine_hw.so`
/// still loads and still decodes H.264 (it simply refuses the newer codecs'
/// files at `Demuxer`).
///
/// 4:2:0 only, 8- or 10-bit: HEVC Main and Main 10, VP9 profile 0 and 2, AV1
/// Main at either depth.
/// A 10-bit stream decodes into a P010 pool and is read back down to the 8-bit
/// I420 every frame here is ([`Session::emit`]).
///
/// corner-cut: 10-bit is *carried* 8-bit, because [`VhFrame`] is a byte-per-sample
/// interface all the way to the renderer. That costs the low two bits of a Main
/// 10 source; the upgrade path is a 16-bit `VhFrame`, which is a change to every
/// stage between here and the texture upload, not to this file.
/// VP9 profile 2 reaches the P010 pool like the rest: a VP9 stream states its
/// depth in no container record the way an `av1C` or an `hvcC` does, so
/// `Demuxer` reads it off the first keyframe's uncompressed header
/// (`demux::vp9_bit_depth`) rather than assuming the 8 bits of profile 0.
enum Decoder {
    H264(Dec<H264>),
    Hevc(Dec<H265>),
    Vp9(Dec<Vp9>),
    Av1(Dec<Av1>),
}

impl Decoder {
    fn get(&mut self) -> &mut dyn StatelessVideoDecoder<Handle = Handle> {
        match self {
            Self::H264(d) => d,
            Self::Hevc(d) => d,
            Self::Vp9(d) => d,
            Self::Av1(d) => d,
        }
    }
}

type Encoder = StatelessEncoder<
    GenericDmaVideoFrame,
    VaapiEncBackend<GenericDmaVideoFrame, Surface<GenericDmaVideoFrame>>,
>;
type Av1Encoder = Av1StatelessEncoder<
    GenericDmaVideoFrame,
    VaapiEncBackend<GenericDmaVideoFrame, Surface<GenericDmaVideoFrame>>,
>;
type HevcEncoder = H265StatelessEncoder<
    GenericDmaVideoFrame,
    VaapiEncBackend<GenericDmaVideoFrame, Surface<GenericDmaVideoFrame>>,
>;

/// Ceiling on decode/drain iterations that make no progress. VA-API in blocking
/// mode should always either hand back an event or accept input; this only
/// exists so a misbehaving driver returns an error instead of hanging the app.
const STALL_LIMIT: u32 = 10_000;

/// How many undecodable access units a seek may drop before the session gives
/// up and calls the stream broken. An open GOP's leading pictures are a handful
/// (a Blu-ray remux's are 8-16); a file that keeps failing past this is not one
/// we restarted in the middle of, so the error is real and the caller still
/// hears it.
const LEADING_LIMIT: u32 = 64;

/// How many pictures handed out as DRM-PRIME buffers stay un-decoded-over, and
/// therefore how many extra buffers the pool reserves the moment a caller asks
/// for one. The export's decode channel is two pictures deep and it encodes the
/// one it is holding, so at most four are ever outstanding; the rest is margin,
/// at ~3 MB of video memory each.
const DMA_HOLD: usize = 6;

struct Session {
    decoder: Decoder,
    /// NV12 (or, for a 10-bit stream, P010) descriptor for `vaGetImage`,
    /// queried once at open.
    nv12_format: VAImageFormat,
    /// Whether the surfaces are P010 rather than NV12, which is the one thing
    /// the read-back has to do differently.
    ten_bit: bool,
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
    /// Access units dropped because they referenced pictures this session never
    /// decoded -- see [`Session::pump`]. Counted rather than flagged so the
    /// tolerance ends somewhere, and raised to [`LEADING_LIMIT`] the moment a
    /// picture is handed out, which is where the window closes.
    leading: u32,
    /// Tightly packed I420 handed out through [`VhFrame`]; reused every frame,
    /// which is exactly why the pointers are only valid until the next call.
    out: Vec<u8>,
    luma: usize,
    chroma: usize,
    /// What the caller's encoder can read straight off the GPU, asked once per
    /// [`vh_next_frame_dma`] call and `None` for every read-back caller.
    want: Option<VhDma>,
    /// The descriptor the last such call produced, its file descriptor already
    /// belonging to the caller. `None` says that picture was read back instead.
    dma: Option<VhDma>,
    /// Pictures handed out as DRM-PRIME buffers, newest last: the pool takes a
    /// buffer back the moment its handle drops, and the decoder would write the
    /// next picture straight into the one an encoder is still reading. Bounded
    /// by [`Session::hold`], which is what the pool reserved for it.
    held: VecDeque<Handle>,
    /// Extra pool buffers reserved for [`Session::held`]: [`DMA_HOLD`] once a
    /// caller has asked for a buffer *before* the pool was sized, and 0
    /// otherwise -- a session whose pool is already sized may not hold anything
    /// back, because those buffers are the decoder's own.
    hold: usize,
    /// Whether the pool has been sized, which happens once, at the first
    /// `FormatChanged`.
    sized: bool,
}

/// Mesa's GBM cannot allocate an NV12 buffer object at all (measured on
/// radeonsi: every flag/modifier combination returns NULL), so we allocate one
/// linear single-plane R8 buffer big enough for luma + chroma and describe the
/// NV12 layout inside it ourselves. VA-API imports it as one DRM-PRIME object
/// with two planes at known offsets, which is exactly what the descriptor in
/// `GenericDmaVideoFrame` emits.
///
/// `ten_bit` asks for the same thing in P010: the very same two-plane 4:2:0
/// shape with 16-bit samples, so the buffer is simply twice as wide.
fn alloc_nv12(
    gbm: &gbm::Device<std::fs::File>,
    coded: Resolution,
    ten_bit: bool,
) -> Result<GenericDmaVideoFrame, String> {
    let rows = coded.height + coded.height.div_ceil(2);
    let width = coded.width * if ten_bit { 2 } else { 1 };
    let bo = gbm
        .create_buffer_object::<()>(width, rows, GbmFormat::R8, BufferObjectFlags::LINEAR)
        .map_err(|e| format!("gbm R8 {width}x{rows} allocation failed: {e}"))?;
    let pitch = bo.stride().map_err(|e| e.to_string())? as usize;
    let fd = bo.fd().map_err(|e| e.to_string())?;
    GenericDmaVideoFrame::new(
        vec![std::fs::File::from(fd)],
        nv12_layout(coded, pitch, ten_bit),
    )
}

/// Two NV12 (or P010) planes packed one after the other in a single buffer
/// object.
fn nv12_layout(coded: Resolution, pitch: usize, ten_bit: bool) -> FrameLayout {
    FrameLayout {
        format: (
            Fourcc::from(if ten_bit { b"P010" } else { b"NV12" }),
            0, /* DRM_FORMAT_MOD_LINEAR */
        ),
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
        // A 10-bit stream decodes into P010 surfaces and is read back through a
        // P010 image; a driver that has no such image format is one this cannot
        // read 10-bit off at all, which is a null session and a software
        // fallback like any other refusal here.
        let ten_bit = demuxer.bit_depth() > 8;
        let want = if ten_bit {
            VA_FOURCC_P010
        } else {
            VA_FOURCC_NV12
        };
        let nv12_format = display
            .query_image_formats()
            .ok()?
            .into_iter()
            .find(|f| f.fourcc == want)?;
        let decoder = match meta.codec {
            Codec::H264 => {
                Decoder::H264(Dec::<H264>::new_vaapi(display, BlockingMode::Blocking).ok()?)
            }
            Codec::Hevc => {
                Decoder::Hevc(Dec::<H265>::new_vaapi(display, BlockingMode::Blocking).ok()?)
            }
            Codec::Vp9 => {
                Decoder::Vp9(Dec::<Vp9>::new_vaapi(display, BlockingMode::Blocking).ok()?)
            }
            Codec::Av1 => {
                Decoder::Av1(Dec::<Av1>::new_vaapi(display, BlockingMode::Blocking).ok()?)
            }
        };
        let pool = FramePool::new(move |info: &StreamInfo| {
            alloc_nv12(&gbm, info.coded_resolution, ten_bit).expect("output frame allocation failed")
        });
        Some(Self {
            decoder,
            nv12_format,
            ten_bit,
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
            // A stream read from its own beginning has no missing references
            // and therefore no leading pictures to forgive: the window opens
            // only for a session positioned somewhere else ([`Session::open_at`]),
            // so a file whose first picture will not decode is still refused at
            // the door rather than opening onto nothing.
            leading: LEADING_LIMIT,
            out: Vec::new(),
            luma: 0,
            chroma: 0,
            want: None,
            dma: None,
            held: VecDeque::new(),
            hold: 0,
            sized: false,
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
        if target_frame > 0 {
            session.leading = 0;
        }
        Some(session)
    }

    /// Repositions an **open** session, exactly as [`Session::open_at`] positions
    /// a new one: the next [`Session::pump`] hands back sample `target_sample`.
    ///
    /// This is what a seek costs when the session is kept: a demuxer seek and a
    /// decoder flush, and *not* the VA-API initialisation (~90 ms measured), the
    /// render node probe, the container parse and the surface pool that opening
    /// one again pays for.
    ///
    /// The flush is the whole of the decoder's reset: cros-codecs finishes what
    /// is in flight and goes to `Reset`, from where it resumes on the next
    /// parameter set or key frame -- and every sync sample this demuxer hands
    /// out carries the parameter sets in front of it (`demux`'s `is_sync`
    /// re-injection), which is exactly where a seek lands. The pictures the
    /// flush makes ready are dropped with `ready`: they belong to where the
    /// session *was*.
    fn seek_to(&mut self, target_sample: u32) -> Result<(), String> {
        self.decoder.get().flush().map_err(|e| e.to_string())?;
        // Drain what the flush completed, dropping every handle: each one holds
        // a pool buffer out of circulation, and none of them is ours to show.
        while let Some(event) = self.decoder.get().next_event() {
            if let DecoderEvent::FormatChanged = event {
                let info = self
                    .decoder
                    .get()
                    .stream_info()
                    .ok_or("format changed without stream info")?
                    .clone();
                self.pool.resize(&info);
            }
        }
        self.ready.clear();
        self.pending.clear();
        self.flushed = false;
        // The same arithmetic `open_at` does, for the same reason.
        let target_frame = target_sample.saturating_sub(1);
        let first = self.demuxer.seek_to_sync_at_or_before(target_frame);
        self.skip = (i64::from(target_frame) - first).max(0) as u32;
        // ...and the same open-GOP window: a session positioned anywhere but the
        // start may meet leading pictures that reference what it never decoded.
        self.leading = if target_frame > 0 { 0 } else { LEADING_LIMIT };
        Ok(())
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
                        let mut info = self
                            .decoder
                            .get()
                            .stream_info()
                            .ok_or("format changed without stream info")?
                            .clone();
                        // The buffers a caller may hold on to are *extra*: the
                        // count the decoder asks for is what it needs to keep
                        // decoding, and lending one of those out is a stall.
                        info.min_num_frames += self.hold;
                        self.pool.resize(&info);
                        self.sized = true;
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
                self.deliver(handle);
                // Past the random access point's leading pictures: a decode
                // error from here on is a real one again.
                self.leading = LEADING_LIMIT;
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
                leading,
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
                // A picture that references pictures we never decoded, which is
                // what an open GOP's leading pictures are after a seek: they
                // display *before* the random access point we restarted at, so
                // they are not ours to show and the stream is not broken. Drop
                // the rest of the access unit (they are picture-aligned) and
                // walk on -- failing here is what used to make a Blu-ray remux
                // fall back to software on 17 seeks out of 20.
                Err(e) if *leading < LEADING_LIMIT => {
                    if *leading == 0 {
                        eprintln!("engine_hw: skipping leading pictures after seek ({e})");
                    }
                    *leading += 1;
                    pending.clear();
                    *timestamp += 1;
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    /// Hands one decoded picture to the caller: as the buffer it was decoded
    /// into where that is what the caller asked for and the encoder on the
    /// other side can read it, and as read-back I420 everywhere else.
    fn deliver(&mut self, handle: Handle) {
        self.dma = None;
        if self.hold > 0 && let Some(dma) = self.export_dma(&handle) {
            self.meta.width = dma.width;
            self.meta.height = dma.height;
            self.dma = Some(dma);
            // Kept out of the pool until the caller is [`DMA_HOLD`] pictures
            // past it, by which time nothing is reading it any more.
            self.held.push_back(handle);
            while self.held.len() > self.hold {
                self.held.pop_front();
            }
            return;
        }
        self.emit(handle);
    }

    /// This picture's surface as a DRM-PRIME buffer, if it *is* the shape the
    /// caller's encoder takes -- one composed two-plane layer in at most two
    /// objects, the fourcc and both sizes it asked for. `None` for every other
    /// answer, and the caller then gets the pixels it always got rather than a
    /// failure.
    fn export_dma(&self, handle: &Handle) -> Option<VhDma> {
        let want = self.want?;
        let res = handle.display_resolution();
        if (res.width, res.height) != (want.width, want.height) {
            return None;
        }
        // Cheap no-op in blocking mode, but the encoder must never read a
        // surface the decoder has not finished writing.
        let _ = DecodedHandle::sync(handle);
        let borrowed = handle.borrow();
        // The *display* surface for the same reason [`Session::emit`] reads it:
        // an AV1 picture with film grain is displayed from a second surface.
        let mut desc = borrowed.display_surface().export_prime().ok()?;
        let layer = desc.layers.first()?;
        if desc.layers.len() != 1
            || layer.num_planes != 2
            || desc.fourcc != want.fourcc
            || desc.width != want.coded_width
            || desc.height != want.coded_height
        {
            return None;
        }
        // Whatever the driver says the two planes live in, said back to it
        // unchanged on the import: radeonsi answers with an object per plane,
        // other drivers with one for both, and neither is ours to reinterpret.
        let (offset, stride) = (
            [layer.offset[0], layer.offset[1]],
            [layer.pitch[0], layer.pitch[1]],
        );
        let planes = [layer.object_index[0] as usize, layer.object_index[1] as usize];
        if planes[0] != 0 || planes[1] > 1 || desc.objects.len() != planes[1] + 1 {
            return None;
        }
        let modifier = desc.objects[0].drm_format_modifier;
        // Handed over with the descriptor: the caller closes them.
        let second = (planes[1] == 1)
            .then(|| desc.objects.pop().map(|object| object.fd.into_raw_fd()))
            .flatten()
            .unwrap_or(-1);
        let first = desc.objects.remove(0).fd.into_raw_fd();
        Some(VhDma {
            fd: [first, second],
            fourcc: desc.fourcc,
            modifier,
            coded_width: desc.width,
            coded_height: desc.height,
            width: res.width,
            height: res.height,
            offset,
            stride,
        })
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
        // The *display* surface, which is the reconstructed picture for every
        // codec here but the second, grain-synthesized one for an AV1 frame that
        // asks for film grain -- reading the reconstructed surface of such a
        // frame would show the film with its grain stripped off.
        let image = Image::create_from(borrowed.display_surface(), self.nv12_format, size, size)
            .expect("vaGetImage failed");
        let va = *image.image();
        let data: &[u8] = image.as_ref();

        let (dst_y, rest) = self.out.split_at_mut(self.luma);
        let (dst_u, dst_v) = rest.split_at_mut(self.chroma);
        if self.ten_bit {
            p010_to_i420(
                &data[va.offsets[Y_PLANE] as usize..],
                va.pitches[Y_PLANE] as usize,
                dst_y,
                &data[va.offsets[UV_PLANE] as usize..],
                va.pitches[UV_PLANE] as usize,
                dst_u,
                dst_v,
                w,
                h,
            );
            return;
        }
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

/// One P010 picture down to packed 8-bit I420: [`nv12_to_i420`] for a surface
/// whose samples are 16-bit little-endian with the value in the **high** bits,
/// which is what P010 means. The high byte of each pair is therefore the 10-bit
/// sample shifted down by two, i.e. the 8-bit one -- no rounding, because a
/// second read of the low byte costs more than the half-bit it would buy.
fn p010_to_i420(
    src_y: &[u8],
    src_y_pitch: usize,
    dst_y: &mut [u8],
    src_uv: &[u8],
    src_uv_pitch: usize,
    dst_u: &mut [u8],
    dst_v: &mut [u8],
    width: usize,
    height: usize,
) {
    for row in 0..height {
        let src = &src_y[row * src_y_pitch..];
        let dst = &mut dst_y[row * width..row * width + width];
        for (x, out) in dst.iter_mut().enumerate() {
            *out = src[2 * x + 1];
        }
    }
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    for row in 0..ch {
        let src = &src_uv[row * src_uv_pitch..];
        let (u, v) = (
            &mut dst_u[row * cw..row * cw + cw],
            &mut dst_v[row * cw..row * cw + cw],
        );
        for x in 0..cw {
            u[x] = src[4 * x + 1];
            v[x] = src[4 * x + 3];
        }
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

/// Repositions an open session so the next [`vh_next_frame`] returns sample
/// `target_sample` (1-based, as [`vh_open_at`]). 0 on success, negative on
/// failure -- and a caller that gets one closes the session and opens another,
/// which is what it did for every seek before this symbol existed.
///
/// # Safety
/// `session` must come from [`vh_open_at`] and still be open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_seek(session: *mut c_void, target_sample: u32) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session.
        let session = unsafe { &mut *(session as *mut Session) };
        match session.seek_to(target_sample) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("engine_hw: seek failed: {e}");
                -2
            }
        }
    }))
    .unwrap_or(-1)
}

/// As [`vh_next_frame`], but hands the picture over *on the GPU* where it can
/// be: `dma` is filled and its `fd` belongs to the caller (who closes it) when
/// the decoded surface is exactly what an encoder of `coded_width` x
/// `coded_height` carrying a `width` x `height` picture reads, and `out` is
/// filled with read-back I420 exactly as [`vh_next_frame`] fills it otherwise.
/// `dma.fd` below zero is what says which happened.
///
/// The buffer stays out of the decoder's pool for [`DMA_HOLD`] further
/// pictures and no longer: encode it before asking for that many more. Only a
/// session that has never read a frame back may hand buffers over at all --
/// those extra pool buffers are reserved at the first picture, and a decoder
/// robbed of its own would stall.
///
/// # Safety
/// `session` must come from [`vh_open_at`]; `out` and `dma` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_next_frame_dma(
    session: *mut c_void,
    out: *mut VhFrame,
    dma: *mut VhDma,
    coded_width: u32,
    coded_height: u32,
    width: u32,
    height: u32,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() || out.is_null() || dma.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session and writable destinations.
        let session = unsafe { &mut *(session as *mut Session) };
        // SAFETY: as above.
        unsafe { *dma = VhDma::default() };
        // A pool sized without the reserve has nothing to lend: this session
        // has been read back from, so it stays that way.
        if !session.sized || session.hold > 0 {
            session.hold = DMA_HOLD;
            session.want = Some(VhDma {
                fourcc: VA_FOURCC_NV12,
                coded_width,
                coded_height,
                width,
                height,
                ..VhDma::default()
            });
        }
        let pumped = session.pump();
        session.want = None;
        match pumped {
            Ok(true) => {
                match session.dma.take() {
                    // SAFETY: `dma` is non-null and writable per the contract.
                    Some(desc) => unsafe { *dma = desc },
                    // SAFETY: as above, for `out`.
                    None => session.fill(unsafe { &mut *out }),
                }
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

/// What each codec needs of the driver before this plugin can claim it: the
/// 8-bit decode profiles, the 10-bit ones, and the profile the *encoder* here
/// would open -- `None` for the one this plugin has no encoder for at all, so a
/// GPU with a VP9 encode entrypoint is never reported as one edith can
/// reach. The encode profiles are the very ones [`EncSession::open_codec`]
/// asks for, which is what keeps this a description of the plugin rather than a
/// second opinion about it.
const CAP_TABLE: [(u32, &[VAProfile::Type], &[VAProfile::Type], Option<VAProfile::Type>); 4] = [
    (
        CAP_H264,
        &[
            VAProfile::VAProfileH264ConstrainedBaseline,
            VAProfile::VAProfileH264Main,
            VAProfile::VAProfileH264High,
        ],
        &[],
        Some(VAProfile::VAProfileH264Main),
    ),
    (
        CAP_HEVC,
        &[VAProfile::VAProfileHEVCMain],
        &[VAProfile::VAProfileHEVCMain10],
        // 8-bit Main only: the encoder here opens Main whatever the source was,
        // so a Main 10 entrypoint is a *decode* claim on this line and nothing
        // more. Main 10 encode would be a second profile here and a 10-bit input
        // buffer in `EncSession`, neither of which exists yet.
        Some(VAProfile::VAProfileHEVCMain),
    ),
    (
        CAP_VP9,
        &[VAProfile::VAProfileVP9Profile0],
        &[VAProfile::VAProfileVP9Profile2],
        None,
    ),
    (
        CAP_AV1,
        &[VAProfile::VAProfileAV1Profile0],
        // AV1 carries 8- and 10-bit in the same profile 0, so the profile bit
        // is the same one and the P010 read-back is what decides.
        &[VAProfile::VAProfileAV1Profile0],
        Some(VAProfile::VAProfileAV1Profile0),
    ),
];

/// This machine's decode and encode seats, asked of the driver rather than
/// assumed: which profiles it has, which entrypoints each of those carries, and
/// whether a P010 image exists to read a 10-bit picture back through. Only
/// codecs this plugin actually implements can light up -- `CAP_TABLE` is that
/// half of the intersection.
///
/// `None` when there is no display to ask at all, which is the same "software
/// only" a missing plugin means.
fn query_caps() -> Option<VhCaps> {
    let (display, _gbm) = open_devices()?;
    let profiles = display.query_config_profiles().ok()?;
    let p010 = display
        .query_image_formats()
        .is_ok_and(|formats| formats.iter().any(|f| f.fourcc == VA_FOURCC_P010));
    let has = |profile: VAProfile::Type, entrypoint: VAEntrypoint::Type| {
        profiles.contains(&profile)
            && display
                .query_config_entrypoints(profile)
                .is_ok_and(|e| e.contains(&entrypoint))
    };
    let decodes = |list: &[VAProfile::Type]| {
        list.iter()
            .any(|&p| has(p, VAEntrypoint::VAEntrypointVLD))
    };
    let mut caps = VhCaps::default();
    for (bit, eight, ten, encode) in CAP_TABLE {
        if decodes(eight) {
            caps.decode |= bit;
        }
        if p010 && decodes(ten) {
            caps.decode_10bit |= bit;
        }
        // Both entrypoints, exactly as the encoder's own open takes either.
        if encode.is_some_and(|p| {
            has(p, VAEntrypoint::VAEntrypointEncSlice) || has(p, VAEntrypoint::VAEntrypointEncSliceLP)
        }) {
            caps.encode |= bit;
        }
    }
    Some(caps)
}

/// Fills `out` with what this machine and this plugin can do together. 0 on
/// success, negative when there is nothing to report -- an *optional* symbol on
/// purpose, so the caller resolving it is free to be older or newer than this.
///
/// Costs one VA-API init; the caller caches the answer.
///
/// # Safety
/// `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_caps(out: *mut VhCaps) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out.is_null() {
            return -1;
        }
        let Some(caps) = query_caps() else {
            return -1;
        };
        // SAFETY: caller-guaranteed writable destination.
        unsafe { *out = caps };
        0
    }))
    .unwrap_or(-1)
}

fn align16(v: u32) -> u32 {
    v.div_ceil(16) * 16
}

fn align64(v: u32) -> u32 {
    v.div_ceil(64) * 64
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
/// corner-cut: the real fix is a `VAEncPackedHeaderH264_Slice` buffer in the
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
/// Pictures the caller already has on the GPU go straight in
/// ([`EncSession::encode_dma`]) and touch none of what follows. The rest --
/// pixels in the caller's memory -- go through [`enc_depth`] reusable NV12
/// buffer objects: the caller's
/// I420 planes are interleaved into the next one and the encoder imports it as a
/// VA surface. Reuse is safe because a buffer is only written again once the
/// access unit coded *from* it has been polled back -- the encoder runs in
/// blocking mode and a coded unit in hand means the GPU has finished reading its
/// input -- and [`EncSession::encode`] holds exactly that many pictures in
/// flight, no more.
///
/// **Why more than one.** With a single buffer the picture had to be coded
/// before the next one could be written into it, so a frame cost the CPU's
/// interleave *plus* the GPU's encode end to end (6.7 ms + 2.5 ms at 1080p on
/// radeonsi, measured 2026-08-13). With two, the interleave of the next picture
/// runs while the GPU codes this one and the frame costs the longer of them.
struct EncSession {
    /// Boxed because the three codecs are three types and everything past
    /// `encode` is the same trait: one session type serves them all, which is
    /// what keeps the C ABI at one new symbol per codec (the open) instead of
    /// four.
    encoder: Box<dyn VideoEncoder<GenericDmaVideoFrame>>,
    /// Which codec the open picked, kept because one thing past it still
    /// differs: the H.264 driver leaves a *NAL* header byte at zero, and
    /// neither an HEVC access unit nor an AV1 temporal unit has that one.
    codec: EncCodec,
    /// Owns the DRM node the buffer objects below were allocated from.
    _gbm: gbm::Device<std::fs::File>,
    /// [`enc_depth`] input buffers, used round-robin.
    frames: Vec<GenericDmaVideoFrame>,
    /// How many of them there are, which is how many pictures may be in the
    /// encoder at once.
    depth: usize,
    /// Which of them the next picture is written into.
    slot: usize,
    /// Pictures submitted whose access unit has not been polled back yet, which
    /// is what says whether the buffer about to be written is free.
    in_flight: usize,
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
    /// One padded output row, staged in ordinary memory and copied across whole
    /// -- [`EncSession::upload`] says why.
    row: Vec<u8>,
}

/// How many pictures may be in the encoder at once. Two is what it takes to
/// overlap the CPU's interleave with the GPU's encode; more would buy nothing,
/// since the interleave is the longer of the two and a third buffer would only
/// hold a picture waiting for a lane that is never idle.
const ENC_DEPTH: usize = 2;

/// How many pictures *this* seat may hold, which is [`ENC_DEPTH`] for H.264 and
/// one for the other two.
///
/// Depth 2 means a submitted picture must hand its access unit back on the next
/// poll, and [`EncSession::encode`] treats a seat that does not as an error --
/// it cannot write into a buffer the GPU may still be reading. That contract
/// was measured on the H.264 seat and only there. HEVC and AV1 on this plugin
/// are intra-only and go through the same driver, so they very probably behave
/// the same way; "very probably" is not what a hard error should rest on, and
/// the GPU has been unavailable since the measurement was possible. They keep
/// the old one-buffer behaviour -- the picture coded before the next is
/// written, exactly as every seat did before -- until someone measures them.
fn enc_depth(codec: EncCodec) -> usize {
    match codec {
        EncCodec::H264 => ENC_DEPTH,
        EncCodec::Hevc | EncCodec::Av1 => 1,
    }
}

/// Which codec an [`EncSession`] was opened for. Decided once, at the open, the
/// way the H.264 seat's dimensions are -- everything after it is one trait.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EncCodec {
    H264,
    Hevc,
    Av1,
}

impl EncSession {
    fn open(width: u32, height: u32, fps_num: u32, fps_den: u32, bitrate: u64) -> Option<Self> {
        Self::open_codec(width, height, fps_num, fps_den, bitrate, EncCodec::H264)
    }

    fn open_codec(
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        bitrate: u64,
        codec: EncCodec,
    ) -> Option<Self> {
        // NV12 chroma is half resolution, so odd dimensions have no packing;
        // and radeonsi refuses encode contexts below 64x64 (measured).
        if width < 64 || height < 64 || width % 2 != 0 || height % 2 != 0 {
            return None;
        }
        // The HEVC entrypoint's own floor, which is a different number: the same
        // driver that takes a 128x128 H.264 context answers `VaError(19)` to an
        // HEVC one below 384x384 (measured 2026-08-13 on radeonsi). Said here so
        // a small export falls back to software at the open rather than through
        // a driver refusal, and so the C ABI never claims a seat it has not got.
        if codec == EncCodec::Hevc && (width < 384 || height < 384) {
            return None;
        }
        if fps_num == 0 || fps_den == 0 || bitrate == 0 {
            return None;
        }
        let (display, gbm) = open_devices()?;
        // A macroblock is 16 wide; an HEVC coding tree block is 64, and that
        // seat's parameter sets declare the picture at coding-tree size (see the
        // vendored `h265/predictor.rs`), so the surface has to carry every row
        // those sets promise.
        let align = match codec {
            EncCodec::Hevc => align64,
            _ => align16,
        };
        let coded = Resolution {
            width: align(width),
            height: align(height),
        };
        let framerate = ((fps_num as f64 / fps_den as f64).round() as i64).clamp(1, 240) as u32;
        let profile = match codec {
            EncCodec::Av1 => VAProfile::VAProfileAV1Profile0,
            EncCodec::Hevc => VAProfile::VAProfileHEVCMain,
            EncCodec::H264 => VAProfile::VAProfileH264Main,
        };
        // The driver's own answer to "can you encode this at all": a GPU with no
        // AV1 encode entrypoint is what makes the caller fall back to `rav1e`,
        // and asking here is what turns that into a null rather than a failed
        // encode session halfway through an export.
        let entrypoints = display.query_config_entrypoints(profile).ok()?;
        if !entrypoints.contains(&VAEntrypoint::VAEntrypointEncSlice)
            && !entrypoints.contains(&VAEntrypoint::VAEntrypointEncSliceLP)
        {
            return None;
        }
        let low_power = entrypoints.contains(&VAEntrypoint::VAEntrypointEncSliceLP);
        // No B-frames on either codec: coded order stays display order, which is
        // what both muxers' duration-only timing assumes.
        let pred_structure = PredictionStructure::LowDelay {
            limit: (framerate * 2) as u16,
        };
        let encoder: Box<dyn VideoEncoder<GenericDmaVideoFrame>> = match codec {
            EncCodec::Av1 => {
                // Constant quality, because that is the only rate control the
                // vendored AV1 backend takes (`stateless/av1/vaapi.rs` refuses
                // anything else outright); 128 of 255 is its own reference
                // value. The caller's bitrate therefore does not reach this
                // seat -- it reaches `rav1e`, which is the other one.
                //
                // corner-cut: bitrate-driven AV1 on the GPU needs `VA_RC_CBR`
                // wired through that backend, which is a change in the vendored
                // crate rather than here.
                let config = Av1Config {
                    profile: Av1Profile::Profile0,
                    resolution: Resolution { width, height },
                    pred_structure,
                    initial_tunings: Tunings {
                        rate_control: RateControl::ConstantQuality(128),
                        framerate,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                Box::new(
                    Av1Encoder::new_vaapi(
                        display,
                        config,
                        Fourcc::from(b"NV12"),
                        coded,
                        low_power,
                        BlockingMode::Blocking,
                    )
                    .ok()?,
                )
            }
            EncCodec::Hevc => {
                // **Every picture an IDR**, which is the one place this seat
                // does not mirror the H.264 one. A GOP's P slices come out of
                // this driver undecodable: the slice segment headers are the
                // *driver's* to write (as they are for H.264) and radeonsi
                // writes a constant picture order count into every one of them,
                // so a decoder sees frame after frame claiming to be the same
                // output picture. Measured 2026-08-13: with the default GOP,
                // only the leading IDR of each group decodes.
                //
                // So the hardware seat is intra-only exactly as the software one
                // is (`export::Enc::open_hevc`), and an HEVC export is an
                // intraframe master either way -- the same file, coded by the
                // GPU instead of by twelve cores.
                //
                // corner-cut: inter HEVC on this GPU needs the slice segment
                // header written here rather than by the driver
                // (`VAEncPackedHeaderSlice`), which is a change in the vendored
                // backend; upgrade path is that, or a driver that writes a
                // correct POC.
                let config = H265Config {
                    resolution: Resolution { width, height },
                    profile: H265Profile::Main,
                    level: H265Level::L4,
                    pred_structure: PredictionStructure::LowDelay { limit: 1 },
                    initial_tunings: Tunings {
                        rate_control: RateControl::ConstantBitrate(bitrate),
                        framerate,
                        ..Default::default()
                    },
                };
                Box::new(
                    HevcEncoder::new_vaapi(
                        display,
                        config,
                        Fourcc::from(b"NV12"),
                        coded,
                        low_power,
                        BlockingMode::Blocking,
                    )
                    .ok()?,
                )
            }
            EncCodec::H264 => {
                let config = H264Config {
                    resolution: Resolution { width, height },
                    profile: H264Profile::Main,
                    level: Level::L4,
                    pred_structure,
                    initial_tunings: Tunings {
                        rate_control: RateControl::ConstantBitrate(bitrate),
                        framerate,
                        ..Default::default()
                    },
                };
                Box::new(
                    Encoder::new_vaapi(
                        display,
                        config,
                        Fourcc::from(b"NV12"),
                        coded,
                        low_power,
                        BlockingMode::Blocking,
                    )
                    .ok()?,
                )
            }
        };
        // Allocated at the aligned size, so the buffer's pitch is never smaller
        // than a padded row.
        let depth = enc_depth(codec);
        let frames: Vec<_> = (0..depth)
            .map(|_| alloc_nv12(&gbm, coded, false))
            .collect::<Result<_, _>>()
            .ok()?;
        let layout = nv12_layout(coded, frames[0].get_plane_pitch()[0], false);
        Some(Self {
            encoder,
            codec,
            _gbm: gbm,
            frames,
            depth,
            slot: 0,
            in_flight: 0,
            layout,
            coded,
            width: width as usize,
            height: height as usize,
            timestamp: 0,
            drained: false,
            ready: VecDeque::new(),
            out: Vec::new(),
            row: Vec::new(),
        })
    }

    /// Interleaves one packed I420 picture into the NV12 input buffer `slot`.
    ///
    /// **Row at a time, through ordinary memory.** The mapping below is the
    /// GPU's own buffer, which is write-combining: a store of a byte or two
    /// costs a partial-buffer flush, while one `memcpy` of a whole row costs a
    /// burst. Interleaving the chroma directly into it was 8.2 ms a picture at
    /// 1080p against 6.7 ms staged (measured 2026-08-13 on radeonsi, alternating
    /// the two paths frame by frame inside one export so the machine's load
    /// weighed on both), and the picture is four fifths of what a hardware
    /// export spends per frame. The padding is written the same way and for the
    /// same reason: it used to be a four-byte second store per row.
    ///
    /// # Safety
    /// `src`'s planes must cover `height` (resp. `height / 2`) rows of their
    /// declared stride.
    unsafe fn upload(&mut self, src: &VhFrame, slot: usize) -> Result<(), String> {
        let (w, h) = (self.width, self.height);
        let (cw, ch) = (w / 2, h / 2);
        let (aw, ah) = (self.coded.width as usize, self.coded.height as usize);
        // Two fields at once: the buffer is mapped for writing while the staging
        // row is written into, and they are not the same field.
        let Self { frames, row, .. } = self;
        let frame = &mut frames[slot];
        let pitch = frame.get_plane_pitch();
        let mapping = frame.map_mut()?;
        let planes = mapping.get();
        row.resize(aw, 0);
        let row_buf = &mut row[..];
        {
            let mut dst = planes[0].borrow_mut();
            for row in 0..ah {
                // SAFETY: caller contract; rows past the picture repeat the last.
                let s = unsafe {
                    std::slice::from_raw_parts(src.y.add(row.min(h - 1) * src.y_stride), w)
                };
                row_buf[..w].copy_from_slice(s);
                // Macroblock padding: replicate the edge rather than leave the
                // buffer's garbage there, which would cost bitrate for nothing.
                row_buf[w..].fill(s[w - 1]);
                dst[row * pitch[0]..row * pitch[0] + aw].copy_from_slice(row_buf);
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
            for x in 0..cw {
                row_buf[2 * x] = u[x];
                row_buf[2 * x + 1] = v[x];
            }
            for x in w..aw {
                row_buf[x] = row_buf[x - 2];
            }
            dst[row * pitch[1]..row * pitch[1] + aw].copy_from_slice(row_buf);
        }
        Ok(())
    }

    /// # Safety
    /// As [`EncSession::upload`].
    unsafe fn encode(&mut self, src: &VhFrame, force_keyframe: bool) -> Result<(), String> {
        // The buffer this picture is written into is the one whose own access
        // unit is already in hand -- `in_flight` is held below `depth` at the
        // end of every call, so the round-robin never catches the GPU up. At
        // depth 1 that is the picture just coded, which is the one-buffer
        // behaviour every seat had before ([`enc_depth`]).
        let slot = self.slot;
        // SAFETY: caller contract, forwarded.
        unsafe { self.upload(src, slot)? };
        self.slot = (self.slot + 1) % self.depth;
        let meta = FrameMetadata {
            timestamp: self.timestamp,
            layout: self.layout.clone(),
            force_keyframe,
        };
        self.timestamp += 1;
        self.encoder
            .encode(meta, self.frames[slot].clone())
            .map_err(|e| e.to_string())?;
        self.in_flight += 1;
        // ...and the picture before it is collected here rather than this one:
        // that is the whole overlap, the GPU coding what was just submitted
        // while the caller decodes and interleaves the next picture.
        while self.in_flight >= self.depth {
            if !self.pull()? {
                // Blocking mode: a picture accepted and no coded unit for the
                // one before it means the encoder is not answering, and the
                // next upload would overwrite a buffer the GPU still reads.
                return Err("the encoder took a picture and returned nothing coded".into());
            }
        }
        Ok(())
    }

    /// Encodes a picture the decoder left on the GPU: the buffer is imported as
    /// a surface of this session's own shape and read there, so nothing of it
    /// ever passes through the CPU -- no `vaGetImage` on the way out of the
    /// decoder and no interleave on the way in here.
    ///
    /// The geometry is checked rather than trusted: a buffer of another shape
    /// would be coded as garbage, and the caller asked
    /// [`vh_enc_dma_geometry`] what this seat takes before the decoder ever
    /// handed one over.
    ///
    /// **No deferred poll here, unlike [`EncSession::encode`].** That one holds
    /// [`enc_depth`] pictures in flight to overlap the *next* picture's CPU
    /// interleave with this one's encode; a picture arriving on the GPU has no
    /// interleave to overlap, so there is nothing to defer for and the access
    /// units are collected as soon as they exist. What the depth still buys is
    /// the input buffer, and this path uses none of `frames`: the surface is the
    /// decoder's, held for us by its own [`DMA_HOLD`] contract, and draining
    /// every call is what keeps that hold short.
    ///
    /// It is counted in `in_flight` all the same, because one session takes both
    /// kinds -- an untouched clip hands buffers over while a graded one or a gap
    /// goes through the pixel path -- and a submission missing from the count is
    /// a `frames` slot the pixel path would believe free one picture too early.
    fn encode_dma(&mut self, dma: &VhDma, force_keyframe: bool) -> Result<(), String> {
        if dma.fd[0] < 0
            || dma.fourcc != VA_FOURCC_NV12
            || (dma.coded_width, dma.coded_height) != (self.coded.width, self.coded.height)
            || (dma.width as usize, dma.height as usize) != (self.width, self.height)
        {
            return Err(format!(
                "imported buffer is {}x{} in {} coded {}x{}, not {}x{} coded {}x{}",
                dma.width,
                dma.height,
                dma.fourcc,
                dma.coded_width,
                dma.coded_height,
                self.width,
                self.height,
                self.coded.width,
                self.coded.height
            ));
        }
        // One object or two, exactly as the decoder's driver described them:
        // the buffers stay the caller's, so each is dup'd for the frame below.
        let mut files = Vec::new();
        for fd in dma.fd.iter().copied().take_while(|fd| *fd >= 0) {
            // SAFETY: the descriptor's handles are the caller's, open for the
            // length of the call.
            let owned = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }
                .try_clone_to_owned()
                .map_err(|e| format!("dup of the imported buffer failed: {e}"))?;
            files.push(std::fs::File::from(owned));
        }
        let objects = files.len();
        let layout = FrameLayout {
            format: (Fourcc::from(b"NV12"), dma.modifier),
            size: self.coded,
            planes: (0..2)
                .map(|plane| PlaneLayout {
                    buffer_index: plane.min(objects - 1),
                    offset: dma.offset[plane] as usize,
                    stride: dma.stride[plane] as usize,
                })
                .collect(),
        };
        let frame = GenericDmaVideoFrame::new(files, layout.clone())?;
        let meta = FrameMetadata {
            timestamp: self.timestamp,
            layout,
            force_keyframe,
        };
        self.timestamp += 1;
        self.encoder
            .encode(meta, frame)
            .map_err(|e| e.to_string())?;
        self.in_flight += 1;
        self.collect()
    }

    /// Moves every access unit the encoder has finished into `ready`.
    fn collect(&mut self) -> Result<(), String> {
        while self.pull()? {}
        Ok(())
    }

    /// One finished access unit into `ready`; `false` where the encoder has
    /// none to give.
    fn pull(&mut self) -> Result<bool, String> {
        let Some(coded) = self.encoder.poll().map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        let mut au = coded.bitstream;
        if self.codec == EncCodec::H264 {
            fix_slice_nal_headers(&mut au);
        }
        self.ready.push_back(au);
        self.in_flight = self.in_flight.saturating_sub(1);
        Ok(true)
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

/// The same, coding AV1 instead of H.264: null unless this GPU has an AV1
/// encode entrypoint, which is recent hardware -- the caller then encodes AV1 in
/// software instead. `bitrate` is not used by this codec (see `EncSession::open`:
/// the vendored backend is constant-quality only) and is taken all the same, so
/// the two opens stay one signature.
///
/// The session it returns is fed, drained and closed through `vh_enc_frame`,
/// `vh_enc_drain` and `vh_enc_close` exactly as an H.264 one is; what comes back
/// out of them is AV1 temporal units rather than Annex-B access units.
#[unsafe(no_mangle)]
pub extern "C" fn vh_enc_av1_open(
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    bitrate: u64,
) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        match EncSession::open_codec(width, height, fps_num, fps_den, bitrate, EncCodec::Av1) {
            Some(session) => Box::into_raw(Box::new(session)) as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// The same, coding HEVC: null unless this GPU has an HEVC encode entrypoint and
/// the picture is at least 384x384 ([`EncSession::open_codec`] states that floor
/// and where it was measured), and the caller then encodes HEVC in software
/// instead.
///
/// **Intra-only**: every picture it hands back is an IDR carrying its own VPS,
/// SPS and PPS, which is what makes the file decodable at all on this driver --
/// see the `EncCodec::Hevc` arm of [`EncSession::open_codec`]. `bitrate` is the
/// constant bitrate the GPU codes at, as the H.264 seat's is.
///
/// The session it returns is fed, drained and closed through `vh_enc_frame`,
/// `vh_enc_drain` and `vh_enc_close` exactly as an H.264 one is; what comes back
/// out of them is Annex-B HEVC access units.
#[unsafe(no_mangle)]
pub extern "C" fn vh_enc_hevc_open(
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    bitrate: u64,
) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        match EncSession::open_codec(width, height, fps_num, fps_den, bitrate, EncCodec::Hevc) {
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

/// What a decoded buffer must look like to reach this encoder without a copy:
/// the surface size it codes in, written to `coded_width`/`coded_height`. 1 on
/// an answer, negative on none. NV12 and the size this session was opened for
/// are the rest of the shape, and the caller already knows both.
///
/// # Safety
/// `session` must come from [`vh_enc_open`]; both outputs must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_enc_dma_geometry(
    session: *mut c_void,
    coded_width: *mut u32,
    coded_height: *mut u32,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() || coded_width.is_null() || coded_height.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session and writable destinations.
        unsafe {
            let session = &*(session as *const EncSession);
            *coded_width = session.coded.width;
            *coded_height = session.coded.height;
        }
        1
    }))
    .unwrap_or(-1)
}

/// Encodes one picture the decoder left on the GPU, described by `dma` -- whose
/// file descriptor stays the *caller's*, borrowed for the length of the call.
/// Same answer and same lifetime rule as [`vh_enc_frame`]; a buffer of a shape
/// this session cannot read is an error and not a silent re-shape.
///
/// # Safety
/// `session` must come from [`vh_enc_open`], `dma` must be readable and its
/// descriptor open, and `out`/`out_len` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vh_enc_frame_dma(
    session: *mut c_void,
    dma: *const VhDma,
    force_key: i32,
    out: *mut *const u8,
    out_len: *mut usize,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() || dma.is_null() || out.is_null() || out_len.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session and readable descriptor.
        let (session, dma) = unsafe { (&mut *(session as *mut EncSession), &*dma) };
        match session.encode_dma(dma, force_key != 0) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs against whatever GPU this machine has -- and passes on one with no
    /// VA-API at all, which is exactly what `None` means here. What it pins is
    /// the *shape* of the answer, the contents being the driver's: nothing may
    /// be claimed that this plugin cannot actually reach.
    #[test]
    fn the_caps_query_never_claims_more_than_this_plugin_implements() {
        let Some(caps) = query_caps() else {
            return;
        };
        eprintln!("{caps:?}");
        // Encode is this plugin's three seats and no others, whatever
        // entrypoints the driver has: a VP9 EncSlice is not an encoder here.
        assert_eq!(
            caps.encode & !(CAP_H264 | CAP_HEVC | CAP_AV1),
            0,
            "{caps:?}"
        );
        // A 10-bit claim is a decode claim -- same profile family, read back
        // through P010 -- and there is no 10-bit encode path at all.
        assert_eq!(caps.decode_10bit & !caps.decode, 0, "{caps:?}");
        // H.264 here is 8-bit 4:2:0; the table has no 10-bit profile for it.
        assert_eq!(caps.decode_10bit & CAP_H264, 0, "{caps:?}");
    }
}
