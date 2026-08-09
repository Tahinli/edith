//! One file, one timeline: the decode workers, the audio output and the master
//! clock wired together so a front-end only has to render and call [`tick`].
//!
//! [`tick`]: PlaybackSession::tick
//!
//! Everything degrades to silent-but-correct: no audio track, no plugin, no
//! PipeWire daemon and an unusable format all end up in [`ClockSource::Wall`],
//! where the video is paced by real time instead of by the device.
//!
//! Drift policy stays with the caller -- this type answers "what time is it",
//! the renderer decides which frame that means.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::ao::AoSession;
use crate::audio::{AudioChunk, AudioSession};
use crate::clock::{ClockSource, PlaybackClock};
use crate::decode::{DecodeSession, Frame};
use crate::demux::VideoMeta;

/// How long the feeder waits out a full ring. The ring holds a second, so this
/// only has to be short next to that; it costs one wakeup per 10 ms of audio.
const RING_FULL_WAIT: Duration = Duration::from_millis(10);

/// The output half of a session: the device, plus what the feeder has handed it.
struct Audio {
    /// Shared because `write` runs on the feeder thread while `position` and
    /// `set_active` are called from the caller's. The plugin serialises those
    /// itself (atomics and a channel), but the lock is what makes it sayable in
    /// Rust and it is never held for more than a memcpy.
    ao: Arc<Mutex<AoSession>>,
    sample_rate: u32,
    channels: u64,
    /// Interleaved samples accepted by the device so far.
    fed: Arc<AtomicU64>,
    /// The decoder ran out and `fed` is final.
    fed_all: Arc<AtomicBool>,
}

impl Audio {
    /// Whether the device has played everything there will ever be, i.e. the
    /// audio clock is about to stop meaning anything.
    fn played_out(&self, position: i64) -> bool {
        self.fed_all.load(Ordering::Acquire)
            && position as u64 * self.channels >= self.fed.load(Ordering::Relaxed)
    }
}

/// A file opened for playback. Starts paused at t=0; call [`PlaybackSession::play`].
pub struct PlaybackSession {
    meta: VideoMeta,
    frames: Receiver<Frame>,
    clock: PlaybackClock,
    audio: Option<Audio>,
}

impl PlaybackSession {
    /// Opens `path` and starts both decode workers. Only video failure is fatal:
    /// a file we cannot hear is still a file we can watch.
    pub fn open(path: impl AsRef<Path>) -> crate::Result<Self> {
        let path = path.as_ref();
        let (meta, frames) = DecodeSession::open(path)?;
        let audio = open_audio(path);
        let source = match audio {
            Some(_) => ClockSource::Audio,
            None => ClockSource::Wall,
        };
        Ok(Self {
            meta,
            frames,
            clock: PlaybackClock::new(source),
            audio,
        })
    }

    pub fn meta(&self) -> &VideoMeta {
        &self.meta
    }

    /// Decoded frames, in decode order. The channel is bounded, so leaving it
    /// alone is what pauses the decoder.
    pub fn frames(&self) -> &Receiver<Frame> {
        &self.frames
    }

    /// Current timeline position in seconds.
    pub fn now(&self) -> f64 {
        self.clock.now()
    }

    pub fn is_playing(&self) -> bool {
        self.clock.is_playing()
    }

    pub fn play(&mut self) {
        if self.clock.is_playing() {
            return;
        }
        // Invariant: the clock is positioned *before* audio restarts. Audio
        // reports are relative to the first one after that, so a seek would go
        // here too -- `clock.seek_to(t)` and then `set_active(true)`.
        self.clock.play();
        if let Some(audio) = &self.audio {
            audio.ao.lock().unwrap().set_active(true);
        }
    }

    pub fn pause(&mut self) {
        if !self.clock.is_playing() {
            return;
        }
        if let Some(audio) = &self.audio {
            audio.ao.lock().unwrap().set_active(false);
        }
        self.clock.pause();
    }

    pub fn toggle(&mut self) {
        if self.clock.is_playing() {
            self.pause()
        } else {
            self.play()
        }
    }

    /// Moves the clock forward; the caller runs this once per rendered frame.
    /// Costs an atomic load, and once per session a mode switch.
    pub fn tick(&mut self) {
        let Some(audio) = &self.audio else {
            return; // wall time from the start, nothing to poll
        };
        if self.clock.source() != ClockSource::Audio {
            return;
        }
        // `None` until the device's first callback: the clock holds at its
        // anchor rather than guessing, which is why playback starts on time
        // instead of a quantum early.
        let Some(position) = audio.ao.lock().unwrap().position() else {
            return;
        };
        if audio.played_out(position) {
            // Audio EOF. Wall time carries the same timeline on from here, with
            // no jump, and this branch cannot be reached again.
            self.clock.switch_to_wall();
        } else {
            self.clock.set_audio_position(position as u64, audio.sample_rate);
        }
    }
}

/// `None` for a silent session -- no audio track, no plugin, no daemon, or a
/// track the decoder refuses. Dropping the receiver on the way out stops the
/// audio decode worker, so a failure here leaves nothing running.
fn open_audio(path: &Path) -> Option<Audio> {
    let (meta, rx) = match AudioSession::open(path) {
        Ok(Some(opened)) => opened,
        Ok(None) => return None,
        Err(e) => {
            eprintln!("audio disabled: {e}");
            return None;
        }
    };
    let ao = AoSession::open(meta.sample_rate, u32::from(meta.channels))?;
    // The stream comes up streaming; a session starts paused, so silence the
    // device until `play`. The feeder still fills the ring in the meantime.
    ao.set_active(false);

    let audio = Audio {
        ao: Arc::new(Mutex::new(ao)),
        sample_rate: meta.sample_rate,
        channels: u64::from(meta.channels),
        fed: Arc::new(AtomicU64::new(0)),
        fed_all: Arc::new(AtomicBool::new(false)),
    };
    let (ao, fed, fed_all) = (
        audio.ao.clone(),
        audio.fed.clone(),
        audio.fed_all.clone(),
    );
    thread::Builder::new()
        .name("audio-feed".into())
        .spawn(move || {
            feed(rx, &ao, &fed);
            fed_all.store(true, Ordering::Release);
        })
        .ok()?;
    Some(audio)
}

/// Pours decoded chunks into the device, keeping whatever the ring would not
/// take. Ends when the decoder is done or the output dies.
fn feed(rx: Receiver<AudioChunk>, ao: &Mutex<AoSession>, fed: &AtomicU64) {
    let mut pending: Vec<f32> = Vec::new();
    loop {
        if pending.is_empty() {
            match rx.recv() {
                Ok(chunk) => pending = chunk.samples,
                Err(_) => return, // decoder finished or went away
            }
        }
        let accepted = match ao.lock().unwrap().write(&pending) {
            Some(n) => n,
            None => return, // output died; the clock switches to wall
        };
        fed.fetch_add(accepted as u64, Ordering::Relaxed);
        pending.drain(..accepted);
        if !pending.is_empty() {
            // Ring full: nothing to do but let the device drain it.
            thread::sleep(RING_FULL_WAIT);
        }
    }
}
