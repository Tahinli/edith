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

struct Plugin {
    open_at: unsafe extern "C" fn(*const c_char, u32) -> *mut c_void,
    meta: unsafe extern "C" fn(*mut c_void, *mut VhMeta) -> i32,
    next_frame: unsafe extern "C" fn(*mut c_void, *mut VhFrame) -> i32,
    close: unsafe extern "C" fn(*mut c_void),
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
                    close: *lib.get(b"vh_close\0").ok()?,
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
            Ok(Some((
                std::slice::from_raw_parts(frame.y, w * h),
                std::slice::from_raw_parts(frame.u, cw * ch),
                std::slice::from_raw_parts(frame.v, cw * ch),
                frame.width,
                frame.height,
            )))
        }
    }
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
    frame:
        unsafe extern "C" fn(*mut c_void, *const VhFrame, i32, *mut *const u8, *mut usize) -> i32,
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
                    frame: *lib.get(b"vh_enc_frame\0").ok()?,
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
