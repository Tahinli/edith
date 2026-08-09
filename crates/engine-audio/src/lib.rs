//! PipeWire playback, shipped as a `dlopen`-able plugin so the main binary
//! never gets a DT_NEEDED on libpipewire. Every entry point is `extern "C"`,
//! catches unwinds and reports failure as a null pointer or a negative code:
//! the caller's contract is "any failure means the app runs muted".
//!
//! ponytail: that guarantee rests on `panic = "unwind"`. Building this crate with
//! `panic = "abort"` would turn a PipeWire bug into a killed app; the upgrade
//! path is running the output in a child process instead of in-process.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::{properties::properties, spa};
use spa::pod::Pod;

/// How long the loop thread waits for the daemon to accept the stream before
/// the open counts as failed. Negotiation is a couple of graph cycles when it
/// works at all, so this only ever fires on a broken setup.
const READY_TIMEOUT: Duration = Duration::from_secs(1);
/// Belt and braces around the whole open: no PipeWire call on this path is
/// supposed to block, but `ao_open` must never hang the caller.
const OPEN_TIMEOUT: Duration = Duration::from_secs(2);
/// Loop iteration timeout on the audio thread; also the worst-case latency of
/// an `ao_set_active` taking effect.
const POLL: Duration = Duration::from_millis(10);
/// Frames we hand over per callback, and the latency we ask the graph for.
/// Left to itself PipeWire gives us a buffer the size of the sink's (measured:
/// 12288 frames, 256 ms) and calls us once per buffer, so the position clock --
/// which only moves in the callback -- would step 256 ms at a time and land
/// that coarseness straight in the A/V sync. 1024 frames is ~23 ms, comfortably
/// inside a video frame; measured granularity is then one graph quantum.
///
/// ponytail: a graph running a quantum longer than this would consume more per
/// cycle than we deliver and starve. The fix is the crate's `v0_3_49` feature,
/// whose `pw_buffer.requested` says exactly how many frames the graph wants --
/// a Cargo.toml change, so not this slice.
const QUANTUM: u32 = 1024;

/// Lock-free SPSC ring of f32 samples (interleaved, one producer: `ao_write`,
/// one consumer: the RT process callback). Samples live in atomics rather than
/// behind a lock so the RT thread never waits on the decoder thread.
struct Ring {
    /// Power-of-two length, so the monotonic counters below can be masked and
    /// still index correctly across a `usize` wrap.
    slots: Box<[AtomicU32]>,
    write: AtomicUsize,
    read: AtomicUsize,
}

impl Ring {
    fn new(capacity: usize) -> Self {
        let len = capacity.next_power_of_two();
        Self {
            slots: (0..len).map(|_| AtomicU32::new(0)).collect(),
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
        }
    }

    /// Appends as much of `src` as fits, returning how many samples were taken.
    fn push(&self, src: &[f32]) -> usize {
        let write = self.write.load(Ordering::Relaxed);
        let used = write.wrapping_sub(self.read.load(Ordering::Acquire));
        let n = src.len().min(self.slots.len() - used);
        let mask = self.slots.len() - 1;
        for (i, sample) in src[..n].iter().enumerate() {
            self.slots[write.wrapping_add(i) & mask].store(sample.to_bits(), Ordering::Relaxed);
        }
        self.write.store(write.wrapping_add(n), Ordering::Release);
        n
    }

    /// Pops up to `want` samples as little-endian f32 into `dst`, returning how
    /// many were available. `dst` must hold `want * 4` bytes.
    fn pop(&self, dst: &mut [u8], want: usize) -> usize {
        let read = self.read.load(Ordering::Relaxed);
        let avail = self.write.load(Ordering::Acquire).wrapping_sub(read);
        let n = want.min(avail);
        let mask = self.slots.len() - 1;
        for i in 0..n {
            let bits = self.slots[read.wrapping_add(i) & mask].load(Ordering::Relaxed);
            dst[i * 4..i * 4 + 4].copy_from_slice(&bits.to_le_bytes());
        }
        self.read.store(read.wrapping_add(n), Ordering::Release);
        n
    }
}

/// The consumer half of a flush: drops everything queued, then pops as usual.
/// Doing it here rather than in `ao_flush` keeps the ring single-consumer --
/// only the RT thread ever moves `read` -- and means a flush asked for while
/// paused (no callbacks running) still lands before the next pop, so the stale
/// samples can never be heard.
/// The producer half of a flush: while the flag is pending, the ring still
/// holds pre-flush audio waiting to be discarded, and anything accepted now
/// would queue BEHIND that discard -- with no RT callbacks running (paused
/// seek) the next callback would then drain the new stream's head along with
/// the stale tail. Refuse instead; the writer's retry loop parks until the
/// RT thread consumes the flag.
fn accept(flush: &AtomicBool, ring: &Ring, samples: &[f32]) -> usize {
    if flush.load(Ordering::Acquire) {
        return 0;
    }
    ring.push(samples)
}

fn consume(flush: &AtomicBool, ring: &Ring, dst: &mut [u8], want: usize) -> usize {
    if flush.swap(false, Ordering::Acquire) {
        ring.read
            .store(ring.write.load(Ordering::Acquire), Ordering::Release);
    }
    ring.pop(dst, want)
}

/// Everything the RT thread and the caller's thread both touch.
struct Shared {
    ring: Ring,
    /// Samples per channel played at the device, -1 until the first callback.
    position: AtomicI64,
    /// Set once the daemon has accepted the stream.
    ready: AtomicBool,
    /// Stream died (daemon gone, node removed); every later write fails.
    dead: AtomicBool,
    /// Asked for by `ao_flush`, honoured by the next process callback.
    flush: AtomicBool,
    /// Set by `ao_set_active(true)`, cleared by the next process callback: the
    /// graph clock keeps ticking while the stream is inactive, and that time
    /// was never played.
    resumed: AtomicBool,
    underruns: AtomicU64,
}

enum Ctl {
    Active(bool),
    Quit,
}

fn pw_init() {
    // Refcounted inside libpipewire, but a plugin has no safe point to call
    // `pw::deinit`, so we init exactly once and never tear down.
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(pw::init);
}

/// Builds the stream, reports readiness through `ready`, then owns the PipeWire
/// loop until told to quit. Everything PipeWire lives in this one frame: the
/// smart pointers are `Rc`-flavoured and the borrow chain (listener -> stream ->
/// core -> context -> loop) is what enforces the destruction order.
fn run(
    rate: u32,
    channels: usize,
    shared: Arc<Shared>,
    ready: mpsc::Sender<()>,
    ctl: mpsc::Receiver<Ctl>,
) -> Result<(), pw::Error> {
    pw_init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    // Fails synchronously when there is no daemon to talk to: the clean probe.
    let core = context.connect_rc(None)?;

    let stream = pw::stream::StreamBox::new(
        &core,
        "video-editor",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            // Deliberately no MEDIA_ROLE: the session manager keys saved stream
            // volume/mute by media.role before anything else, so declaring
            // "Movie" put us in one bucket with every other video player on the
            // box -- a mute somebody set on one of them last month came back as
            // "edith has no sound", with nothing in the app to undo it. Without
            // a role we get our own bucket, keyed by name, and start audible.
            // Restoring stays ON: it is also what initialises the channel
            // volumes, and a stream that opts out of it comes up at -inf dB.
            *pw::keys::AUDIO_CHANNELS => channels.to_string(),
            // Asks the graph to schedule us at our own quantum, which is what
            // keeps the fill below from under-delivering on a lazy sink.
            *pw::keys::NODE_LATENCY => format!("{QUANTUM}/{rate}"),
        },
    )?;

    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed({
            let shared = shared.clone();
            move |_, (), _old, new| match new {
                pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming => {
                    shared.ready.store(true, Ordering::Relaxed)
                }
                pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected => {
                    shared.dead.store(true, Ordering::Relaxed)
                }
                pw::stream::StreamState::Connecting => {}
            }
        })
        .process({
            let shared = shared.clone();
            let stride = 4 * channels;
            // Graph ticks that elapsed while the stream was inactive, and the
            // last raw reading they are measured against.
            let (mut skew, mut last_raw) = (0i64, 0i64);
            move |stream, ()| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else {
                    return;
                };
                let n_frames = match data.data() {
                    Some(slice) => {
                        // Only a quantum per callback, however big the buffer
                        // is: everything we queue here is latency the clock,
                        // the pause and a future seek all have to wait out.
                        let frames = (slice.len() / stride).min(QUANTUM as usize);
                        let want = frames * channels;
                        let got = consume(&shared.flush, &shared.ring, slice, want);
                        if got < want {
                            // Underrun: silence rather than stale samples, and
                            // the clock below keeps running -- the device plays
                            // that silence, so it really is elapsed time.
                            slice[got * 4..want * 4].fill(0);
                            shared.underruns.fetch_add(1, Ordering::Relaxed);
                        }
                        frames
                    }
                    None => 0,
                };
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = stride as _;
                *chunk.size_mut() = (stride * n_frames) as _;

                // The A/V master clock: what the device has actually played,
                // which is the stream position minus everything still in flight.
                if let Ok(time) = stream.time() {
                    let ticks = time.ticks() as i64 - time.delay();
                    let (num, denom) = (time.rate().num as i128, time.rate().denom as i128);
                    // `ticks` counts in the stream's own rate; convert to ours.
                    let played = if denom > 0 {
                        (ticks as i128 * num * rate as i128 / denom) as i64
                    } else {
                        ticks
                    };
                    // The graph clock runs whether or not this stream does, so a
                    // pause shows up here as a step. Nothing was played during
                    // it: fold it into the skew instead of the position, or the
                    // pause leaks into the caller's timeline.
                    if shared.resumed.swap(false, Ordering::Relaxed) {
                        skew += played - last_raw;
                    }
                    last_raw = played;
                    shared
                        .position
                        .store((played - skew).max(0), Ordering::Relaxed);
                }
            }
        })
        .register()?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(rate);
    audio_info.set_channels(channels as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    match channels {
        1 => position[0] = libspa_sys::SPA_AUDIO_CHANNEL_MONO,
        2 => {
            position[0] = libspa_sys::SPA_AUDIO_CHANNEL_FL;
            position[1] = libspa_sys::SPA_AUDIO_CHANNEL_FR;
        }
        // Anything else keeps UNKNOWN positions and lets the daemon map them.
        _ => {}
    }
    audio_info.set_position(position);

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: libspa_sys::SPA_TYPE_OBJECT_Format,
            id: libspa_sys::SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        }),
    )
    .map_err(|_| pw::Error::CreationFailed)?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or(pw::Error::CreationFailed)?];

    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    // `connect` only starts negotiation, so a bad format or a daemon that
    // refuses us still shows up here rather than as silent nothing later.
    let deadline = Instant::now() + READY_TIMEOUT;
    while !shared.ready.load(Ordering::Relaxed) {
        if shared.dead.load(Ordering::Relaxed) || Instant::now() >= deadline {
            return Err(pw::Error::CreationFailed);
        }
        mainloop.loop_().iterate(pw::loop_::Timeout::Finite(POLL));
    }
    let _ = ready.send(());

    // Driving the loop by hand instead of `mainloop.run()` + a PipeWire channel:
    // loop callbacks must be `'static` and the stream borrows the core, so a
    // callback cannot touch the stream. Iterating lets us call `set_active` from
    // this very frame. With RT_PROCESS the audio callback runs on the RT thread
    // regardless, so this loop only handles negotiation and control.
    'run: loop {
        mainloop.loop_().iterate(pw::loop_::Timeout::Finite(POLL));
        loop {
            match ctl.try_recv() {
                Ok(Ctl::Active(active)) => {
                    let _ = stream.set_active(active);
                }
                // Disconnected means the session was dropped without a Quit.
                Ok(Ctl::Quit) | Err(mpsc::TryRecvError::Disconnected) => break 'run,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
    }
    let _ = stream.disconnect();
    Ok(())
}

struct Session {
    shared: Arc<Shared>,
    ctl: mpsc::Sender<Ctl>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.ctl.send(Ctl::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let underruns = self.shared.underruns.load(Ordering::Relaxed);
        if underruns > 0 {
            eprintln!("engine_audio: {underruns} underruns");
        }
    }
}

/// Opens a playback stream for `sample_rate` Hz, `channels` channels of
/// interleaved f32. Returns null on any failure at all: no libpipewire, no
/// daemon, an unusable format.
#[unsafe(no_mangle)]
pub extern "C" fn ao_open(sample_rate: u32, channels: u32) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if !(8_000..=768_000).contains(&sample_rate)
            || channels == 0
            || channels as usize > spa::param::audio::MAX_CHANNELS
        {
            return std::ptr::null_mut();
        }
        pw_init();

        let shared = Arc::new(Shared {
            // One second of audio: enough that a stalled decoder is audible as
            // an underrun rather than papered over by a huge buffer.
            ring: Ring::new(sample_rate as usize * channels as usize),
            position: AtomicI64::new(-1),
            ready: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            flush: AtomicBool::new(false),
            resumed: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
        });
        let (ready_tx, ready_rx) = mpsc::channel();
        let (ctl_tx, ctl_rx) = mpsc::channel();
        let thread = std::thread::spawn({
            let shared = shared.clone();
            move || {
                if let Err(e) = run(sample_rate, channels as usize, shared, ready_tx, ctl_rx) {
                    eprintln!("engine_audio: {e}");
                }
            }
        });

        // A failed setup drops `ready_tx`, so this returns without waiting out
        // the timeout; the timeout only covers a PipeWire call that blocks
        // against its contract, and then we leave the thread to its own exit.
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(()) => Box::into_raw(Box::new(Session {
                shared,
                ctl: ctl_tx,
                thread: Some(thread),
            })) as *mut c_void,
            Err(_) => {
                let _ = ctl_tx.send(Ctl::Quit);
                std::ptr::null_mut()
            }
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Queues up to `n` interleaved f32 samples, returning how many were accepted:
/// short counts mean the ring is full. Never blocks. -1 once the stream is dead.
///
/// # Safety
/// `session` must come from [`ao_open`] and `samples` must point at `n` f32s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ao_write(session: *mut c_void, samples: *const f32, n: usize) -> isize {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() || samples.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session and `n` readable samples.
        let session = unsafe { &*(session as *const Session) };
        if session.shared.dead.load(Ordering::Relaxed) {
            return -1;
        }
        let samples = unsafe { std::slice::from_raw_parts(samples, n) };
        accept(&session.shared.flush, &session.shared.ring, samples) as isize
    }))
    .unwrap_or(-1)
}

/// Samples per channel actually played at the device, or -1 while unknown.
///
/// # Safety
/// `session` must come from [`ao_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ao_position(session: *mut c_void) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session.
        let session = unsafe { &*(session as *const Session) };
        session.shared.position.load(Ordering::Relaxed)
    }))
    .unwrap_or(-1)
}

/// Pauses (0) or resumes (non-zero) playback. Paused freezes the position.
/// 0 on success, negative on failure.
///
/// # Safety
/// `session` must come from [`ao_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ao_set_active(session: *mut c_void, active: u32) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session.
        let session = unsafe { &*(session as *const Session) };
        if active != 0 {
            session.shared.resumed.store(true, Ordering::Relaxed);
        }
        match session.ctl.send(Ctl::Active(active != 0)) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    }))
    .unwrap_or(-1)
}

/// Drops every queued sample, so playback resumes from whatever is written
/// next: what a seek needs. Takes effect on the next process callback (or the
/// first one after a resume), and leaves the played position alone -- the
/// device keeps counting, the caller re-bases against it.
/// 0 on success, negative on failure.
///
/// # Safety
/// `session` must come from [`ao_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ao_flush(session: *mut c_void) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        if session.is_null() {
            return -1;
        }
        // SAFETY: caller-guaranteed live session.
        let session = unsafe { &*(session as *const Session) };
        session.shared.flush.store(true, Ordering::Release);
        0
    }))
    .unwrap_or(-1)
}

/// Stops playback and releases the session. Safe to call with null.
///
/// # Safety
/// `session` must come from [`ao_open`] and must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ao_close(session: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !session.is_null() {
            // SAFETY: pointer came from `Box::into_raw` in `ao_open`.
            drop(unsafe { Box::from_raw(session as *mut Session) });
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::{AtomicBool, Ordering, Ring, accept, consume};

    /// The paused-seek ordering: a flush requested with no RT callbacks running
    /// must not let the writer queue new audio behind the pending discard.
    #[test]
    fn pending_flush_refuses_writes_until_consumed() {
        let ring = Ring::new(4);
        let flush = AtomicBool::new(false);
        let mut out = [0u8; 16];

        // Stale pre-seek audio sits in the ring; a flush is requested while
        // paused (no callback runs yet).
        assert_eq!(accept(&flush, &ring, &[1.0, 2.0]), 2);
        flush.store(true, Ordering::Release);

        // The new stream's feeder tries to write: refused, ring untouched.
        assert_eq!(accept(&flush, &ring, &[3.0, 4.0]), 0);

        // First post-resume callback consumes the flag and drains the stale
        // tail -- and ONLY the stale tail, because nothing was queued behind it.
        assert_eq!(consume(&flush, &ring, &mut out, 2), 0);

        // Writer unparks; the new audio flows intact.
        assert_eq!(accept(&flush, &ring, &[3.0, 4.0]), 2);
        assert_eq!(consume(&flush, &ring, &mut out, 2), 2);
        assert_eq!(&out[..4], &3.0f32.to_le_bytes());
    }

    #[test]
    fn ring_wraps_fills_and_starves() {
        let ring = Ring::new(4);
        let mut out = [0u8; 24];

        // Short write, then read it back as little-endian f32.
        assert_eq!(ring.push(&[1.0, 2.0]), 2);
        assert_eq!(ring.pop(&mut out, 2), 2);
        assert_eq!(&out[..4], &1.0f32.to_le_bytes());
        assert_eq!(&out[4..8], &2.0f32.to_le_bytes());

        // Full ring accepts only what fits, and the tail wraps the mask.
        assert_eq!(ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 4);
        assert_eq!(ring.push(&[9.0]), 0);
        assert_eq!(ring.pop(&mut out, 6), 4);
        assert_eq!(&out[12..16], &4.0f32.to_le_bytes());

        // Empty ring starves instead of replaying stale samples.
        assert_eq!(ring.pop(&mut out, 4), 0);
    }

    #[test]
    fn flush_drops_queued_samples() {
        let ring = Ring::new(4);
        let flush = AtomicBool::new(false);
        let mut out = [0u8; 16];

        // No flush pending: an ordinary pop.
        assert_eq!(ring.push(&[1.0, 2.0]), 2);
        assert_eq!(consume(&flush, &ring, &mut out, 2), 2);

        // Flushed: the queued samples are gone, so the callback gets nothing
        // and fills silence, and the flag is one-shot.
        assert_eq!(ring.push(&[3.0, 4.0]), 2);
        flush.store(true, Ordering::Release);
        assert_eq!(consume(&flush, &ring, &mut out, 2), 0);
        assert!(!flush.load(Ordering::Acquire));

        // Ring is empty, not corrupt: the next write plays as usual.
        assert_eq!(ring.push(&[5.0, 6.0, 7.0, 8.0]), 4);
        assert_eq!(consume(&flush, &ring, &mut out, 4), 4);
        assert_eq!(&out[..4], &5.0f32.to_le_bytes());
        assert_eq!(&out[12..16], &8.0f32.to_le_bytes());
    }
}
