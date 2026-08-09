//! Runtime-optional hardware decode plugin (`libengine_hw.so`).
//!
//! The plugin links libva/gbm/drm; this crate and the app binary must not, so
//! the only coupling is this C ABI plus a `dlopen`. Anything that goes wrong --
//! plugin missing, no VA-API runtime, no render node, unsupported stream --
//! leaves us with `None` and the caller falls back to the software decoder.

use std::ffi::{CString, c_char, c_void};
use std::path::Path;
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

fn load() -> Option<Plugin> {
    // Next to the executable first (cargo puts both in target/<profile>), then
    // whatever the dynamic linker's search path turns up.
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(LIB_NAME)));
    let candidates = beside_exe
        .as_deref()
        .into_iter()
        .chain(std::iter::once(Path::new(LIB_NAME)));

    for candidate in candidates {
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
