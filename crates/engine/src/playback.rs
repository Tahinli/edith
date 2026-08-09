//! One timeline over one or more files: the decode workers, the audio output
//! and the master clock wired together so a front-end only has to render and
//! call [`tick`].
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
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::ao::AoSession;
use crate::audio::{AudioChunk, AudioSession};
use crate::clock::{ClockSource, PlaybackClock};
use crate::decode::{DecodeSession, Frame};
use crate::demux::{Demuxer, VideoMeta};
use crate::project::{Clip, Project};

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
    meta: VideoMeta,
    frames: Receiver<Frame>,
    /// Stops the current decode worker, so an abandoned one cannot outlive a
    /// seek by more than one access unit.
    cancel: Arc<AtomicBool>,
    clock: PlaybackClock,
    audio: Option<Audio>,
    /// The edit list. Everything a caller says in seconds is a *timeline*
    /// position; only this maps it onto the file.
    project: Project,
    /// Source range the current decode worker was opened for, and where that
    /// range starts on the timeline -- together they rewrite a source frame
    /// index into a timeline one. Not a clip index: a `cut` splits the clip
    /// under a running worker, and only the mapping survives that.
    range: Clip,
    timeline_start: u32,
    /// The last clip has been played out; see [`PlaybackSession::is_eos`].
    eos: bool,
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
        // One clip covering the file, so timeline == source until the first
        // edit -- and `open_at(_, 0)` is exactly that clip's range.
        let project = Project::single(&path, meta.frame_count);
        let range = project.clips()[0];
        Ok(Self {
            meta,
            frames,
            cancel,
            clock: PlaybackClock::new(source),
            audio,
            project,
            range,
            timeline_start: 0,
            eos: false,
        })
    }

    /// Opens a project file written by [`save_project`](Self::save_project):
    /// the whole timeline, restored to where its playhead stood, paused like
    /// [`open`](Self::open).
    ///
    /// A *new* session rather than a reload of this one, so a load that fails
    /// leaves the caller's current session untouched -- atomic by
    /// construction. Every named file is opened and checked here: source 0
    /// defines the timeline exactly as `open` does, every other source has to
    /// match it the way [`import`](Self::import) demands, and every clip has to
    /// still be inside the file it plays from. A source that vanished or
    /// shrank since the save is a refusal naming it, not a silently shorter
    /// timeline -- the disappearing-file tolerance elsewhere in this type
    /// applies only *after* a project has loaded.
    ///
    /// The undo history is not saved: `undo` is `false` on a fresh load.
    pub fn open_project(path: &Path) -> crate::Result<Self> {
        let doc = crate::edith::load(path)?;
        let first = doc.sources.first().ok_or("the project names no sources")?;
        // Source 0 both scaffolds the session and defines the timeline, so it
        // is opened for playback rather than merely probed.
        let (meta, frames, cancel) = DecodeSession::open_at(first, 0)
            .map_err(|e| format!("source {}: {e}", first.display()))?;
        let audio = open_audio(first);
        let first_audio = AudioSession::probe(first)?;

        let mut counts = vec![meta.frame_count];
        for source in &doc.sources[1..] {
            let (other, _) =
                Demuxer::open(source).map_err(|e| format!("source {}: {e}", source.display()))?;
            matches_timeline(source, &other, &meta, &first_audio)
                .map_err(|e| format!("source {}: {e}", source.display()))?;
            counts.push(other.frame_count);
        }
        for (i, clip) in doc.clips.iter().enumerate() {
            if clip.out_frame > counts[clip.source] {
                return Err(format!(
                    "clip {i} ends at frame {} but {} has {} frames",
                    clip.out_frame,
                    doc.sources[clip.source].display(),
                    counts[clip.source]
                )
                .into());
            }
        }

        let playhead = doc.playhead;
        let project = Project::from_parts(doc.sources, doc.clips)
            .ok_or("the project's clips do not fit its sources")?;
        let range = project.clips()[0];
        let mut session = Self {
            meta,
            frames,
            cancel,
            clock: PlaybackClock::new(match audio {
                Some(_) => ClockSource::Audio,
                None => ClockSource::Wall,
            }),
            audio,
            project,
            range,
            timeline_start: 0,
            eos: false,
        };
        // The scaffolding above opened source 0 from its first frame and the
        // whole of its audio; this puts both onto the clip the playhead is
        // actually in -- a seek replaces the decoder and supersedes the audio
        // worker by epoch, exactly as it does after an edit. It also clamps,
        // so a playhead past a hand-shortened timeline lands on the last frame.
        session.seek(f64::from(playhead) / meta.frame_rate);
        Ok(session)
    }

    pub fn meta(&self) -> &VideoMeta {
        &self.meta
    }

    /// The files this timeline plays from, index 0 first -- a caller needs it
    /// to name an export or a window after the media rather than the project.
    pub fn sources(&self) -> &[PathBuf] {
        self.project.sources()
    }

    /// Writes the timeline to `path` as a `.edith`, atomically (see
    /// [`crate::edith`]). Sources no clip plays from are left out, and the
    /// playhead is saved with it so a reopened project resumes where it stood.
    pub fn save_project(&self, path: &Path) -> crate::Result<()> {
        let (sources, clips) = self.project.without_orphan_sources();
        let playhead = secs_to_frame(self.now(), self.meta.frame_rate)
            .min(self.project.timeline_frames().saturating_sub(1));
        crate::edith::save(path, &sources, &clips, playhead)
    }

    /// The next decoded frame, its `index` rewritten from a source frame to a
    /// *timeline* frame -- the only frame space that leaves the engine, and the
    /// one [`PlaybackSession::now`] is in.
    ///
    /// `None` means "nothing right now": the decoder is behind, or a clip
    /// boundary is being reopened (~80 ms, during which the caller simply keeps
    /// showing its last frame). End of stream is [`PlaybackSession::is_eos`].
    pub fn try_frame(&mut self) -> Option<Frame> {
        loop {
            match self.frames.try_recv() {
                Ok(mut frame) => {
                    frame.index =
                        self.timeline_start + frame.index.saturating_sub(self.range.in_frame);
                    return Some(frame);
                }
                Err(TryRecvError::Empty) => return None,
                // The worker stopped: at the end of its range, or on a decode
                // error -- which is why this skips the rest of a broken clip
                // rather than stalling on it.
                Err(TryRecvError::Disconnected) => {
                    if !self.next_clip() {
                        self.eos = true;
                        return None;
                    }
                }
            }
        }
    }

    /// Whether the timeline has been played out. Any seek clears it, so an edit
    /// after the end (which reseeks) revives the session.
    pub fn is_eos(&self) -> bool {
        self.eos
    }

    /// Starts decoding whatever follows the current range on the timeline;
    /// `false` past the end. The next timeline frame is derived rather than
    /// remembered as a clip index, because a `cut` while playing splits the
    /// clip under the running worker and only the mapping stays true.
    fn next_clip(&mut self) -> bool {
        let next = self.timeline_start + self.range.len();
        let Some((idx, source)) = self.project.map_timeline(next) else {
            return false;
        };
        let clip = self.project.clips()[idx];
        let out = clip.out_frame;
        match DecodeSession::open_range(&self.project.sources()[clip.source], source, out) {
            Ok((_, frames, cancel)) => {
                self.frames = frames;
                self.cancel = cancel;
            }
            // Disappearing-file case, as in `seek`. The old receiver is still
            // disconnected, so the next pass moves on to the clip after this.
            Err(e) => eprintln!("clip at timeline frame {next}: video open failed: {e}"),
        }
        self.range = Clip {
            in_frame: source,
            out_frame: out,
            ..clip
        };
        self.timeline_start = next;
        true
    }

    /// Length of the edited timeline in seconds -- what a ruler shows, and it
    /// shrinks with every delete.
    pub fn timeline_duration(&self) -> f64 {
        f64::from(self.project.timeline_frames()) / self.meta.frame_rate
    }

    /// `(start, len)` per clip in timeline seconds, in order: what a clips lane
    /// needs to lay itself out. Selection is the caller's business.
    pub fn clip_spans(&self) -> Vec<(f64, f64)> {
        let fps = self.meta.frame_rate;
        self.project
            .clip_spans()
            .iter()
            .map(|&(start, len)| (f64::from(start) / fps, f64::from(len) / fps))
            .collect()
    }

    /// [`clip_spans`](Self::clip_spans) plus the index into
    /// [`Project::sources`] each clip plays from -- what a lane needs to colour
    /// an imported clip differently from the file the session was opened with.
    pub fn clip_spans_by_source(&self) -> Vec<(f64, f64, usize)> {
        self.clip_spans()
            .into_iter()
            .zip(self.project.clips())
            .map(|((start, len), clip)| (start, len, clip.source))
            .collect()
    }

    /// Splits the clip under `timeline_secs` in two. Metadata only: a cut never
    /// changes the timeline->source mapping, so the running decoder stays
    /// correct and playback does not blink. `false` at a clip start or past the
    /// end, where there would be nothing to split off.
    pub fn cut_at(&mut self, timeline_secs: f64) -> bool {
        self.project
            .cut(secs_to_frame(timeline_secs, self.meta.frame_rate))
    }

    /// The clip at `idx` -- what a caller copies. It is a pair of source frame
    /// numbers and nothing else, so a copy stays valid after the clip it came
    /// from is deleted. `None` past the end.
    pub fn clip_at(&self, idx: usize) -> Option<Clip> {
        self.project.clips().get(idx).copied()
    }

    /// Inserts `clip` at the playhead, splitting the clip under it. Like a
    /// delete this moves every following frame, so the session reseeks; past
    /// the end of the timeline the clip is appended. One undo step.
    pub fn paste_at(&mut self, timeline_secs: f64, clip: Clip) -> bool {
        let at = secs_to_frame(timeline_secs, self.meta.frame_rate);
        self.edit(|p| p.paste(at, clip))
    }

    /// Removes a clip and closes the gap. Unlike a cut this *does* move every
    /// following frame, so the session reseeks to wherever the playhead now
    /// points. `false` for a bad index or the last remaining clip.
    pub fn delete_clip(&mut self, idx: usize) -> bool {
        self.edit(|p| p.delete(idx))
    }

    /// Undoes the last successful cut or delete, and reseeks like a delete.
    pub fn undo(&mut self) -> bool {
        self.edit(Project::undo)
    }

    /// Appends the whole of `path` to the end of the timeline. One undo step,
    /// and the file becomes a source only if it is not one already.
    ///
    /// Refused unless it matches the timeline exactly -- same dimensions, same
    /// frame rate, same audio parameters or both silent -- because one timeline
    /// this slice means one set of encoder/device parameters; the `Err` names
    /// the property that disagrees, for a caller to show. Nothing is changed by
    /// a refusal.
    pub fn import(&mut self, path: &Path) -> crate::Result<()> {
        let (meta, _) = Demuxer::open(path)?;
        let first = AudioSession::probe(&self.project.sources()[0])?;
        matches_timeline(path, &meta, &self.meta, &first)?;

        let old_end = self.timeline_duration();
        let source = self.project.import(path);
        // Refused only for an unknown index, and this one just came from `import`.
        self.project.append_clip(source, meta.frame_count);
        // Reseek like any other edit, even though nothing before the playhead
        // moved: the running audio worker's segment list stops at the old end,
        // so without this the appended clip would play silent. At EOS the wall
        // clock has run on past the timeline, so resume from the join instead
        // of wherever it got to -- and the seek is what clears `eos`.
        let at = if self.eos { old_end } else { self.now() };
        self.seek(at);
        Ok(())
    }

    /// Applies an edit and, if it took, reseeks onto the new mapping. `seek`
    /// clamps to the new duration and keeps playing playing, so a delete that
    /// shortens the timeline past the playhead lands on the last frame.
    fn edit(&mut self, f: impl FnOnce(&mut Project) -> bool) -> bool {
        if !f(&mut self.project) {
            return false;
        }
        let now = self.now();
        self.seek(now);
        true
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

    /// Repositions the timeline to `secs`, clamped to the *timeline*. Both decode
    /// workers are replaced rather than steered: a fresh channel cannot hold a
    /// stale frame, and it works just as well after EOF, where the old workers
    /// have already exited.
    ///
    /// Playing stays playing and paused stays paused -- but a paused seek still
    /// decodes, so the caller can show the frame it landed on.
    pub fn seek(&mut self, secs: f64) {
        let fps = self.meta.frame_rate;
        let total = self.project.timeline_frames();
        let t = secs.clamp(0.0, f64::from(total) / fps);
        let target = secs_to_frame(t, fps).min(total.saturating_sub(1));
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
        // `target` is inside the timeline (never empty), so this always maps;
        // the range runs from there to the end of the clip it landed in.
        if let Some((idx, source)) = self.project.map_timeline(target) {
            let clip = self.project.clips()[idx];
            let out = clip.out_frame;
            match DecodeSession::open_range(&self.project.sources()[clip.source], source, out) {
                Ok((_, frames, cancel)) => {
                    self.frames = frames;
                    self.cancel = cancel;
                }
                // The file opened once already, so this is a disappearing-file
                // case: the timeline still moves, there are simply no more
                // pictures.
                Err(e) => eprintln!("seek: video reopen failed: {e}"),
            }
            self.range = Clip {
                in_frame: source,
                out_frame: out,
                ..clip
            };
            self.timeline_start = target;
        }
        self.eos = false;

        let mut audio_running = false;
        if let Some(audio) = &self.audio
            && !audio.died.load(Ordering::Acquire)
        {
            // The device position keeps counting across a flush, so re-basing
            // `fed` on it is what keeps `played_out` exact for the new stream.
            let played = audio.ao.lock().unwrap().position().unwrap_or(0).max(0) as u64;
            audio.fed.store(played * audio.channels, Ordering::Relaxed);
            audio.fed_all.store(false, Ordering::Release);
            // One worker for the whole rest of the timeline, so the joins
            // between clips are gapless: the video reopens at a boundary, the
            // ear never hears it -- and a join between two *files* is just
            // another segment, because every segment names its own source.
            let segs = self.project.segments_from(target, fps);
            audio_running = match AudioSession::open_multi_segments(self.project.sources(), &segs) {
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

    /// Renders the edited timeline to `out` on a worker thread. The session is
    /// untouched -- the export decodes the source file for itself -- but a
    /// caller should still [`pause`](PlaybackSession::pause) first, because
    /// playback and export otherwise fight over the same decoder hardware.
    ///
    /// Later edits do not reach a running export: it works from a snapshot of
    /// the edit list taken here.
    pub fn export_to(&self, out: &Path) -> crate::ExportHandle {
        crate::export::start(self.project.clone(), self.meta, out)
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

/// Whether `path`, already demuxed to `meta`, may join a timeline whose
/// parameters are `timeline` and whose audio probes as `first`: same
/// dimensions, same frame rate, same audio parameters or both silent -- one
/// timeline this slice means one set of encoder/device parameters.
///
/// The `Err` names the property that disagrees, and those strings are what a
/// front-end shows verbatim; [`PlaybackSession::import`] and
/// [`PlaybackSession::open_project`] share them so a file refused at import is
/// refused in the same words at load.
fn matches_timeline(
    path: &Path,
    meta: &VideoMeta,
    timeline: &VideoMeta,
    first: &Option<crate::AudioProbe>,
) -> crate::Result<()> {
    if (meta.width, meta.height) != (timeline.width, timeline.height) {
        return Err(format!(
            "{}x{} does not match the timeline's {}x{}",
            meta.width, meta.height, timeline.width, timeline.height
        )
        .into());
    }
    // Container frame rates are computed from timescales, so never `==`.
    if (meta.frame_rate - timeline.frame_rate).abs() > 0.01 {
        return Err(format!(
            "{:.3} fps does not match the timeline's {:.3} fps",
            meta.frame_rate, timeline.frame_rate
        )
        .into());
    }
    // Whole-probe equality: rate, layout and the esds fields, which the audio
    // worker holds every source to anyway. Both silent is a match.
    let probe = AudioSession::probe(path)?;
    if probe != *first {
        return Err(match (probe, first) {
            (None, _) => "the file is silent, the timeline has audio".to_string(),
            (_, None) => "the file has audio, the timeline is silent".to_string(),
            (Some(a), Some(b)) => format!(
                "audio {} Hz {} ch does not match the timeline's {} Hz {} ch",
                a.params.sample_rate, a.channels, b.params.sample_rate, b.channels
            ),
        }
        .into());
    }
    Ok(())
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

/// Timeline seconds to a 0-based frame index, floor semantics: position `t`
/// shows the frame whose interval contains it. The epsilon absorbs float error
/// at exact frame boundaries -- `123.0/30.0 * 30.0` is `122.999...`, and a
/// bare truncation would land one frame early (S6 finding: 4 of every 300
/// frames round-trip wrong through `clip_spans()` seconds without this).
fn secs_to_frame(secs: f64, fps: f64) -> u32 {
    (secs * fps + 1e-6).floor().max(0.0) as u32
}

#[cfg(test)]
mod tests {
    use super::secs_to_frame;

    #[test]
    fn boundary_seconds_round_trip_to_their_own_frame() {
        for f in 0u32..300 {
            let secs = f64::from(f) / 30.0;
            assert_eq!(secs_to_frame(secs, 30.0), f, "frame {f}");
        }
        // Mid-frame positions still floor, and negatives clamp.
        assert_eq!(secs_to_frame(10.5 / 30.0, 30.0), 10);
        assert_eq!(secs_to_frame(-0.2, 30.0), 0);
    }
}
