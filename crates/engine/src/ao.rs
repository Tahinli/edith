//! Runtime-optional audio output plugin (`libengine_audio.so`).
//!
//! The plugin links libpipewire; this crate and the app binary must not, so the
//! only coupling is this C ABI plus a `dlopen`. Anything that goes wrong --
//! plugin missing, no PipeWire daemon, an unusable format -- leaves us with
//! `None` and the caller plays silently.
//!
//! `VE_NO_AO=1` (and cargo-test binaries under `target/*/deps/`) open a silent
//! sink instead: the session, the clock and the feeder stay, PipeWire is never
//! touched. `VE_AO=1` forces the real device, which is what the ignored
//! `ao_output` suite needs.

use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::time::Instant;

use libloading::Library;

const LIB_NAME: &str = "libengine_audio.so";
static DEVICES_OPENED: AtomicU64 = AtomicU64::new(0);

fn env_is(name: &str, value: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == value)
}

/// A cargo-test binary lives in `target/<profile>/deps/<name>-<hash>`. The
/// editor itself is `target/<profile>/edith`, which is what the harness
/// launches, so this never mutes a real session.
fn cargo_test_binary() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.ends_with("deps")))
        .unwrap_or(false)
}

fn silent_output() -> bool {
    if env_is("VE_AO", "1") {
        return false;
    }
    env_is("VE_NO_AO", "1") || cargo_test_binary()
}

struct Plugin {
    open: unsafe extern "C" fn(u32, u32) -> *mut c_void,
    write: unsafe extern "C" fn(*mut c_void, *const f32, usize) -> isize,
    position: unsafe extern "C" fn(*mut c_void) -> i64,
    set_active: unsafe extern "C" fn(*mut c_void, u32) -> i32,
    set_volume: unsafe extern "C" fn(*mut c_void, f32) -> i32,
    flush: unsafe extern "C" fn(*mut c_void) -> i32,
    underruns: unsafe extern "C" fn(*mut c_void, *mut i64) -> i64,
    /// Optional, unlike its neighbours: without it a plugin plays exactly as it
    /// did and only counts the tail of a played-out stream as lateness, which
    /// is a worse diagnostic rather than a worse session.
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
                    // Required on the same terms as the two above: the plugin
                    // ships from this build, and a measurement that silently
                    // reads zero underruns is worse than a run that says the
                    // plugin is the wrong one.
                    underruns: *lib.get(b"ao_underruns\0").ok()?,
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
    backend: Backend,
}

enum Backend {
    Plugin {
        plugin: &'static Plugin,
        handle: *mut c_void,
    },
    /// Wall-clock paced, samples discarded. Same contract as the plugin
    /// (paused until `set_active`, position is samples per channel) without
    /// a sink-input on the user's speakers.
    Silent(Silent),
}

struct Silent {
    rate: f64,
    /// Samples already accounted for; `-1` until the stream has run.
    base: AtomicI64,
    /// 0 = paused; otherwise milliseconds from [`epoch`] at the last resume.
    origin_ms: AtomicU64,
}

fn epoch() -> Instant {
    static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
    *EPOCH
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

impl Silent {
    fn new(sample_rate: u32) -> Self {
        Self {
            rate: f64::from(sample_rate),
            base: AtomicI64::new(-1),
            origin_ms: AtomicU64::new(0),
        }
    }

    fn extra(&self) -> i64 {
        match self.origin_ms.load(Ordering::Relaxed) {
            0 => 0,
            origin => {
                let ms = now_ms().saturating_sub(origin) as f64;
                (ms / 1000.0 * self.rate).round() as i64
            }
        }
    }

    fn position(&self) -> Option<i64> {
        let base = self.base.load(Ordering::Relaxed);
        let extra = self.extra();
        if base < 0 && self.origin_ms.load(Ordering::Relaxed) == 0 {
            None
        } else {
            Some(base.max(0) + extra.max(0))
        }
    }

    fn set_active(&self, active: bool) {
        if active {
            if self.origin_ms.load(Ordering::Relaxed) == 0 {
                self.base
                    .compare_exchange(-1, 0, Ordering::Relaxed, Ordering::Relaxed)
                    .ok();
                self.origin_ms.store(now_ms().max(1), Ordering::Relaxed);
            }
        } else {
            let origin = self.origin_ms.swap(0, Ordering::Relaxed);
            if origin != 0 {
                let ms = now_ms().saturating_sub(origin) as f64;
                let extra = (ms / 1000.0 * self.rate).round() as i64;
                self.base
                    .compare_exchange(-1, 0, Ordering::Relaxed, Ordering::Relaxed)
                    .ok();
                self.base.fetch_add(extra.max(0), Ordering::Relaxed);
            }
        }
    }
}

impl AoSession {
    /// Connects a playback stream for interleaved f32 at `sample_rate`.
    /// `None` covers everything: no plugin, no daemon, unusable format.
    /// A silent pin still returns `Some`: the timeline keeps an audio clock.
    pub fn open(sample_rate: u32, channels: u32) -> Option<Self> {
        if silent_output() {
            return Some(Self {
                backend: Backend::Silent(Silent::new(sample_rate)),
            });
        }
        let plugin = plugin()?;
        // SAFETY: plain scalars in, a session pointer or null out.
        let handle = unsafe { (plugin.open)(sample_rate, channels) };
        if handle.is_null() {
            return None;
        }
        DEVICES_OPENED.fetch_add(1, Ordering::Relaxed);
        Some(Self {
            backend: Backend::Plugin { plugin, handle },
        })
    }

    /// How many times this process has opened a real PipeWire stream. The
    /// silent sink does not count: that is the whole point of the pin.
    pub fn devices_opened() -> u64 {
        DEVICES_OPENED.load(Ordering::Relaxed)
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
        match &mut self.backend {
            Backend::Silent(_) => Some(samples.len()),
            Backend::Plugin { plugin, handle } => {
                // SAFETY: `handle` is live and `samples` is a valid slice for the call.
                match unsafe { (plugin.write)(*handle, samples.as_ptr(), samples.len()) } {
                    n if n < 0 => None,
                    n => Some(n as usize),
                }
            }
        }
    }

    /// Samples per channel actually played at the device -- the master clock.
    /// `None` until the stream has run its first cycle.
    pub fn position(&self) -> Option<i64> {
        match &self.backend {
            Backend::Silent(s) => s.position(),
            Backend::Plugin { plugin, handle } => {
                // SAFETY: `handle` came from `ao_open` and is still open.
                match unsafe { (plugin.position)(*handle) } {
                    n if n < 0 => None,
                    n => Some(n),
                }
            }
        }
    }

    /// Says the last sample of this stream has been queued: the ring plays out
    /// as it is, and the silence after it is not counted against a decoder that
    /// has already finished. Nothing at all on a plugin without the symbol.
    pub fn stream_ended(&self) {
        if let Backend::Plugin { plugin, handle } = &self.backend {
            if let Some(ended) = plugin.stream_ended {
                // SAFETY: `handle` came from `ao_open` and is still open.
                unsafe { ended(*handle) };
            }
        }
    }

    /// Pauses or resumes playback; while paused the position stays put.
    pub fn set_active(&self, active: bool) -> bool {
        match &self.backend {
            Backend::Silent(s) => {
                s.set_active(active);
                true
            }
            Backend::Plugin { plugin, handle } => {
                // SAFETY: `handle` came from `ao_open` and is still open.
                unsafe { (plugin.set_active)(*handle, active as u32) == 0 }
            }
        }
    }

    /// Sets the output gain, 0.0 (silent) to 1.0 (as written): the editor's
    /// volume and its mute are the same knob by the time they get here. The
    /// clock is unaffected -- a silenced stream still plays, so the picture
    /// keeps running against it. `false` for a gain outside the range.
    pub fn set_volume(&self, gain: f32) -> bool {
        if !(0.0..=1.0).contains(&gain) {
            return false;
        }
        match &self.backend {
            Backend::Silent(_) => true,
            Backend::Plugin { plugin, handle } => {
                // SAFETY: `handle` came from `ao_open` and is still open.
                unsafe { (plugin.set_volume)(*handle, gain) == 0 }
            }
        }
    }

    /// Drops everything still queued, so the next samples written play right
    /// away -- what a seek needs. The played position keeps counting.
    pub fn flush(&self) -> bool {
        match &self.backend {
            Backend::Silent(_) => true,
            Backend::Plugin { plugin, handle } => {
                // SAFETY: `handle` came from `ao_open` and is still open.
                unsafe { (plugin.flush)(*handle) == 0 }
            }
        }
    }

    /// Starved callbacks so far, and the device position (samples per channel)
    /// the last of them was seen at -- `None` for a stream that has never run
    /// dry. Two atomic loads inside the plugin, so it is cheap enough to poll
    /// and it never touches the RT thread's path.
    pub fn underruns(&self) -> (u64, Option<i64>) {
        match &self.backend {
            Backend::Silent(_) => (0, None),
            Backend::Plugin { plugin, handle } => {
                let mut last = -1i64;
                // SAFETY: `handle` came from `ao_open`, and `last` is a live i64 of ours.
                let count = unsafe { (plugin.underruns)(*handle, &raw mut last) };
                (count.max(0) as u64, (last >= 0).then_some(last))
            }
        }
    }
}

impl Drop for AoSession {
    fn drop(&mut self) {
        if let Backend::Plugin { plugin, handle } = &self.backend {
            // SAFETY: `handle` came from `ao_open` and is closed exactly once.
            unsafe { (plugin.close)(*handle) }
        }
    }
}

// The session is used from one thread at a time (the audio feeder) and never
// shared; the PipeWire state behind it is not `Sync`.
unsafe impl Send for AoSession {}

#[cfg(test)]
mod tests {
    use super::AoSession;

    #[test]
    fn a_silent_pin_never_opens_a_device() {
        // SAFETY: this is the engine's own unit-test process.
        unsafe { std::env::set_var("VE_NO_AO", "1") };
        let mut ao = AoSession::open(48_000, 2).expect("silent opens");
        assert_eq!(AoSession::devices_opened(), 0);
        assert!(ao.set_volume(0.0));
        assert!(!ao.set_volume(1.5));
        assert!(ao.set_active(true));
        assert_eq!(ao.write(&[0.0; 64]), Some(64));
        assert!(ao.flush());
        ao.stream_ended();
        assert_eq!(ao.underruns(), (0, None));
    }
}
