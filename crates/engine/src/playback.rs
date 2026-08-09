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

use std::path::{Path, PathBuf};
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
/// Cloned into the feeder thread; every field that matters is shared.
#[derive(Clone)]
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
    /// The output plugin failed mid-stream. Its position is frozen from here on,
    /// so it can never satisfy [`Audio::played_out`] -- the flag is what lets
    /// [`PlaybackSession::tick`] hand the timeline to wall time instead.
    died: Arc<AtomicBool>,
    /// Bumped by every seek. A feeder only owns the device while this still
    /// matches the value it started with; see [`feed`].
    epoch: Arc<AtomicU64>,
}

impl Audio {
    /// Whether the device has played everything there will ever be, i.e. the
    /// audio clock is about to stop meaning anything.
    fn played_out(&self, position: i64) -> bool {
        self.fed_all.load(Ordering::Acquire)
            && position as u64 * self.channels >= self.fed.load(Ordering::Relaxed)
    }

    /// Starts a feeder for the current epoch, draining `rx` into the device.
    /// `false` if the thread would not start, i.e. nothing will ever be fed.
    fn spawn_feeder(&self, rx: Receiver<AudioChunk>) -> bool {
        let me = self.clone();
        let epoch = self.epoch.load(Ordering::Acquire);
        thread::Builder::new()
            .name("audio-feed".into())
            .spawn(move || {
                feed(rx, &me, epoch);
                // Only the current feeder gets to declare the end of the audio:
                // a superseded one is finished, the stream is not.
                if me.epoch.load(Ordering::Acquire) == epoch {
                    me.fed_all.store(true, Ordering::Release);
                }
            })
            .is_ok()
    }
}

/// A file opened for playback. Starts paused at t=0; call [`PlaybackSession::play`].
pub struct PlaybackSession {
    /// Kept for [`PlaybackSession::seek`], which reopens both workers.
    path: PathBuf,
    meta: VideoMeta,
    frames: Receiver<Frame>,
    /// Stops the current decode worker, so an abandoned one cannot outlive a
    /// seek by more than one access unit.
    cancel: Arc<AtomicBool>,
    clock: PlaybackClock,
    audio: Option<Audio>,
}

impl PlaybackSession {
    /// Opens `path` and starts both decode workers. Only video failure is fatal:
    /// a file we cannot hear is still a file we can watch.
    pub fn open(path: impl AsRef<Path>) -> crate::Result<Self> {
        let path = path.as_ref().to_path_buf();
        // `open_at(_, 0)` rather than `open` purely for the cancel handle: the
        // field has to exist from the start for the first seek to use it.
        let (meta, frames, cancel) = DecodeSession::open_at(&path, 0)?;
        let audio = open_audio(&path);
        let source = match audio {
            Some(_) => ClockSource::Audio,
            None => ClockSource::Wall,
        };
        Ok(Self {
            path,
            meta,
            frames,
            cancel,
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

    /// Repositions the timeline to `secs`, clamped to the file. Both decode
    /// workers are replaced rather than steered: a fresh channel cannot hold a
    /// stale frame, and it works just as well after EOF, where the old workers
    /// have already exited.
    ///
    /// Playing stays playing and paused stays paused -- but a paused seek still
    /// decodes, so the caller can show the frame it landed on.
    pub fn seek(&mut self, secs: f64) {
        let fps = self.meta.frame_rate;
        let t = secs.clamp(0.0, f64::from(self.meta.frame_count) / fps);
        let target = ((t * fps) as u32).min(self.meta.frame_count.saturating_sub(1));
        let was_playing = self.clock.is_playing();

        if let Some(audio) = &self.audio {
            let ao = audio.ao.lock().unwrap();
            ao.set_active(false);
            // Bumped and flushed while holding the device lock, because the
            // feeder re-checks the epoch inside that same lock: whatever it is
            // carrying can no longer reach the ring after this point.
            audio.epoch.fetch_add(1, Ordering::Release);
            ao.flush();
        }

        // Cancel first, drop second: the old worker may be parked in `send` on
        // the bounded channel, where only the disconnect wakes it -- and it
        // then finds the flag already set.
        self.cancel.store(true, Ordering::Release);
        match DecodeSession::open_at(&self.path, target) {
            Ok((_, frames, cancel)) => {
                self.frames = frames;
                self.cancel = cancel;
            }
            // The file opened once already, so this is a disappearing-file case:
            // the timeline still moves, there are simply no more pictures.
            Err(e) => eprintln!("seek: video reopen failed: {e}"),
        }

        let mut audio_running = false;
        if let Some(audio) = &self.audio
            && !audio.died.load(Ordering::Acquire)
        {
            // The device position keeps counting across a flush, so re-basing
            // `fed` on it is what keeps `played_out` exact for the new stream.
            let played = audio.ao.lock().unwrap().position().unwrap_or(0).max(0) as u64;
            audio.fed.store(played * audio.channels, Ordering::Relaxed);
            audio.fed_all.store(false, Ordering::Release);
            audio_running = match AudioSession::open_at(&self.path, t) {
                Ok(Some((_, rx))) => audio.spawn_feeder(rx),
                _ => false,
            };
            if !audio_running {
                // Nothing will ever be fed again; let `tick` fall to wall time
                // instead of waiting on a device that has no more work coming.
                audio.fed_all.store(true, Ordering::Release);
            }
        }

        // Same invariant as `play`: position the clock before audio restarts.
        if audio_running && self.clock.source() != ClockSource::Audio {
            self.clock.switch_to_audio();
        }
        self.clock.seek_to(t);
        if was_playing && let Some(audio) = &self.audio {
            audio.ao.lock().unwrap().set_active(true);
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
        if audio.died.load(Ordering::Acquire) {
            // Output died mid-stream: its position will never reach `fed`, so
            // the EOF comparison below is unreachable. Wall time takes over
            // from wherever the clock last stood.
            self.clock.switch_to_wall();
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
            self.clock
                .set_audio_position(position as u64, audio.sample_rate);
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
        died: Arc::new(AtomicBool::new(false)),
        epoch: Arc::new(AtomicU64::new(0)),
    };
    audio.spawn_feeder(rx).then_some(audio)
}

/// Pours decoded chunks into the device, keeping whatever the ring would not
/// take. Ends when the decoder is done, the output dies, or a seek supersedes
/// this feeder.
fn feed(rx: Receiver<AudioChunk>, audio: &Audio, epoch: u64) {
    let mut pending: Vec<f32> = Vec::new();
    loop {
        if pending.is_empty() {
            match rx.recv() {
                Ok(chunk) => pending = chunk.samples,
                Err(_) => return, // decoder finished or went away
            }
        }
        let accepted = {
            let mut ao = audio.ao.lock().unwrap();
            // Under the lock, immediately before the write: `seek` bumps the
            // epoch and flushes the ring holding this very lock, so past this
            // check no stale sample can be queued behind the flush.
            if audio.epoch.load(Ordering::Acquire) != epoch {
                return;
            }
            match ao.write(&pending) {
                Some(n) => n,
                None => {
                    // Output died; flag it so `tick` moves the clock to wall time.
                    audio.died.store(true, Ordering::Release);
                    return;
                }
            }
        };
        audio.fed.fetch_add(accepted as u64, Ordering::Relaxed);
        pending.drain(..accepted);
        if !pending.is_empty() {
            // Ring full: nothing to do but let the device drain it.
            thread::sleep(RING_FULL_WAIT);
        }
    }
}
