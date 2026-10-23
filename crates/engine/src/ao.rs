//! Audio output: a PulseAudio native-protocol playback stream ([`crate::pulse`]),
//! served by pipewire-pulse or PulseAudio, or a silent wall-clock stand-in.
//!
//! Anything that goes wrong -- no server, the server refuses us, an unusable
//! format -- leaves us with `None` and the caller plays on a wall clock. Nothing
//! here links a C library or needs `unsafe`.
//!
//! `VE_NO_AO=1` (and cargo-test binaries under `target/*/deps/`) open a silent
//! sink instead: the session, the clock and the feeder stay, and the server is
//! never touched. `VE_AO=1` forces the real server, which is what the ignored
//! `ao_output` suite needs.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Instant;

use crate::pulse::{self, Playback};

static DEVICES_OPENED: AtomicU64 = AtomicU64::new(0);

fn env_is(name: &str, value: &str) -> bool {
    crate::envcfg::var_os(name).is_some_and(|v| v == value)
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

/// Set by [`set_silent_output`]: this process opens silent sinks from then on.
static NO_AO: AtomicBool = AtomicBool::new(false);

/// Makes every audio session this process opens silent, the same as `VE_NO_AO=1`
/// does, without touching the process environment (which is `unsafe` to edit in
/// Rust 2024). One-way and process-wide; `VE_AO=1` still forces the real device.
pub fn set_silent_output() {
    NO_AO.store(true, Ordering::SeqCst);
}

fn silent_output() -> bool {
    if env_is("VE_AO", "1") {
        return false;
    }
    NO_AO.load(Ordering::SeqCst) || env_is("VE_NO_AO", "1") || cargo_test_binary()
}

/// An open playback stream. Dropping it stops playback and closes the session.
pub struct AoSession {
    backend: Backend,
}

enum Backend {
    Server(Playback),
    /// Wall-clock paced, samples discarded. Same contract as the server stream
    /// (paused until `set_active`, position is samples per channel) without a
    /// sink-input on the user's speakers.
    Silent(Silent),
}

struct Silent {
    rate: f64,
    /// Samples already accounted for; `-1` until the stream has run.
    base: AtomicI64,
    /// 0 = paused; otherwise milliseconds from [`epoch`] at the last resume.
    origin_ms: AtomicU64,
    /// The gain last pushed, so the silent sink answers the same question the
    /// real stream does: what is this stream actually multiplying by. Without it
    /// every gain read back as a shrug, and a device that had been re-armed at
    /// full volume looked exactly like one that had kept its level.
    gain: AtomicU32,
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
            gain: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
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
    /// `None` covers everything: no server, a refused client, unusable format.
    /// A silent pin still returns `Some`: the timeline keeps an audio clock.
    pub fn open(sample_rate: u32, channels: u32) -> Option<Self> {
        if silent_output() {
            return Some(Self {
                backend: Backend::Silent(Silent::new(sample_rate)),
            });
        }
        let playback = Playback::open(sample_rate, channels)?;
        DEVICES_OPENED.fetch_add(1, Ordering::Relaxed);
        Some(Self {
            backend: Backend::Server(playback),
        })
    }

    /// How many times this process has opened a real server stream. The silent
    /// sink does not count: that is the whole point of the pin.
    pub fn devices_opened() -> u64 {
        DEVICES_OPENED.load(Ordering::Relaxed)
    }

    /// Whether this session is the silent sink rather than a real stream --
    /// what the `VE_NO_AO` pin buys, and the one form of that claim a caller can
    /// own: [`AoSession::devices_opened`] is process-wide and moves under any
    /// other session this process opens.
    pub fn is_silent(&self) -> bool {
        matches!(self.backend, Backend::Silent(_))
    }

    /// Whether a server is there to talk to: its socket exists. Says nothing
    /// about whether it will take a stream -- only [`AoSession::open`] answers
    /// that, and it is cheap enough to be the real probe.
    pub fn probe() -> bool {
        pulse::server_path().is_some_and(|path| path.exists())
    }

    /// Queues interleaved samples, returning how many were accepted; a short
    /// count means the queue is full and the caller should come back later.
    /// `None` once the output has died (server gone).
    pub fn write(&mut self, samples: &[f32]) -> Option<usize> {
        match &mut self.backend {
            Backend::Silent(_) => Some(samples.len()),
            Backend::Server(playback) => playback.write(samples),
        }
    }

    /// Samples per channel actually played at the device -- the master clock.
    /// `None` until the stream has started.
    pub fn position(&self) -> Option<i64> {
        match &self.backend {
            Backend::Silent(s) => s.position(),
            Backend::Server(playback) => playback.position(),
        }
    }

    /// Says the last sample of this stream has been queued: the queue plays out
    /// as it is, and the silence after it is not counted against a decoder that
    /// has already finished.
    pub fn stream_ended(&self) {
        if let Backend::Server(playback) = &self.backend {
            playback.stream_ended();
        }
    }

    /// Pauses or resumes playback; while paused the position stays put.
    pub fn set_active(&self, active: bool) -> bool {
        match &self.backend {
            Backend::Silent(s) => {
                s.set_active(active);
                true
            }
            Backend::Server(playback) => playback.set_active(active),
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
            Backend::Silent(s) => {
                s.set_gain(gain);
                true
            }
            Backend::Server(playback) => playback.set_volume(gain),
        }
    }

    /// The gain this stream is actually running at. Always answered: the gain is
    /// applied in this process, so the stream knows it exactly.
    pub fn gain(&self) -> Option<f32> {
        match &self.backend {
            Backend::Silent(s) => Some(s.gain()),
            Backend::Server(playback) => Some(playback.gain()),
        }
    }

    /// Drops everything still queued, so the next samples written play right
    /// away -- what a seek needs. The played position keeps counting.
    pub fn flush(&self) -> bool {
        match &self.backend {
            Backend::Silent(_) => true,
            Backend::Server(playback) => playback.flush(),
        }
    }

    /// Starved callbacks so far, and the device position (samples per channel)
    /// the last of them was seen at -- `None` for a stream that has never run
    /// dry. Cheap enough to poll: it reads the counters the reader thread keeps.
    pub fn underruns(&self) -> (u64, Option<i64>) {
        match &self.backend {
            Backend::Silent(_) => (0, None),
            Backend::Server(playback) => playback.underruns(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AoSession;

    /// This module's counter assertions, one test at a time: the counters are
    /// process-wide, so two of these tests measuring at once would see each
    /// other's opens. Same shape as `waveform::counters_serial`, and for the
    /// same reason -- the counters stay readable, and one at a time is the
    /// whole point.
    fn counters_serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn a_silent_pin_never_opens_a_device() {
        let _serial = counters_serial();
        super::set_silent_output();
        let before = AoSession::devices_opened();
        let mut ao = AoSession::open(48_000, 2).expect("silent opens");
        // The pin's effect, on the session this test owns: no other test in
        // this binary can open one for it, and no absolute claim is made on a
        // counter *any* session in the process moves.
        assert!(
            ao.is_silent(),
            "the pin must route the open to the silent sink"
        );
        assert_eq!(
            AoSession::devices_opened(),
            before,
            "the silent open moved the process's device counter"
        );
        assert!(ao.set_volume(0.0));
        assert!(!ao.set_volume(1.5));
        assert!(ao.set_active(true));
        assert_eq!(ao.write(&[0.0; 64]), Some(64));
        assert!(ao.flush());
        ao.stream_ended();
        assert_eq!(ao.underruns(), (0, None));
    }
}
