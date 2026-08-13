//! Runtime-optional hardware decode/encode plugin (`libengine_hw.so`).
//!
//! The plugin links libva/gbm/drm; this crate and the app binary must not, so
//! the only coupling is this C ABI plus a `dlopen`. Anything that goes wrong --
//! plugin missing, no VA-API runtime, no render node, unsupported stream --
//! leaves us with `None` and the caller falls back to the software codec.
//!
//! Decode and encode resolve their symbols into *separate* tables on purpose: a
//! plugin built before the encode entry points existed must still decode, so a
//! missing `vh_enc_*` may only cost us [`HwEncoder`], never [`HwSession`].
//!
//! The codec is *not* part of this ABI and adding VP9 and HEVC did not change
//! it: the plugin demuxes the file it is given and picks its decoder from the
//! container, so a plugin older than that support simply returns null for such a
//! path -- the same "no" it gives for an unsupported profile, and the caller's
//! honest [`crate::demux::Codec::needs_plugin`] refusal covers both.

use std::ffi::{CString, c_char, c_void};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libloading::Library;

const LIB_NAME: &str = "libengine_hw.so";

/// Stream metadata, C layout. Mirrors [`crate::VideoMeta`].
#[repr(C)]
#[derive(Default)]
pub struct VhMeta {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub frame_count: u32,
}

/// One decoded picture as planar I420. Pointers belong to the plugin and stay
/// valid only until the next `vh_next_frame`/`vh_close` on the same session.
#[repr(C)]
pub struct VhFrame {
    pub y: *const u8,
    pub u: *const u8,
    pub v: *const u8,
    pub y_stride: usize,
    pub u_stride: usize,
    pub v_stride: usize,
    pub width: u32,
    pub height: u32,
}

impl Default for VhFrame {
    fn default() -> Self {
        Self {
            y: std::ptr::null(),
            u: std::ptr::null(),
            v: std::ptr::null(),
            y_stride: 0,
            u_stride: 0,
            v_stride: 0,
            width: 0,
            height: 0,
        }
    }
}

/// One decoded picture *left on the GPU*: a DRM-PRIME buffer the encoder
/// imports instead of the caller reading the pixels back and writing them out
/// again. All integers, so it crosses a channel to the encoding thread the way
/// a picture's bytes used to.
///
/// The `fd` belongs to whoever holds this: the plugin hands ownership over with
/// the descriptor and [`DmaFrame`] closes it. What it does *not* own is the
/// buffer's contents -- the decode session keeps the picture from being decoded
/// over for a few frames only (see `vh_next_frame_dma`), which is why nothing
/// but the export's two-deep decode channel may sit between the two calls.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VhDma {
    /// One DRM-PRIME handle per plane, luma then chroma -- which is how the
    /// driver hands its own surfaces out (radeonsi exports an NV12 surface as
    /// *two* objects into one allocation, measured 2026-08-13), and describing
    /// it any other way is guessing at what the second one means. `fd[0]` below
    /// zero says this picture is not on the GPU at all; `fd[1]` below zero says
    /// both planes live in the first object.
    pub fd: [i32; 2],
    /// `NV12` and nothing else, today.
    pub fourcc: u32,
    /// DRM format modifier of the buffer, passed through to the import.
    pub modifier: u64,
    /// The surface's own size, which is the picture's rounded up to whatever
    /// the codec codes in -- what an encoder has to agree with, sample for
    /// sample, before it can read this buffer at all.
    pub coded_width: u32,
    pub coded_height: u32,
    /// The picture inside it.
    pub width: u32,
    pub height: u32,
    /// Luma then chroma, each inside its own object above.
    pub offset: [u32; 2],
    pub stride: [u32; 2],
}

impl Default for VhDma {
    fn default() -> Self {
        Self {
            fd: [-1, -1],
            fourcc: 0,
            modifier: 0,
            coded_width: 0,
            coded_height: 0,
            width: 0,
            height: 0,
            offset: [0; 2],
            stride: [0; 2],
        }
    }
}

/// A [`VhDma`] with its file descriptor owned: dropping it closes the handle,
/// which is the only thing this side of the ABI has to get right.
pub struct DmaFrame(VhDma);

impl DmaFrame {
    pub fn width(&self) -> u32 {
        self.0.width
    }

    pub fn height(&self) -> u32 {
        self.0.height
    }
}

impl Drop for DmaFrame {
    fn drop(&mut self) {
        for fd in self.0.fd {
            if fd >= 0 {
                // SAFETY: the plugin transferred these descriptors to us with
                // the frame and nothing else owns them; `DmaFrame` is not
                // `Clone`, and the two are distinct handles even where they
                // name one allocation.
                drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
            }
        }
    }
}

/// What *this machine* takes, as far as the plugin implements it: one bit per
/// codec ([`CAP_H264`] and its three neighbours) in each mask, so a caller reads
/// "H.264 decodes and encodes here, HEVC only decodes" off three `u32`s.
///
/// The intersection is the plugin's to compute, never the driver's alone: a GPU
/// with an HEVC *encode* entrypoint is still not an HEVC encoder here -- the
/// plugin has no such entry point -- and a capability nothing can reach would be
/// a lie in whatever the front-end draws from this.
#[repr(C)]
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct VhCaps {
    pub decode: u32,
    pub encode: u32,
    /// Codecs whose 10-bit profiles decode: the driver has the profile *and* a
    /// P010 image format for the read-back, which is the pair the plugin's
    /// 10-bit path needs.
    pub decode_10bit: u32,
}

pub const CAP_H264: u32 = 1 << 0;
pub const CAP_HEVC: u32 = 1 << 1;
pub const CAP_VP9: u32 = 1 << 2;
pub const CAP_AV1: u32 = 1 << 3;

struct Plugin {
    open_at: unsafe extern "C" fn(*const c_char, u32) -> *mut c_void,
    meta: unsafe extern "C" fn(*mut c_void, *mut VhMeta) -> i32,
    next_frame: unsafe extern "C" fn(*mut c_void, *mut VhFrame) -> i32,
    /// The zero-copy door, and optional for the same reason [`Plugin::caps`] is:
    /// a plugin built before it decodes exactly as it did, through the read-back
    /// above.
    next_frame_dma: Option<
        unsafe extern "C" fn(*mut c_void, *mut VhFrame, *mut VhDma, u32, u32, u32, u32) -> i32,
    >,
    close: unsafe extern "C" fn(*mut c_void),
    /// The capability query, and the one symbol of this table that may be
    /// missing: a plugin built before it exports the four above and not this,
    /// which costs a front-end the listing and never a decode.
    caps: Option<unsafe extern "C" fn(*mut VhCaps) -> i32>,
    /// Repositioning an open session, and optional for the reason `caps` is: a
    /// plugin built before it decodes exactly as it did, and a caller that
    /// cannot reposition closes the session and opens another -- which is what
    /// every seek did before this symbol existed.
    seek: Option<unsafe extern "C" fn(*mut c_void, u32) -> i32>,
    // Never dropped (lives in a static), so the fn pointers above stay valid.
    _lib: Library,
}

fn plugin() -> Option<&'static Plugin> {
    static PLUGIN: OnceLock<Option<Plugin>> = OnceLock::new();
    PLUGIN.get_or_init(load).as_ref()
}

/// Where the plugin may live: next to the executable first (cargo puts both in
/// target/<profile>), then whatever the dynamic linker's search path turns up.
fn candidates() -> impl Iterator<Item = PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(LIB_NAME)))
        .into_iter()
        .chain(std::iter::once(PathBuf::from(LIB_NAME)))
}

fn load() -> Option<Plugin> {
    for candidate in candidates() {
        // SAFETY: loading a shared object runs its initialisers; we only ever
        // name our own plugin, and every symbol below is type-checked against
        // the definitions this crate shares with it.
        let lib = match unsafe { Library::new(candidate) } {
            Ok(lib) => lib,
            Err(_) => continue,
        };
        let plugin = unsafe {
            (|| {
                Some(Plugin {
                    open_at: *lib.get(b"vh_open_at\0").ok()?,
                    meta: *lib.get(b"vh_meta\0").ok()?,
                    next_frame: *lib.get(b"vh_next_frame\0").ok()?,
                    next_frame_dma: lib.get(b"vh_next_frame_dma\0").ok().map(|s| *s),
                    close: *lib.get(b"vh_close\0").ok()?,
                    caps: lib.get(b"vh_caps\0").ok().map(|s| *s),
                    seek: lib.get(b"vh_seek\0").ok().map(|s| *s),
                    _lib: lib,
                })
            })()
        };
        if plugin.is_some() {
            return plugin;
        }
    }
    None
}

/// What the plugin says this machine decodes and encodes, asked once per
/// process and cached: the answer cannot change while we run.
///
/// `None` on every way there is no answer -- no plugin, a plugin older than the
/// symbol (optional exactly as `vh_enc_av1_open` is: such a plugin still
/// decodes), no driver, a driver that refuses the query -- and a caller says
/// "software only" for all of them, which is what they mean.
///
/// Costs one VA-API init (~90 ms) the first time: ask it off a render thread.
pub fn caps() -> Option<VhCaps> {
    static CAPS: OnceLock<Option<VhCaps>> = OnceLock::new();
    *CAPS.get_or_init(|| {
        let query = plugin()?.caps?;
        let mut caps = VhCaps::default();
        // SAFETY: the plugin writes the struct it is handed and reports "no
        // answer" as a negative code; nothing is borrowed across the call.
        match unsafe { query(&mut caps) } {
            0 => Some(caps),
            _ => None,
        }
    })
}

/// An open hardware decode session. Dropping it closes the plugin session.
pub struct HwSession {
    plugin: &'static Plugin,
    handle: *mut c_void,
}

impl HwSession {
    /// Probes the plugin against this very file: success means the driver,
    /// the render node and the stream's profile are all usable.
    pub fn open(path: &Path) -> Option<Self> {
        Self::open_at(path, 0)
    }

    /// As [`HwSession::open`], but the first [`HwSession::next_frame`] returns
    /// display index `start_frame`; earlier pictures are decoded and dropped
    /// inside the plugin, without the read-back copy.
    pub fn open_at(path: &Path, start_frame: u32) -> Option<Self> {
        let plugin = plugin()?;
        let c_path = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        // Sample ids are 1-based, frame indices 0-based.
        let target_sample = start_frame.saturating_add(1);
        // SAFETY: `c_path` is a valid NUL-terminated string alive for the call.
        let handle = unsafe { (plugin.open_at)(c_path.as_ptr(), target_sample) };
        if handle.is_null() {
            return None;
        }
        Some(Self { plugin, handle })
    }

    /// Repositions this very session so the next [`HwSession::next_frame`]
    /// returns display index `start_frame` -- the same landing
    /// [`HwSession::open_at`] gives, without the VA-API initialisation (~90 ms),
    /// the render node probe and the container parse that opening one again
    /// costs. What lets a decode worker outlive the seeks its caller makes.
    ///
    /// `false` when the plugin cannot do it -- too old to export the symbol, or
    /// a decoder that would not flush -- and the caller then opens a new session
    /// exactly as it always did. The session is left untouched either way, so a
    /// `false` is never a half-moved decoder.
    pub fn seek(&mut self, start_frame: u32) -> bool {
        let Some(seek) = self.plugin.seek else {
            return false;
        };
        // Sample ids are 1-based, frame indices 0-based -- as in `open_at`.
        let target_sample = start_frame.saturating_add(1);
        // SAFETY: `handle` came from `vh_open_at` and is still open; the plugin
        // writes nothing through it and the call cannot unwind (it catches).
        unsafe { seek(self.handle, target_sample) == 0 }
    }

    pub fn meta(&self) -> Option<VhMeta> {
        let mut meta = VhMeta::default();
        // SAFETY: `handle` came from `vh_open_at` and is still open.
        if unsafe { (self.plugin.meta)(self.handle, &mut meta) } < 0 {
            return None;
        }
        Some(meta)
    }

    /// Next picture in display order as tightly packed I420 `(y, u, v)`.
    /// `Ok(None)` is clean end of stream.
    pub fn next_frame(&mut self) -> crate::Result<Option<(&[u8], &[u8], &[u8], u32, u32)>> {
        let mut frame = VhFrame::default();
        // SAFETY: `handle` is live; the plugin fills `frame` with pointers that
        // stay valid until the next call, which `&mut self` prevents overlapping.
        match unsafe { (self.plugin.next_frame)(self.handle, &mut frame) } {
            0 => return Ok(None),
            1 => {}
            code => return Err(format!("hardware decode failed (code {code})").into()),
        }
        self.pixels(&frame).map(Some)
    }

    /// The next picture *without reading it back* where the caller's encoder can
    /// take the buffer the decoder wrote into -- which is what `want` describes,
    /// down to the coded size, because an encoder reads a surface of its own
    /// shape or none at all.
    ///
    /// Anything the plugin cannot hand over that way comes back as pixels
    /// instead ([`HwPicture::Pixels`]), so a mismatch costs the read-back it
    /// always cost and never a frame. A plugin too old to know the door exists
    /// is the same answer.
    ///
    /// The buffer behind a [`DmaFrame`] is the decoder's, held out of its pool
    /// for a few pictures only: encode it before asking this for a few more.
    pub fn next_frame_dma(&mut self, want: DmaWant) -> crate::Result<Option<HwPicture<'_>>> {
        let Some(next_dma) = self.plugin.next_frame_dma else {
            return Ok(self.next_frame()?.map(|(y, u, v, w, h)| HwPicture::Pixels(y, u, v, w, h)));
        };
        let mut frame = VhFrame::default();
        let mut dma = VhDma::default();
        // SAFETY: as `next_frame`, plus a writable descriptor whose `fd` the
        // plugin either hands us ownership of or leaves at -1.
        let code = unsafe {
            next_dma(
                self.handle,
                &mut frame,
                &mut dma,
                want.coded_width,
                want.coded_height,
                want.width,
                want.height,
            )
        };
        match code {
            0 => return Ok(None),
            1 => {}
            code => return Err(format!("hardware decode failed (code {code})").into()),
        }
        if dma.fd[0] >= 0 {
            return Ok(Some(HwPicture::Dma(DmaFrame(dma))));
        }
        let (y, u, v, w, h) = self.pixels(&frame)?;
        Ok(Some(HwPicture::Pixels(y, u, v, w, h)))
    }

    /// The plugin's plane pointers as slices, checked against the strides it
    /// declares -- the one shape this side takes.
    fn pixels(&self, frame: &VhFrame) -> crate::Result<(&[u8], &[u8], &[u8], u32, u32)> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        if frame.y.is_null()
            || frame.u.is_null()
            || frame.v.is_null()
            || frame.y_stride != w
            || frame.u_stride != cw
            || frame.v_stride != cw
        {
            return Err("hardware plugin returned a non-packed I420 frame".into());
        }
        // SAFETY: plane pointers are non-null and the strides just checked above
        // pin each plane's length; the borrow ends before the next call.
        unsafe {
            Ok((
                std::slice::from_raw_parts(frame.y, w * h),
                std::slice::from_raw_parts(frame.u, cw * ch),
                std::slice::from_raw_parts(frame.v, cw * ch),
                frame.width,
                frame.height,
            ))
        }
    }
}

/// What an encoder needs a decoded buffer to look like before it can read it
/// without a copy: the surface's coded size and the picture inside it.
#[derive(Clone, Copy)]
pub struct DmaWant {
    pub coded_width: u32,
    pub coded_height: u32,
    pub width: u32,
    pub height: u32,
}

/// One picture out of [`HwSession::next_frame_dma`]: still on the GPU, or read
/// back like every other frame.
pub enum HwPicture<'a> {
    Pixels(&'a [u8], &'a [u8], &'a [u8], u32, u32),
    Dma(DmaFrame),
}

impl Drop for HwSession {
    fn drop(&mut self) {
        // SAFETY: `handle` came from `vh_open_at` and is closed exactly once.
        unsafe { (self.plugin.close)(self.handle) }
    }
}

// The plugin session is used from one thread at a time (the decode worker) and
// never shared; VA-API state inside it is not `Sync`.
unsafe impl Send for HwSession {}

struct EncPlugin {
    open: extern "C" fn(u32, u32, u32, u32, u64) -> *mut c_void,
    /// The AV1 seat, and the one symbol that may be missing: a plugin built
    /// before AV1 encode existed exports the four below and not this, which
    /// costs an AV1 export its hardware path and nothing else. Every session it
    /// opens is fed and closed through the very same three entry points -- the
    /// plugin keeps one session type for both codecs, so the codec is decided
    /// once, at the open, exactly as the H.264 seat's parameters are.
    open_av1: Option<extern "C" fn(u32, u32, u32, u32, u64) -> *mut c_void>,
    /// The HEVC seat, missing from an older plugin for the same reason and at
    /// the same cost: an HEVC export then runs on the software intra encoder.
    open_hevc: Option<extern "C" fn(u32, u32, u32, u32, u64) -> *mut c_void>,
    frame:
        unsafe extern "C" fn(*mut c_void, *const VhFrame, i32, *mut *const u8, *mut usize) -> i32,
    /// The zero-copy pair, both optional together: an older plugin exports
    /// neither and every picture reaches it as bytes, which is what it always
    /// did.
    dma_geometry: Option<unsafe extern "C" fn(*mut c_void, *mut u32, *mut u32) -> i32>,
    frame_dma:
        Option<unsafe extern "C" fn(*mut c_void, *const VhDma, i32, *mut *const u8, *mut usize) -> i32>,
    drain: unsafe extern "C" fn(*mut c_void, *mut *const u8, *mut usize) -> i32,
    close: unsafe extern "C" fn(*mut c_void),
    // Never dropped (lives in a static), so the fn pointers above stay valid.
    _lib: Library,
}

fn enc_plugin() -> Option<&'static EncPlugin> {
    static PLUGIN: OnceLock<Option<EncPlugin>> = OnceLock::new();
    PLUGIN.get_or_init(load_enc).as_ref()
}

fn load_enc() -> Option<EncPlugin> {
    for candidate in candidates() {
        // SAFETY: as in `load` -- our own plugin, symbols type-checked against
        // the definitions this crate shares with it.
        let lib = match unsafe { Library::new(candidate) } {
            Ok(lib) => lib,
            Err(_) => continue,
        };
        let plugin = unsafe {
            (|| {
                Some(EncPlugin {
                    open: *lib.get(b"vh_enc_open\0").ok()?,
                    open_av1: lib.get(b"vh_enc_av1_open\0").ok().map(|s| *s),
                    open_hevc: lib.get(b"vh_enc_hevc_open\0").ok().map(|s| *s),
                    frame: *lib.get(b"vh_enc_frame\0").ok()?,
                    dma_geometry: lib.get(b"vh_enc_dma_geometry\0").ok().map(|s| *s),
                    frame_dma: lib.get(b"vh_enc_frame_dma\0").ok().map(|s| *s),
                    drain: *lib.get(b"vh_enc_drain\0").ok()?,
                    close: *lib.get(b"vh_enc_close\0").ok()?,
                    _lib: lib,
                })
            })()
        };
        if plugin.is_some() {
            return plugin;
        }
    }
    None
}

/// An open hardware encode session. Dropping it closes the plugin session.
pub struct HwEncoder {
    plugin: &'static EncPlugin,
    handle: *mut c_void,
}

impl HwEncoder {
    /// H.264 at `width`x`height`, `fps_num / fps_den` frames per second, coded
    /// at a constant `bitrate` bits per second. `None` whenever hardware encode
    /// is unavailable -- plugin missing, plugin too old to export the encode
    /// entry points, no driver, dimensions the driver refuses -- and the caller
    /// falls back to the software encoder.
    pub fn open(width: u32, height: u32, fps_num: u32, fps_den: u32, bitrate: u64) -> Option<Self> {
        let plugin = enc_plugin()?;
        Self::opened(
            plugin,
            plugin.open,
            width,
            height,
            fps_num,
            fps_den,
            bitrate,
        )
    }

    /// The same, coding AV1 instead. `None` on everything [`HwEncoder::open`]
    /// answers `None` to *and* on a plugin or a GPU with no AV1 encoder -- an
    /// AV1 encode entrypoint is recent hardware, so this is the fallback that
    /// really fires, and the caller's software AV1 encoder takes the export
    /// without saying anything about it.
    ///
    /// The caller only reaches this behind `VE_HW_AV1=1`: the vendored encoder
    /// reset this project's own GPU, which [`crate::export::Enc::open_av1`]
    /// states in full.
    pub fn open_av1(
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        bitrate: u64,
    ) -> Option<Self> {
        let plugin = enc_plugin()?;
        let open = plugin.open_av1?;
        Self::opened(plugin, open, width, height, fps_num, fps_den, bitrate)
    }

    /// The same, coding HEVC. Unlike the AV1 seat this is **not** behind an
    /// opt-in: it is the default for an HEVC export, and what makes that safe is
    /// that the pictures it codes are intra-only, which is the same file the
    /// software seat writes ([`crate::export::Enc::open_hevc`]) and not the
    /// GPU-resetting inter path AV1's opt-in exists for.
    ///
    /// `None` on everything [`HwEncoder::open`] answers `None` to, on a plugin
    /// too old to export the symbol, on a GPU with no HEVC encode entrypoint,
    /// and below 384x384 -- the driver's own floor, stated in the plugin. The
    /// caller then takes the software intra encoder without a word about it.
    pub fn open_hevc(
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        bitrate: u64,
    ) -> Option<Self> {
        let plugin = enc_plugin()?;
        let open = plugin.open_hevc?;
        Self::opened(plugin, open, width, height, fps_num, fps_den, bitrate)
    }

    /// The null check both opens share, which is the plugin's whole way of
    /// saying "no".
    fn opened(
        plugin: &'static EncPlugin,
        open: extern "C" fn(u32, u32, u32, u32, u64) -> *mut c_void,
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        bitrate: u64,
    ) -> Option<Self> {
        let handle = open(width, height, fps_num, fps_den, bitrate);
        if handle.is_null() {
            return None;
        }
        Some(Self { plugin, handle })
    }

    /// Feeds one tightly packed I420 picture. `Ok(None)` means the encoder has
    /// not finished an access unit yet; the returned bytes are Annex-B and stay
    /// valid until the next call on this session.
    pub fn encode(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        width: u32,
        height: u32,
        force_key: bool,
    ) -> crate::Result<Option<&[u8]>> {
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        if y.len() < w * h || u.len() < cw * ch || v.len() < cw * ch {
            return Err("encode input is not a packed I420 frame".into());
        }
        let frame = VhFrame {
            y: y.as_ptr(),
            u: u.as_ptr(),
            v: v.as_ptr(),
            y_stride: w,
            u_stride: cw,
            v_stride: cw,
            width,
            height,
        };
        // SAFETY: `handle` is live, the planes outlive the call and their sizes
        // were just checked against the strides we declare.
        self.take(|plugin, handle, out, out_len| unsafe {
            (plugin.frame)(handle, &frame, force_key as i32, out, out_len)
        })
    }

    /// What a decoded buffer must look like to reach this encoder without a
    /// copy -- the surface size it codes in, plus the picture it was opened for
    /// -- or `None` where the plugin has no zero-copy door at all.
    pub fn dma_want(&self, width: u32, height: u32) -> Option<DmaWant> {
        let geometry = self.plugin.dma_geometry?;
        self.plugin.frame_dma?;
        let (mut coded_width, mut coded_height) = (0u32, 0u32);
        // SAFETY: `handle` is live and both destinations are writable.
        if unsafe { geometry(self.handle, &mut coded_width, &mut coded_height) } != 1 {
            return None;
        }
        Some(DmaWant {
            coded_width,
            coded_height,
            width,
            height,
        })
    }

    /// Feeds one picture the decoder left on the GPU. Same answer and same
    /// lifetime rule as [`HwEncoder::encode`]; an error here is a real one, the
    /// caller having asked [`HwEncoder::dma_want`] what this seat takes before
    /// the decoder handed the buffer over.
    pub fn encode_dma(&mut self, dma: &DmaFrame, force_key: bool) -> crate::Result<Option<&[u8]>> {
        let Some(frame_dma) = self.plugin.frame_dma else {
            return Err("hardware encoder has no zero-copy path".into());
        };
        let desc = dma.0;
        // SAFETY: `handle` is live and `desc` outlives the call; the buffer it
        // names is the decode session's, held for the caller by contract.
        self.take(|_, handle, out, out_len| unsafe {
            frame_dma(handle, &desc, force_key as i32, out, out_len)
        })
    }

    /// Flushes the encoder; call until it returns `Ok(None)`. Same lifetime rule
    /// as [`HwEncoder::encode`].
    pub fn drain(&mut self) -> crate::Result<Option<&[u8]>> {
        // SAFETY: `handle` is live.
        self.take(|plugin, handle, out, out_len| unsafe { (plugin.drain)(handle, out, out_len) })
    }

    /// Shared tail of `encode`/`drain`: run the call, then turn the plugin's
    /// (pointer, length) pair into a slice borrowed for as long as `&mut self`,
    /// which is exactly the "valid until the next call" contract.
    fn take(
        &mut self,
        call: impl FnOnce(&EncPlugin, *mut c_void, *mut *const u8, *mut usize) -> i32,
    ) -> crate::Result<Option<&[u8]>> {
        let mut ptr: *const u8 = std::ptr::null();
        let mut len: usize = 0;
        match call(self.plugin, self.handle, &mut ptr, &mut len) {
            0 => return Ok(None),
            1 => {}
            code => return Err(format!("hardware encode failed (code {code})").into()),
        }
        if ptr.is_null() || len == 0 {
            return Err("hardware encoder returned an empty access unit".into());
        }
        // SAFETY: the plugin promises `len` readable bytes at `ptr` until its
        // next call on this session, which `&mut self` prevents overlapping.
        Ok(Some(unsafe { std::slice::from_raw_parts(ptr, len) }))
    }
}

impl Drop for HwEncoder {
    fn drop(&mut self) {
        // SAFETY: `handle` came from `vh_enc_open` and is closed exactly once.
        unsafe { (self.plugin.close)(self.handle) }
    }
}

// As `HwSession`: used from one thread at a time (the export worker).
unsafe impl Send for HwEncoder {}
