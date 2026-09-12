//! `libvpx`, dlopen'd: the software VP8 decoder, and the reason a VP8 file
//! decodes on a machine whose GPU has no VP8 profile -- or whose GPU is
//! absent altogether.
//!
//! The plugin's own libva linkage, one level down: this plugin keeps no
//! DT_NEEDED on `libvpx` any more than the main binary does. The symbols are
//! resolved lazily, once, and a machine without the library is a machine
//! without the codec -- answered as `None` at [`vpx`] before any file is
//! opened, which is what [`crate::query_caps`] reports as the absence of
//! `CAP_VP8`.
//!
//! The extern declarations are hand-written against `vpx_decoder.h`,
//! `vpx_codec.h` and `vpx_image.h` as this soname has carried them since
//! libvpx 1.14: `VPX_DECODER_ABI_VERSION` 12. A libvpx from another ABI
//! answers `vpx_codec_dec_init_ver` with `VPX_CODEC_ABI_MISMATCH`, which
//! surfaces as a refused session -- loudly, in the eprintln, never as a
//! wrong picture. The struct layouts below are the ABI, not an assumption:
//! `vpx_codec_ctx_t`'s fields have not moved since the library's first
//! release, and `vpx_image_t`'s prefix is what every decoder in the field
//! reads.

use std::ffi::{CStr, c_char, c_int, c_long, c_uint, c_void};
use std::sync::LazyLock;

use libloading::Library;

/// `VPX_DECODER_ABI_VERSION`: `3 + VPX_CODEC_ABI_VERSION`, and
/// `VPX_CODEC_ABI_VERSION = 4 + VPX_IMAGE_ABI_VERSION` with
/// `VPX_IMAGE_ABI_VERSION = 5` -- the three headers this module mirrors.
const VPX_DECODER_ABI_VERSION: c_int = 12;

/// `VPX_CODEC_OK`, the one return code that is not a refusal.
const VPX_CODEC_OK: c_int = 0;

/// `VPX_DL_REALTIME`, the decode `deadline` argument -- which `vpx_decoder.h`
/// states this API ignores; passed because the type demands a value, and the
/// constant because it is the documented one.
const VPX_DL_REALTIME: c_long = 1;

/// The decoder context, `vpx_codec_ctx_t` field for field. Nothing here is
/// read: the struct exists so the context is laid out exactly as libvpx
/// lays it out, and so the `&mut` handoffs to the C side are the whole of
/// the story.
#[repr(C)]
pub struct VpxCtx {
    name: *const c_char,
    iface: *const c_void,
    err: c_int,
    err_detail: *const c_char,
    init_flags: c_uint,
    config: *const c_void,
    priv_: *mut c_void,
}

impl Default for VpxCtx {
    fn default() -> Self {
        // SAFETY: all-pointer/zero initialisation is what libvpx's own init
        // performs on the context before it fills it; `vpx_codec_dec_init_ver`
        // re-zeroes it anyway.
        unsafe { std::mem::zeroed() }
    }
}

/// `vpx_codec_dec_cfg_t`: thread count and a best-known size the caller may
/// leave at zero.
#[repr(C)]
struct VpxDecCfg {
    threads: c_uint,
    w: c_uint,
    h: c_uint,
}

/// `vpx_image_t` complete: the planes of one decoded picture and the strides
/// they sit at. The decoder owns the bytes -- they stay valid only until the
/// next `get_frame` or `decode` on the same context, which is exactly the
/// [`crate::VhFrame`] contract the caller already lives with.
#[repr(C)]
pub struct VpxImage {
    fmt: c_int,
    cs: c_int,
    range: c_int,
    w: c_uint,
    h: c_uint,
    bit_depth: c_uint,
    d_w: c_uint,
    d_h: c_uint,
    r_w: c_uint,
    r_h: c_uint,
    x_chroma_shift: c_uint,
    y_chroma_shift: c_uint,
    pub planes: [*mut u8; 4],
    pub stride: [c_int; 4],
    bps: c_int,
    user_priv: *mut c_void,
    img_data: *mut u8,
    img_data_owner: c_int,
    self_allocd: c_int,
    fb_priv: *mut c_void,
}

impl VpxImage {
    /// Displayed dimensions: what [`crate::VhFrame`] describes, not the
    /// macroblock-padded storage the planes actually cover.
    pub fn display(&self) -> (u32, u32) {
        (self.d_w, self.d_h)
    }
}

type Dx = unsafe extern "C" fn() -> *const c_void;
type DecInitVer =
    unsafe extern "C" fn(*mut VpxCtx, *const c_void, *const VpxDecCfg, c_uint, c_int) -> c_int;
type Decode = unsafe extern "C" fn(*mut VpxCtx, *const u8, c_uint, *mut c_void, c_long) -> c_int;
type GetFrame = unsafe extern "C" fn(*mut VpxCtx, *mut *const c_void) -> *mut VpxImage;
type Destroy = unsafe extern "C" fn(*mut VpxCtx) -> c_int;
type ErrorFn = unsafe extern "C" fn(*const VpxCtx) -> *const c_char;

/// The resolved symbols, held with the library that carries them -- exactly
/// the shape `engine::hw`'s `Plugin` has on the other side of the ABI.
pub struct Vpx {
    vp8_dx: Dx,
    dec_init_ver: DecInitVer,
    decode: Decode,
    get_frame: GetFrame,
    destroy: Destroy,
    error: ErrorFn,
    // Never dropped (the struct lives in a static), so the pointers above
    // stay valid for the life of the process.
    _lib: Library,
}

/// The library, loaded once per process. `None` -- no `libvpx.so` on this
/// machine, or one missing a symbol this module names -- is the one answer,
/// asked before any file is opened.
static VPX: LazyLock<Option<Vpx>> = LazyLock::new(load);

/// The loaded library, or `None` on a machine without one this module can
/// use. Asked before any file is opened.
pub fn vpx() -> Option<&'static Vpx> {
    VPX.as_ref()
}

fn load() -> Option<Vpx> {
    // The versioned soname first -- the name every distribution actually
    // ships -- then the unversioned development name for the odd link that
    // has it.
    for name in ["libvpx.so.9", "libvpx.so"] {
        let lib = match unsafe { Library::new(name) } {
            Ok(lib) => lib,
            Err(_) => continue,
        };
        // SAFETY: the symbols are type-checked against the declarations above;
        // a missing one unloads the library with the dropped closure and the
        // next candidate gets its turn.
        let vpx = unsafe {
            (|| {
                Some(Vpx {
                    vp8_dx: *lib.get(b"vpx_codec_vp8_dx\0").ok()?,
                    dec_init_ver: *lib.get(b"vpx_codec_dec_init_ver\0").ok()?,
                    decode: *lib.get(b"vpx_codec_decode\0").ok()?,
                    get_frame: *lib.get(b"vpx_codec_get_frame\0").ok()?,
                    destroy: *lib.get(b"vpx_codec_destroy\0").ok()?,
                    error: *lib.get(b"vpx_codec_error\0").ok()?,
                    _lib: lib,
                })
            })()
        };
        if vpx.is_some() {
            return vpx;
        }
    }
    None
}

impl Vpx {
    /// `vpx_codec_error`'s line about the context's last answer: what an init
    /// or decode refusal says instead of a bare `vpx_codec_err_t` number.
    fn last_error(&self, ctx: &VpxCtx) -> String {
        // SAFETY: `ctx` is a live libvpx context; the string it names is
        // static and read before anything else touches the context.
        let msg = unsafe { (self.error)(ctx) };
        if msg.is_null() {
            return "no description".to_string();
        }
        // SAFETY: non-null per the check above, NUL-terminated per libvpx.
        unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned()
    }

    /// The VP8 decoder interface, `vpx_codec_vp8_dx`: the argument every
    /// `dec_init_ver` on this context takes.
    fn iface(&self) -> *const c_void {
        // SAFETY: a pure lookup in libvpx's own tables.
        unsafe { (self.vp8_dx)() }
    }

    /// `vpx_codec_dec_init_ver` with the ABI version this module was written
    /// against -- the check a mismatched libvpx fails rather than a call it
    /// misreads.
    pub(super) fn dec_init(&self, ctx: &mut VpxCtx) -> Result<(), String> {
        let cfg = VpxDecCfg {
            threads: threads(),
            w: 0,
            h: 0,
        };
        // SAFETY: the context is ours and live for the session; `cfg` is read
        // only during the call; the version pins the ABI.
        let rc = unsafe { (self.dec_init_ver)(ctx, self.iface(), &cfg, 0, VPX_DECODER_ABI_VERSION) };
        match rc {
            VPX_CODEC_OK => Ok(()),
            _ => Err(format!("libvpx init failed: {}", self.last_error(ctx))),
        }
    }

    /// `vpx_codec_decode`: one access unit in. `VPX_DL_REALTIME` is the
    /// documented-but-ignored deadline.
    pub(super) fn decode(&self, ctx: &mut VpxCtx, au: &[u8]) -> Result<(), String> {
        // SAFETY: `au` outlives the call; the context is live and used from
        // the one thread the session lives on.
        let rc = unsafe {
            (self.decode)(
                ctx,
                au.as_ptr(),
                au.len() as c_uint,
                std::ptr::null_mut(),
                VPX_DL_REALTIME,
            )
        };
        match rc {
            VPX_CODEC_OK => Ok(()),
            _ => Err(format!("libvpx decode failed: {}", self.last_error(ctx))),
        }
    }

    /// `vpx_codec_decode` with a null, empty frame: the end-of-stream signal
    /// that releases whatever the decoder held back.
    pub(super) fn flush(&self, ctx: &mut VpxCtx) -> Result<(), String> {
        // SAFETY: as `decode`, with the nulls libvpx reads as "no more data".
        let rc = unsafe {
            (self.decode)(ctx, std::ptr::null(), 0, std::ptr::null_mut(), VPX_DL_REALTIME)
        };
        match rc {
            VPX_CODEC_OK => Ok(()),
            _ => Err(format!("libvpx flush failed: {}", self.last_error(ctx))),
        }
    }

    /// `vpx_codec_get_frame`: the next picture of the current iteration, or
    /// `None` when the iteration is done. The bytes belong to the decoder
    /// until the next `decode`/`flush`.
    pub(super) fn get_frame<'a>(
        &self,
        ctx: &'a mut VpxCtx,
        iter: &mut *const c_void,
    ) -> Option<&'a VpxImage> {
        // SAFETY: the context is live; the returned pointer is libvpx's own
        // image, borrowed for exactly as long as the lifetime above says.
        unsafe { (self.get_frame)(ctx, iter).as_ref() }
    }

    /// `vpx_codec_destroy`: the context's half of `VpxSession`'s `Drop`.
    pub(super) fn destroy(&self, ctx: &mut VpxCtx) {
        // SAFETY: called once per live context; libvpx frees what the context
        // holds, and a context whose init never completed reads as an
        // `INVALID_PARAM` refusal rather than a crash.
        unsafe { (self.destroy)(ctx) };
    }
}

/// VP8's row-based multithreaded decode stops paying past a handful of
/// threads -- the row sync costs what the parallelism buys -- so this is not
/// the machine's whole core count.
pub(super) fn threads() -> c_uint {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(4) as c_uint)
}
