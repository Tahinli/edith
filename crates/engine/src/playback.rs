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

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::ao::AoSession;
use crate::audio::{AudioChunk, AudioSession};
use crate::clock::{ClockSource, PlaybackClock};
use crate::color::ColorParams;
use crate::decode::{DecodeSession, Frame, Worker};
use crate::demux::{Demuxer, VideoMeta};
use crate::eq::EqParams;
use crate::project::{Clip, Lane, LaneKind, Project, Source, Span};
use crate::scale::{Composer, FitPolicy};

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
    /// The *timeline's* parameters, not any one file's: its frame rate and codec
    /// come from source 0 and are what every other source is held to, while
    /// `width`/`height` are the **project resolution** -- source 0's picture to
    /// begin with, and whatever [`PlaybackSession::set_resolution`] or a saved
    /// `resolution` line makes it after that. Every clip is composed onto it, so
    /// a file of another size is a placed picture rather than a refusal.
    meta: VideoMeta,
    /// Source 0's own picture, kept from the open: the project resolution
    /// starts here, and a caller offering sizes to pick from needs the media's
    /// own among them or a project moved off it could never come back.
    native: (u32, u32),
    frames: Receiver<Frame>,
    /// The current decode worker.
    worker: Worker,
    /// Workers cancelled by a seek or a clip change, waiting to be reaped.
    /// Joining one inline costs whatever VA-API init it is still inside (98 ms
    /// measured worst case), which is a visible hitch on the caller's thread;
    /// parking it here instead makes that free, and [`retire`] drops the ones
    /// that have already returned every time it adds another.
    ///
    /// Never a leak, and that is the invariant: everything in here is joined
    /// when the session drops (`Vec<Worker>`'s own drop does it, one
    /// [`Worker::drop`] each), so no decode worker can outlive the session and
    /// meet Mesa's `atexit` handlers from inside libva.
    ///
    /// [`retire`]: PlaybackSession::retire
    retired: Vec<Worker>,
    clock: PlaybackClock,
    audio: Option<Audio>,
    /// Why this timeline is silent, when the file itself is not: an audio track
    /// we cannot decode is a session that plays perfectly and says nothing about
    /// it otherwise. `None` for a file that has usable audio *and* for one that
    /// has none at all, which is not a surprise worth a notice.
    audio_disabled: Option<String>,
    /// The edit list. Everything a caller says in seconds is a *timeline*
    /// position; only this maps it onto the file.
    project: Project,
    /// What the current video worker was opened for: where it sits on the
    /// timeline, how long it runs, and which source frame it started at --
    /// together they rewrite a source frame index into a timeline one. Not a
    /// clip index: a `split` cuts the clip under a running worker, and only the
    /// mapping survives that. A span with no source is a *gap*, and the worker
    /// feeding it emits black frames indexed from zero.
    span: Span,
    /// The last clip has been played out; see [`PlaybackSession::is_eos`].
    eos: bool,
}

impl PlaybackSession {
    /// Opens `path` and starts both decode workers. Only video failure is fatal:
    /// a file we cannot hear is still a file we can watch.
    pub fn open(path: impl AsRef<Path>) -> crate::Result<Self> {
        let path = path.as_ref().to_path_buf();
        // A timeline is scaffolded from source 0's picture -- its size, its
        // frame rate, its clock. A song has none of that, so it joins a
        // timeline (`import`) rather than starting one, and saying so beats
        // failing in the demuxer's words.
        if crate::is_audio(&path) {
            return Err(format!(
                "{} has no picture: open a video first, then import it onto the audio lane",
                path.display()
            )
            .into());
        }
        // `open_worker` rather than `open` purely for the worker handle: the
        // field has to exist from the start for the first seek to use it.
        // Ungraded: a file just opened has one clip per lane and nothing has
        // graded it yet. Every later span is opened by `start_span`, which asks
        // the project.
        // Passthrough: a freshly opened file *is* the project resolution, so
        // there is nothing to place it on. Every later span goes through
        // `start_span`, which builds the canvas from the project.
        let (meta, stream) = DecodeSession::open_worker(
            &path,
            0,
            u32::MAX,
            ColorParams::default(),
            Composer::passthrough(),
        )?;
        // A file is opened on its first audio stream, like `Project::single`
        // names it: nothing has picked one yet.
        let (audio, audio_disabled) = open_audio(&path, 0);
        let source = match audio {
            Some(_) => ClockSource::Audio,
            None => ClockSource::Wall,
        };
        // One clip per lane covering the file, so timeline == source until the
        // first edit -- and the range opened above is exactly that clip's.
        let project = Project::single(&path, meta.frame_count);
        let span = project.composite_span_at(0).expect("never empty");
        Ok(Self {
            meta,
            native: (meta.width, meta.height),
            frames: stream.frames,
            worker: stream.worker,
            retired: Vec::new(),
            clock: PlaybackClock::new(source),
            audio,
            audio_disabled,
            project,
            span,
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
        // Owned: the sources move into the `Project` below, and the device is
        // opened after that (see the comment there).
        let first = doc
            .sources
            .first()
            .cloned()
            .ok_or("the project names no sources")?;
        // Source 0 both scaffolds the session and defines the timeline, so it
        // is opened for playback rather than merely probed.
        // One value, not two locals: everything below this line can refuse, and
        // locals drop in reverse declaration order -- a bare worker would join
        // its decode thread while the receiver next to it was still holding
        // that thread parked in `send`, which is a hang (see [`FrameStream`]).
        // Ungraded, and superseded before a frame of it is shown: the `seek` at
        // the end of this function reopens the playhead's span through
        // `start_span`, which is where a saved grade reaches the picture.
        let (mut meta, stream) = DecodeSession::open_worker(
            &first.path,
            0,
            u32::MAX,
            ColorParams::default(),
            Composer::passthrough(),
        )
        .map_err(|e| format!("source {}: {e}", first.path.display()))?;
        // The project's own resolution, which is source 0's picture unless the
        // file says otherwise -- every dialect before v7 had no way to say it,
        // and that default is exactly what those projects meant.
        let native = (meta.width, meta.height);
        if let Some((width, height)) = doc.resolution {
            meta.width = width;
            meta.height = height;
        }
        let first_audio = AudioSession::probe(&first.path, first.audio_stream)?;

        let mut counts = vec![meta.frame_count];
        for source in &doc.sources[1..] {
            // A source with no picture is checked on what it does have, and
            // its length is its playing time -- the same two answers `import`
            // gave it when it first joined the timeline.
            if crate::is_audio(&source.path) {
                audio_matches(source, &first_audio)
                    .map_err(|e| format!("source {}: {e}", source.path.display()))?;
                counts.push(audio_frames(&source.path, meta.frame_rate)?);
                continue;
            }
            let (other, _) = Demuxer::open(&source.path)
                .map_err(|e| format!("source {}: {e}", source.path.display()))?;
            matches_timeline(source, &other, &meta, &first_audio)
                .map_err(|e| format!("source {}: {e}", source.path.display()))?;
            counts.push(other.frame_count);
        }
        // The video lane can only play files that have pictures. Hand-written
        // (or hand-edited) project files are the one door this can come in
        // through, so it is refused by name here rather than becoming a clip
        // that decodes to nothing.
        for clip in doc
            .lanes
            .iter()
            .filter(|(kind, _)| *kind == LaneKind::Video)
            .flat_map(|(_, clips)| clips)
        {
            if crate::is_audio(&doc.sources[clip.source].path) {
                return Err(format!(
                    "{} has no picture: it can only play on an audio lane",
                    doc.sources[clip.source].path.display()
                )
                .into());
            }
        }
        for (i, clip) in doc.lanes.iter().flat_map(|(_, clips)| clips).enumerate() {
            if clip.out_frame > counts[clip.source] {
                return Err(format!(
                    "clip {i} ends at frame {} but {} has {} frames",
                    clip.out_frame,
                    doc.sources[clip.source].path.display(),
                    counts[clip.source]
                )
                .into());
            }
        }

        let playhead = doc.playhead;
        let project = Project::from_parts(doc.sources, doc.lanes, doc.eq, doc.color)?;
        let span = project.composite_span_at(0).expect("never empty");
        // Last, because it is the one thing here that cannot be taken back: the
        // feeder thread outlives the `Audio` value (it holds its own clones) and
        // only a session's `drop` retires it, so a refusal above this line would
        // leave a PipeWire stream and a thread behind for a project that never
        // opened. Nothing before it needs the device.
        let (audio, audio_disabled) = open_audio(&first.path, first.audio_stream);
        let mut session = Self {
            meta,
            native,
            frames: stream.frames,
            worker: stream.worker,
            retired: Vec::new(),
            clock: PlaybackClock::new(match audio {
                Some(_) => ClockSource::Audio,
                None => ClockSource::Wall,
            }),
            audio,
            audio_disabled,
            project,
            span,
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

    /// Why this session plays silent although the file has sound -- an audio
    /// track in a codec we cannot decode, or one the decoder refused. `None`
    /// when there is audio, and for a file that never had any. A front-end
    /// shows it once, at open: without it the whole thing is a stderr line.
    pub fn audio_disabled_reason(&self) -> Option<&str> {
        self.audio_disabled.as_deref()
    }

    /// The files this timeline plays from, index 0 first -- a caller needs it
    /// to name an export or a window after the media rather than the project.
    pub fn sources(&self) -> &[Source] {
        self.project.sources()
    }

    /// Writes the timeline to `path` as a `.edith`, atomically (see
    /// [`crate::edith`]). Sources no clip plays from are left out, and the
    /// playhead is saved with it so a reopened project resumes where it stood.
    pub fn save_project(&self, path: &Path) -> crate::Result<()> {
        let (sources, lanes, eq, color) = self.project.without_orphan_sources();
        let playhead = secs_to_frame(self.now(), self.meta.frame_rate)
            .min(self.project.timeline_frames().saturating_sub(1));
        crate::edith::save(
            path,
            &sources,
            &lanes,
            &eq,
            &color,
            (self.meta.width, self.meta.height),
            playhead,
        )
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
                    // A gap's worker indexes from zero, a decoder's from its in
                    // point: `base` is whichever this span started at.
                    let base = self.span.from.map_or(0, |(_, in_frame)| in_frame);
                    frame.index = self.span.start + frame.index.saturating_sub(base);
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

    /// Installs `replacement` as the decode worker, parking the outgoing one in
    /// [`retired`](Self::retired) rather than joining it here, and reaping the
    /// parked ones that have already returned.
    ///
    /// The sweep is what bounds the list: every worker in it was cancelled
    /// before this ran, and a cancelled worker is out within an access unit
    /// (plus whatever VA-API init it was inside), so a scrub reaps last seek's
    /// worker on this seek and the list stays a handful long. Nothing waits.
    fn retire(&mut self, replacement: Worker) {
        self.retired
            .push(std::mem::replace(&mut self.worker, replacement));
        self.retired.retain(|w| !w.is_finished());
    }

    /// Starts feeding whatever follows the current span on the timeline --
    /// another clip, or a gap, which is black frames from a worker that opens no
    /// file. `false` past the end. The next timeline frame is derived rather
    /// than remembered as a clip index, because a `split` while playing cuts the
    /// clip under the running worker and only the mapping stays true.
    fn next_clip(&mut self) -> bool {
        let next = self.span.end();
        let Some(span) = self.project.composite_span_at(next) else {
            return false;
        };
        // We only get here on a disconnect, so the old worker has already
        // returned; cancel anyway, so `retire` treats every path alike.
        self.worker.cancel();
        self.start_span(span);
        true
    }

    /// Points the video worker at `span`: a decoder over its source range, or a
    /// black-frame generator for a gap. The old worker must already have been
    /// cancelled -- this is the half both `seek` and `next_clip` share.
    ///
    /// A source that will not open leaves the *span* installed anyway: the
    /// timeline still moves, there are simply no more pictures, and the
    /// disconnected receiver carries the session on to the next span.
    fn start_span(&mut self, span: Span) {
        let opened = match span.from {
            // The grade is the composite's at this frame -- the same clip the
            // span itself came from -- and it is constant across the span, so
            // the worker carries it and every frame it converts wears it.
            Some((source, in_frame)) => DecodeSession::open_worker(
                &self.project.sources()[source].path,
                in_frame,
                in_frame + span.len,
                self.project
                    .composite_color_at(span.start)
                    .copied()
                    .unwrap_or_default(),
                // ...and the canvas it is placed on: the project's resolution
                // and this clip's own fit policy, constant across the span for
                // the reason the grade is.
                Composer::new(
                    self.meta.width,
                    self.meta.height,
                    self.project.composite_fit_at(span.start),
                ),
            )
            .map(|(_, stream)| stream)
            .inspect_err(|e| eprintln!("timeline frame {}: video open failed: {e}", span.start)),
            None => Ok(DecodeSession::open_black(
                self.meta.width,
                self.meta.height,
                span.len,
            )),
        };
        if let Ok(stream) = opened {
            // Receiver first, worker second: the drop of the *old* receiver is
            // what wakes the outgoing worker if it is parked in `send`, and only
            // then can it return and be reaped by a later sweep. Nothing here
            // joins, so this ordering costs the seek nothing.
            self.frames = stream.frames;
            self.retire(stream.worker);
        }
        self.span = span;
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
        self.lane_spans_by_source(Lane::V1)
    }

    /// [`clip_spans_by_source`](Self::clip_spans_by_source) for either lane,
    /// with each clip's group id -- what a two-lane front-end draws. The holes
    /// between consecutive entries are the gaps.
    pub fn lane_spans_by_source(&self, lane: Lane) -> Vec<(f64, f64, usize)> {
        let fps = self.meta.frame_rate;
        self.project
            .lane(lane)
            .iter()
            .map(|c| (f64::from(c.start) / fps, f64::from(c.len()) / fps, c.source))
            .collect()
    }

    /// Every placement of one lane, in timeline order. What a two-lane
    /// front-end draws and nothing
    /// [`lane_spans_by_source`](Self::lane_spans_by_source) can answer: a clip
    /// carries its group id (which halves a click marks together) and its
    /// source range (where a waveform reads its peaks from) as well as its
    /// span. The holes between consecutive clips are the gaps.
    pub fn lane_clips(&self, lane: Lane) -> &[Clip] {
        self.project.lane(lane)
    }

    /// Every lane's handle, in display order -- what a front-end lays out top to
    /// bottom, and the list [`add_lane`](Self::add_lane) grows.
    pub fn lanes(&self) -> Vec<Lane> {
        self.project.lanes()
    }

    /// Appends an empty lane and hands back its handle. One undo step
    /// ([`Project::add_lane`]), and no reseek: an empty lane changes nothing
    /// that plays until something is placed on it.
    pub fn add_lane(&mut self, kind: LaneKind) -> Lane {
        self.project.add_lane(kind)
    }

    /// Splits every lane at `timeline_secs`, so the two sides become two
    /// groups. Metadata only: a split never changes the timeline->source
    /// mapping, so the running decoder stays correct and playback does not
    /// blink. `false` at a clip start, in a gap and past the end, where there
    /// would be nothing to split off.
    pub fn cut_at(&mut self, timeline_secs: f64) -> bool {
        self.project
            .split(secs_to_frame(timeline_secs, self.meta.frame_rate))
    }

    /// The inverse of [`cut_at`](Self::cut_at): rejoins the clips that meet at
    /// `timeline_secs` in every lane and puts them back in one group. `false`
    /// unless a split could have produced what is there. Metadata only, like the
    /// split -- no reseek.
    pub fn regroup_at(&mut self, timeline_secs: f64) -> bool {
        self.project
            .regroup(secs_to_frame(timeline_secs, self.meta.frame_rate))
    }

    /// What the clip at `idx` of `lane` is equalized with, or `None` for one
    /// that plays flat -- what a card shows before it lets anyone drag a band.
    pub fn eq_of(&self, lane: Lane, idx: usize) -> Option<&EqParams> {
        self.project.eq_of(lane, idx)
    }

    /// Gives that clip an equalizer, or takes it off with `None`. One undo step,
    /// like every other edit -- and, like every other edit, it *reseeks*: the
    /// audio workers are rebuilt from the new curves right where the playhead
    /// is, so a band changed during playback is heard from the next chunk on
    /// rather than at the next play. `false` for a bad index or a non-finite
    /// band, and nothing changes.
    ///
    /// ponytail: the reseek is what makes it live, so the cost of a change is a
    /// decoder restart -- inaudible at a drag's end, but too much to call once
    /// per pointer sample (a caller commits one change per gesture). Upgrade
    /// path is an `Arc` swap the running worker polls per chunk.
    pub fn set_eq(&mut self, lane: Lane, idx: usize, params: Option<EqParams>) -> bool {
        self.edit(|p| p.set_eq(lane, idx, params))
    }

    /// Lifts one lane's clip out, leaving a gap: black frames on the video lane,
    /// silence on the audio one, and nothing else moves. Reseeks, because what
    /// the playhead sits on has changed. `false` for a bad index and for the
    /// lift that would leave the whole timeline empty.
    pub fn lift_clip(&mut self, lane: Lane, idx: usize) -> bool {
        self.edit(|p| p.lift(lane, idx))
    }

    /// Moves that clip onto another lane of the same kind, keeping the frames it
    /// covers -- the drag between tracks. One undo step, and it reseeks like
    /// every other edit: which lane a clip sits on is what the compositor's
    /// topmost-lane-wins rule reads, so the frame on screen is recomposed at
    /// once. `false` for a bad index, for a move across kinds and for one that
    /// would land on another clip; nothing changes.
    pub fn move_clip_to_lane(&mut self, from: Lane, idx: usize, to: Lane) -> bool {
        self.edit(|p| p.move_to_lane(from, idx, to))
    }

    /// The clip the picture is coming from at `timeline_secs`: the lane it sits
    /// on and its index there, by the same topmost-lane-wins rule the decoder
    /// follows. `None` over a gap and past the end. What a card that grades
    /// "this clip" opens on when nothing has been clicked.
    pub fn video_clip_at(&self, timeline_secs: f64) -> Option<(Lane, usize)> {
        self.project
            .composite_clip_at(secs_to_frame(timeline_secs, self.meta.frame_rate))
    }

    /// Which clip of `lane` sits at `timeline_secs`, by the same rule
    /// [`video_clip_at`](Self::video_clip_at) picks the picture's with -- per
    /// lane, so a keyboard walking the lanes for something to select asks the
    /// engine rather than re-deriving where a clip ends.
    pub fn lane_clip_at(&self, lane: Lane, timeline_secs: f64) -> Option<usize> {
        self.project
            .lane_clip_at(lane, secs_to_frame(timeline_secs, self.meta.frame_rate))
    }

    /// How the clip at `idx` of `lane` is graded, or `None` for one that plays
    /// as it was shot -- what a colour card reads when it opens.
    pub fn color_of(&self, lane: Lane, idx: usize) -> Option<&ColorParams> {
        self.project.color_of(lane, idx)
    }

    /// Grades that clip, or takes the grade off with `None`. One undo step like
    /// every other edit, and it reseeks like one too -- which is what puts the
    /// new grade on the frame that is already on screen, paused or playing,
    /// without the caller having to nudge the playhead itself. `false` for an
    /// index that is not there and for a value that is not finite.
    pub fn set_color(&mut self, lane: Lane, idx: usize, params: Option<ColorParams>) -> bool {
        self.edit(|p| p.set_color(lane, idx, params))
    }

    /// The same grade and the same reseek without the undo step
    /// ([`Project::set_color_live`]): what the samples inside one slider drag go
    /// through, so the frame regrades under the hand and the whole gesture is
    /// still a single `z`.
    pub fn set_color_live(&mut self, lane: Lane, idx: usize, params: Option<ColorParams>) -> bool {
        self.edit(|p| p.set_color_live(lane, idx, params))
    }

    /// The project's resolution: what every clip is composed onto, what the
    /// window is sized to and what an export writes -- *not* any one file's.
    /// It starts as source 0's picture, which is what a project meant before it
    /// could have a resolution of its own.
    pub fn resolution(&self) -> (u32, u32) {
        (self.meta.width, self.meta.height)
    }

    /// Source 0's own picture size -- what the project resolution started as,
    /// and the one size a caller offering a list of them must not leave out.
    pub fn native_resolution(&self) -> (u32, u32) {
        self.native
    }

    /// Sets it. Reseeks like an edit, so the frame on screen is recomposed at
    /// the new size at once, paused or playing. `false` for a size that is not a
    /// picture -- zero either way, or past 8K, which is where the per-frame
    /// buffers stop being a sane thing to allocate from a keystroke.
    ///
    /// ponytail: not an undo step. The project resolution is not in the lane
    /// snapshots [`Project::undo`] restores, so cycling it back is one more
    /// keypress rather than a `z`. Upgrade path is snapshotting it beside the
    /// lanes, which every existing snapshot site would then have to carry.
    pub fn set_resolution(&mut self, width: u32, height: u32) -> bool {
        if !crate::is_resolution(width, height) {
            return false;
        }
        if (width, height) == (self.meta.width, self.meta.height) {
            return false;
        }
        self.meta.width = width;
        self.meta.height = height;
        let now = self.now();
        self.seek(now);
        true
    }

    /// How the clip at `idx` of `lane` meets a project canvas of another shape.
    pub fn fit_of(&self, lane: Lane, idx: usize) -> FitPolicy {
        self.project.fit_of(lane, idx)
    }

    /// Sets that clip's fit policy. One undo step and a reseek, exactly like a
    /// grade: the picture on screen is recomposed through the new policy without
    /// the caller nudging the playhead. `false` for an index that is not there.
    pub fn set_fit(&mut self, lane: Lane, idx: usize, fit: FitPolicy) -> bool {
        self.edit(|p| p.set_fit(lane, idx, fit))
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

    /// Places `frames` of `path` played on its audio `stream` at
    /// `timeline_secs`, the way [`paste_at`](Self::paste_at) places a copy --
    /// the door a library row goes through, and the only way a stream other
    /// than the one an import brought in reaches the timeline. The `(file,
    /// stream)` pair becomes a source if it is not one already.
    ///
    /// Refused, changing nothing, unless that stream can join *this* timeline:
    /// same audio parameters as the first source, in the same words
    /// [`import`](Self::import) refuses a file with. One output device and one
    /// copied AAC track mean one set of audio parameters for the whole
    /// timeline, so a 22 kHz mono track cannot join a 44.1 kHz stereo one --
    /// there is no resampler here, and no AAC encoder to write the join with.
    /// A front-end greys such a row out; this is the backstop that keeps a
    /// stale one from making the whole timeline silent.
    ///
    /// Which lane it lands on is decided here, from the file and from `onto` --
    /// the lane it was let go over, if it was let go over one at all: a file
    /// with a picture asked for by no lane, or for one of the first pair, is
    /// pasted across `V1` and `A1` as a grouped take; asked for by any further
    /// lane it is *placed* there alone, overwriting what it lands on and
    /// rippling nothing. A file with no picture ([`crate::is_audio`]) is placed
    /// on the audio lane it was asked for, or on `A1`, and never on a video
    /// lane. A caller never has to ask.
    pub fn place_stream_at(
        &mut self,
        timeline_secs: f64,
        path: &Path,
        stream: usize,
        frames: u32,
        onto: Option<Lane>,
    ) -> crate::Result<bool> {
        let wanted = Source::new(path, stream);
        // Another stream of a file whose picture is *already* on the timeline:
        // that is what a library row is, and it is why nothing here has to
        // check dimensions or frame rate -- the file passed that at import.
        if !self.project.sources().iter().any(|s| s.path == wanted.path) {
            return Err(format!("{} is not on this timeline", path.display()).into());
        }
        let first = self.first_audio()?;
        audio_matches(&wanted, &first)?;
        let source = self.project.import(path, stream);
        let clip = Clip {
            start: 0,
            in_frame: 0,
            out_frame: frames.max(1),
            source,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
        };
        // Which lane a source may land on is decided here and only here, so a
        // front-end never has to make the same call twice: a file with no
        // picture goes on an audio lane alone, overwriting and rippling
        // nothing, because there is nothing on the video lane to move along
        // with it. A lane of the source's own kind takes it as it is dropped;
        // only `V1` means the grouped take, since that is the pair the paste
        // spans and a second video lane is a layer of its own.
        let onto = match (crate::is_audio(path), onto) {
            // No picture: an audio lane and nothing else, whichever one was
            // asked for -- `A1` when none was.
            (true, Some(lane)) if lane.kind == LaneKind::Audio => Some(lane),
            (true, _) => Some(Lane::A1),
            // A picture: the lane it was let go over, unless that is one of the
            // first pair. Those two are the grouped take a paste spans, and a
            // further lane is a layer of its own -- its picture on `V2`, its
            // sound on `A2`.
            (false, Some(lane)) if lane.ord > 0 => Some(lane),
            (false, _) => None,
        };
        Ok(match onto {
            Some(lane) => self.place_at(lane, timeline_secs, clip),
            None => self.paste_at(timeline_secs, clip),
        })
    }

    /// What the timeline's audio *is*: source 0's chosen stream, probed. Every
    /// other source is held to it.
    fn first_audio(&self) -> crate::Result<Option<crate::AudioProbe>> {
        let first = &self.project.sources()[0];
        AudioSession::probe(&first.path, first.audio_stream)
    }

    /// Places `clip` on `lane` alone at the playhead, overwriting what it lands
    /// on and rippling nothing -- the one-lane insert a source with no picture
    /// makes, and the only way an audio-only file reaches the timeline. The
    /// placement belongs to no group (see [`Project::place`]). One undo step,
    /// and it reseeks like every other edit.
    pub fn place_at(&mut self, lane: Lane, timeline_secs: f64, clip: Clip) -> bool {
        let at = secs_to_frame(timeline_secs, self.meta.frame_rate);
        self.edit(|p| p.place(lane, at, clip))
    }

    /// Removes `lane`'s clip at `idx` and everything under it, closing the gap
    /// on every lane. Unlike a split this *does* move every following frame, so
    /// the session reseeks to wherever the playhead now points.
    /// [`lift_clip`](Self::lift_clip) is the one that leaves a hole instead.
    /// `false` for a bad index or the last remaining clip.
    ///
    /// The lane travels because the index is a lane's own: `V2`'s third clip is
    /// not `V1`'s, and a front-end that could only say "the third clip" would
    /// delete the wrong one the moment a second lane exists.
    pub fn delete_clip(&mut self, lane: Lane, idx: usize) -> bool {
        self.edit(|p| p.delete_in(lane, idx))
    }

    /// Undoes the last successful edit, and reseeks like a delete.
    pub fn undo(&mut self) -> bool {
        self.edit(Project::undo)
    }

    /// Appends the whole of `path` to the end of the timeline. One undo step,
    /// and the file becomes a source only if it is not one already.
    ///
    /// Refused unless it can join the timeline -- same codec, same frame rate,
    /// same audio parameters or both silent; the `Err` names the property that
    /// disagrees, for a caller to show. Nothing is changed by a refusal. A
    /// *resolution* of its own is not a refusal: the clip is placed on the
    /// project canvas by its fit policy ([`PlaybackSession::set_fit`]).
    pub fn import(&mut self, path: &Path) -> crate::Result<()> {
        if crate::is_audio(path) {
            return self.import_audio(path);
        }
        let (meta, _) = Demuxer::open(path)?;
        let first = self.first_audio()?;
        // Stream 0: an import brings a file in on its first audio track, and
        // [`place_stream_at`](Self::place_stream_at) is how any other one of
        // its streams reaches the timeline afterwards.
        matches_timeline(&Source::new(path, 0), &meta, &self.meta, &first)?;

        let old_end = self.timeline_duration();
        let source = self.project.import(path, 0);
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

    /// Appends a standalone audio file to the end of the timeline, on the
    /// **audio lane alone**: a song has no picture, so there is nothing to put
    /// on the video lane and the timeline shows black under it. Its length is
    /// its playing time rounded up to whole frames, which is the only frame
    /// count such a source has.
    ///
    /// Refused in the same words as any other import when its rate or layout
    /// disagrees with the timeline's -- one output device, one set of
    /// parameters -- and refused outright on a silent timeline, which has no
    /// device open at all. Nothing is changed by a refusal.
    fn import_audio(&mut self, path: &Path) -> crate::Result<()> {
        let first = self.first_audio()?;
        // Stream 0, and there is no other: a standalone audio file carries one
        // track (`AudioSession::probe_streams`), so nothing here has a stream
        // to pick the way `place_stream_at` does for an mp4.
        audio_matches(&Source::new(path, 0), &first)?;
        let frames = audio_frames(path, self.meta.frame_rate)?;

        let old_end = self.timeline_duration();
        let source = self.project.import(path, 0);
        let at = self.project.timeline_frames();
        self.project.place(
            Lane::A1,
            at,
            Clip {
                start: at,
                in_frame: 0,
                out_frame: frames,
                source,
                link: None,
                eq: None,
                color: None,
                fit: FitPolicy::default(),
            },
        );
        // Same reason as a video import: the running audio worker's segment
        // list stops at the old end, so without a reseek the appended clip
        // would play silent.
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

    /// Sets the monitoring gain, 0.0 (silent) to 1.0. Mute is a gain of 0.0:
    /// one knob, because muting and turning it down are the same thing to the
    /// device, and *which* the user meant is a question only the UI can answer
    /// -- it keeps the level to come back to.
    ///
    /// Deliberately not session state: nothing here remembers it, so a caller
    /// pushes it once per session and an export -- which renders the file
    /// rather than what the speakers are doing -- can never pick it up. Silent
    /// with no audio device, which is also the only honest answer there.
    ///
    /// Mute is not pause: the samples are still consumed and the device clock
    /// still runs, so the picture keeps playing against it.
    pub fn set_gain(&self, gain: f32) -> bool {
        match &self.audio {
            Some(audio) => audio.ao.lock().unwrap().set_volume(gain),
            None => false,
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
        // then finds the flag already set. It is then parked, not joined; see
        // [`retire`](Self::retire), which is what keeps a scrub off the price
        // of a VA-API init.
        self.worker.cancel();
        // `target` is inside the timeline (never empty), so this always spans;
        // the span runs from there to the end of whatever it landed in -- a
        // clip, or a gap, which starts a black-frame worker instead.
        if let Some(span) = self.project.composite_span_at(target) {
            self.start_span(span);
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
            // One play list per audio lane, summed by the mixer: what the ear
            // hears is every lane at once, and a lane's own gaps are silence in
            // it rather than a hole in the count the master clock keeps.
            let segs = self.project.audio_segments_from(target, fps);
            // The equalizers beside them, one per segment: the worker filters a
            // segment's own samples before the mix, so a clip's curve reaches
            // that clip and stops. Rebuilt on every seek, which is what makes an
            // EQ edit audible at once -- `edit` reseeks, so the drag that
            // changed a band restarts the workers with the new curve already in
            // them, and no live channel has to reach into a running one.
            let eqs = self.project.audio_eqs_from(target, fps);
            // Each source on the stream it was placed with: what plays is what
            // the library row said, and what an export copies (`export::run`).
            let sources = self.project.audio_sources();
            audio_running = match AudioSession::open_mixed_streams_eq(&sources, &segs, &eqs) {
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
    ///
    /// ponytail: this defaults the settings; the app still calls it that way,
    /// and the export options card replaces the call with
    /// [`export_to_with`](PlaybackSession::export_to_with).
    pub fn export_to(&self, out: &Path) -> crate::ExportHandle {
        self.export_to_with(out, &crate::export::ExportSettings::default())
    }

    /// [`export_to`](PlaybackSession::export_to) with the output settings spelled
    /// out rather than defaulted.
    pub fn export_to_with(
        &self,
        out: &Path,
        settings: &crate::export::ExportSettings,
    ) -> crate::ExportHandle {
        crate::export::start(self.project.clone(), self.meta, out, settings)
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

impl Drop for PlaybackSession {
    /// The video workers -- the running one and every cancelled one still in
    /// [`retired`](PlaybackSession::retired) -- cancel and join themselves when
    /// those fields drop right after this, the running one behind its receiver
    /// so a `send` cannot hold the join. *That* is what a session must not exit
    /// without: no decode worker outlives it, so none can be inside libva when
    /// Mesa's `atexit` handlers free the state under it.
    ///
    /// This is the audio half: bumping the epoch retires the feeder at its next write,
    /// which drops the decode receiver with it. Neither is waited for -- no
    /// driver state is torn down on the audio side.
    fn drop(&mut self) {
        if let Some(audio) = &self.audio {
            audio.epoch.fetch_add(1, Ordering::Release);
        }
    }
}

/// Whether `path`, already demuxed to `meta`, may join a timeline whose
/// parameters are `timeline` and whose audio probes as `first`: same codec,
/// same frame rate, same audio parameters or both silent.
///
/// *Not* the same size any more. A project has its own resolution
/// ([`PlaybackSession::set_resolution`]) and every clip is placed on it by its
/// own fit policy, so a 640x360 file joining a 1920x1080 timeline is a
/// letterboxed clip rather than a refusal. The frame rate stays refused on
/// purpose: mixing rates means resampling the *timeline*, which is a different
/// problem from resampling a picture, and a refusal is the honest answer until
/// it is solved.
///
/// The `Err` names the property that disagrees, and those strings are what a
/// front-end shows verbatim; [`PlaybackSession::import`] and
/// [`PlaybackSession::open_project`] share them so a file refused at import is
/// refused in the same words at load.
fn matches_timeline(
    source: &Source,
    meta: &VideoMeta,
    timeline: &VideoMeta,
    first: &Option<crate::AudioProbe>,
) -> crate::Result<()> {
    // Codec first: one timeline is one kind of source. Mixing them would decode
    // (every clip opens its own decoder) but on a machine without the VA-API
    // plugin the VP9 half would be black frames with the refusal only on
    // stderr, which is exactly what a front-end cannot show.
    if meta.codec != timeline.codec {
        return Err(format!(
            "{} does not match the timeline's {}",
            meta.codec.name(),
            timeline.codec.name()
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
    audio_matches(source, first)
}

/// Whether a candidate's audio may join a timeline whose first source probes as
/// `first`: same rate, same layout, or both silent. One output device and one
/// exported track is all there is, and no resampler.
///
/// The audio half of [`matches_timeline`], which a stream placed on its own
/// ([`PlaybackSession::place_stream_at`]) has to pass while the picture it
/// comes with is already on the timeline.
///
/// The format itself is deliberately *not* part of this: an mp3 that agrees on
/// rate and layout plays alongside a timeline of mp4s perfectly well. What it
/// cannot do is be copied into an export, which is a refusal of its own, at
/// export time (`AudioSession::copy_multi_streams`).
fn audio_matches(source: &Source, first: &Option<crate::AudioProbe>) -> crate::Result<()> {
    // Whole-probe equality: rate and layout, which the audio worker holds every
    // source to anyway. Both silent is a match.
    let probe = AudioSession::probe(&source.path, source.audio_stream)?;
    if probe == *first {
        return Ok(());
    }
    Err(match (probe, first) {
        (None, _) => "the file is silent, the timeline has audio".to_string(),
        (_, None) => "the file has audio, the timeline is silent".to_string(),
        (Some(a), Some(b)) => format!(
            "audio {} Hz {} ch does not match the timeline's {} Hz {} ch",
            a.sample_rate, a.channels, b.sample_rate, b.channels
        ),
    }
    .into())
}

/// How many timeline frames a standalone audio file occupies: its playing time
/// rounded *up*, so the last partial frame is still covered rather than cut off,
/// and never zero (a clip is never empty).
fn audio_frames(path: &Path, fps: f64) -> crate::Result<u32> {
    let secs = AudioSession::duration_secs(path)?
        .ok_or_else(|| format!("{} has no audio track", path.display()))?;
    let frames = (secs * fps).ceil();
    if !frames.is_finite() || frames < 0.0 || frames > f64::from(u32::MAX) {
        return Err(format!(
            "{} is {secs} s long, which is not a timeline",
            path.display()
        )
        .into());
    }
    Ok((frames as u32).max(1))
}

/// `None` for a silent session -- no audio track, no plugin, no daemon, or a
/// track the decoder refuses. Dropping the receiver on the way out stops the
/// audio decode worker, so a failure here leaves nothing running.
///
/// The second half is why, for the cases the *file* is to blame for: those are
/// a surprise worth showing (see
/// [`audio_disabled_reason`](PlaybackSession::audio_disabled_reason)), where a
/// file with no audio at all and a machine with no output device are not.
fn open_audio(path: &Path, stream: usize) -> (Option<Audio>, Option<String>) {
    let sources = [(path.to_path_buf(), stream)];
    let (meta, rx) =
        match AudioSession::open_multi_streams(&sources, &[(Some(0), 0.0, f64::INFINITY)]) {
            Ok(Some(opened)) => opened,
            // No AAC track: silent by nature, or sound in a codec we do not
            // have -- and only the second of those is worth a word.
            Ok(None) => {
                let reason = AudioSession::unsupported(path).ok().flatten();
                if let Some(reason) = &reason {
                    eprintln!("audio disabled: {reason}");
                }
                return (None, reason);
            }
            Err(e) => {
                eprintln!("audio disabled: {e}");
                return (None, Some(e.to_string()));
            }
        };
    let Some(ao) = AoSession::open(meta.sample_rate, u32::from(meta.channels)) else {
        return (None, None); // no device: the machine's business, not the file's
    };
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
    (audio.spawn_feeder(rx).then_some(audio), None)
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
