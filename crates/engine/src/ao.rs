//! Runtime-optional audio output plugin (`libengine_audio.so`).
//!
//! The plugin links libpipewire; this crate and the app binary must not, so the
//! only coupling is this C ABI plus a `dlopen`. Anything that goes wrong --
//! plugin missing, no PipeWire daemon, an unusable format -- leaves us with
//! `None` and the caller plays silently.

use std::ffi::c_void;
use std::path::Path;
use std::sync::OnceLock;

use libloading::Library;

const LIB_NAME: &str = "libengine_audio.so";

struct Plugin {
    open: unsafe extern "C" fn(u32, u32) -> *mut c_void,
    write: unsafe extern "C" fn(*mut c_void, *const f32, usize) -> isize,
    position: unsafe extern "C" fn(*mut c_void) -> i64,
    set_active: unsafe extern "C" fn(*mut c_void, u32) -> i32,
    set_volume: unsafe extern "C" fn(*mut c_void, f32) -> i32,
    flush: unsafe extern "C" fn(*mut c_void) -> i32,
    /// Optional, unlike its neighbours: this one is a *counter*, and a plugin
    /// too old to have it is a session that plays perfectly and cannot say how
    /// many quanta it starved for. Rejecting the whole plugin over that would
    /// trade the sound for the diagnostic.
    underruns: Option<unsafe extern "C" fn(*mut c_void) -> i64>,
    /// Optional for [`Plugin::underruns`]'s reason: without it a plugin plays
    /// exactly as it did, it only counts the tail of a stream as lateness.
    stream_ended: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
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
        // the signatures this crate shares with it.
        let lib = match unsafe { Library::new(candidate) } {
            Ok(lib) => lib,
            Err(_) => continue,
        };
        let plugin = unsafe {
            (|| {
                Some(Plugin {
                    open: *lib.get(b"ao_open\0").ok()?,
                    write: *lib.get(b"ao_write\0").ok()?,
                    position: *lib.get(b"ao_position\0").ok()?,
                    set_active: *lib.get(b"ao_set_active\0").ok()?,
                    // Required, so a plugin predating seek is rejected whole
                    // and we play muted rather than half-working. Volume is
                    // required on the same terms: the two ship from one build,
                    // and a mute button that does nothing is worse than a run
                    // with no sound at all, which at least says so.
                    set_volume: *lib.get(b"ao_set_volume\0").ok()?,
                    flush: *lib.get(b"ao_flush\0").ok()?,
                    underruns: lib.get(b"ao_underruns\0").ok().map(|f| *f),
                    stream_ended: lib.get(b"ao_stream_ended\0").ok().map(|f| *f),
                    close: *lib.get(b"ao_close\0").ok()?,
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

/// An open playback stream. Dropping it stops playback and closes the session.
pub struct AoSession {
    plugin: &'static Plugin,
    handle: *mut c_void,
}

impl AoSession {
    /// Connects a playback stream for interleaved f32 at `sample_rate`.
    /// `None` covers everything: no plugin, no daemon, unusable format.
    pub fn open(sample_rate: u32, channels: u32) -> Option<Self> {
        let plugin = plugin()?;
        // SAFETY: plain scalars in, a session pointer or null out.
        let handle = unsafe { (plugin.open)(sample_rate, channels) };
        if handle.is_null() {
            return None;
        }
        Some(Self { plugin, handle })
    }

    /// Whether the plugin itself is loadable. Says nothing about the daemon --
    /// only [`AoSession::open`] answers that, and it is cheap enough to be the
    /// real probe.
    pub fn probe() -> bool {
        plugin().is_some()
    }

    /// Queues interleaved samples, returning how many were accepted; a short
    /// count means the ring is full and the caller should come back later.
    /// `None` once the output has died (daemon gone).
    pub fn write(&mut self, samples: &[f32]) -> Option<usize> {
        // SAFETY: `handle` is live and `samples` is a valid slice for the call.
        match unsafe { (self.plugin.write)(self.handle, samples.as_ptr(), samples.len()) } {
            n if n < 0 => None,
            n => Some(n as usize),
        }
    }

    /// Samples per channel actually played at the device -- the master clock.
    /// `None` until the stream has run its first cycle.
    pub fn position(&self) -> Option<i64> {
        // SAFETY: `handle` came from `ao_open` and is still open.
        match unsafe { (self.plugin.position)(self.handle) } {
            n if n < 0 => None,
            n => Some(n),
        }
    }

    /// Says the last sample of this stream has been queued: the ring plays out
    /// as it is, and the silence after it is not counted against a decoder that
    /// has already finished. Nothing at all on a plugin without the symbol.
    pub fn stream_ended(&self) {
        if let Some(ended) = self.plugin.stream_ended {
            // SAFETY: `handle` came from `ao_open` and is still open.
            unsafe { ended(self.handle) };
        }
    }

    /// Quanta the device filled with silence for want of samples, since the
    /// stream opened: what a benchmark reads to say the decoder kept up.
    /// `None` from a plugin that does not carry the counter.
    pub fn underruns(&self) -> Option<u64> {
        // SAFETY: `handle` came from `ao_open` and is still open.
        let n = unsafe { (self.plugin.underruns?)(self.handle) };
        (n >= 0).then_some(n as u64)
    }

    /// Pauses or resumes playback; while paused the position stays put.
    pub fn set_active(&self, active: bool) -> bool {
        // SAFETY: `handle` came from `ao_open` and is still open.
        unsafe { (self.plugin.set_active)(self.handle, active as u32) == 0 }
    }

    /// Sets the output gain, 0.0 (silent) to 1.0 (as written): the editor's
    /// volume and its mute are the same knob by the time they get here. The
    /// clock is unaffected -- a silenced stream still plays, so the picture
    /// keeps running against it. `false` for a gain outside the range.
    pub fn set_volume(&self, gain: f32) -> bool {
        // SAFETY: `handle` came from `ao_open` and is still open.
        unsafe { (self.plugin.set_volume)(self.handle, gain) == 0 }
    }

    /// Drops everything still queued, so the next samples written play right
    /// away -- what a seek needs. The played position keeps counting.
    pub fn flush(&self) -> bool {
        // SAFETY: `handle` came from `ao_open` and is still open.
        unsafe { (self.plugin.flush)(self.handle) == 0 }
    }
}

impl Drop for AoSession {
    fn drop(&mut self) {
        // SAFETY: `handle` came from `ao_open` and is closed exactly once.
        unsafe { (self.plugin.close)(self.handle) }
    }
}

// The session is used from one thread at a time (the audio feeder) and never
// shared; the PipeWire state behind it is not `Sync`.
unsafe impl Send for AoSession {}
