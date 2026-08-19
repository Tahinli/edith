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
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::ao::AoSession;
use crate::audio::{AudioChunk, AudioSession};
use crate::clock::{ClockSource, PlaybackClock};
use crate::color::ColorParams;
use crate::colorspace::ColorDescription;
use crate::decode::{Backend, BackendCell, DecodeSession, Frame, Worker};
use crate::demux::{Codec, Demuxer, VideoMeta};
use crate::eq::EqParams;
use crate::project::{Clip, Edge, Lane, LaneKind, Project, Rate, Source, Span, Speed, SubClip};
use crate::scale::{Composer, FitPolicy};

/// How long the feeder waits out a full ring. The ring holds a second, so this
/// only has to be short next to that; it costs one wakeup per 10 ms of audio.
const RING_FULL_WAIT: Duration = Duration::from_millis(10);

/// How long the feeder waits out a ring that took *nothing*: the flush of a
/// restart, which only the device's own callback can clear (`engine-audio`
/// refuses a write queued behind one). Short, because this is the gap at the
/// head of every restart and the ear is in it -- one wakeup per millisecond,
/// for the millisecond or two it lasts.
const FLUSH_WAIT: Duration = Duration::from_millis(1);

/// The timeline a file with no picture scaffolds: 1080p at 30 fps, H.264 --
/// nothing was shot on it, so it is the canvas a *later* import meets rather
/// than a description of the song. H.264 because it is the one codec that
/// decodes without the plugin, 30 fps because a rounded-up audio length is what
/// the frame count means and a whole number of frames per second keeps it
/// honest; both are what a video imported onto such a timeline is held to
/// ([`matches_timeline`]).
///
/// corner-cut: a 25 fps video imported onto an audio-only timeline it could have
/// defined plays at 25 fps, correctly, but on a 30 fps timeline -- it is read
/// through [`Rate`] like any other file shot at another rate rather than
/// defining one. Upgrade path is adopting the first *picture's* rate and codec
/// on import, which means retiming every audio clip already placed in frames.
const AUDIO_ONLY_CANVAS: (u32, u32, f64) = (1920, 1080, 30.0);

/// The frame rate a timeline scaffolded from a still image runs at, for
/// [`AUDIO_ONLY_CANVAS`]'s reason: nothing was shot on it. The *size* is not
/// invented the same way -- an image has a picture of its own, and that picture
/// is the canvas a later import meets.
const IMAGE_ONLY_RATE: f64 = 30.0;

/// How long a still image *is*, in seconds: the wall a trim may drag its edge
/// out to, since an image has no length of its own to be walled by. Ten
/// minutes, which is a title card nobody will run out of and still a finite
/// number -- a `u32::MAX` sentinel here would make [`Self::trim_room`] hand a
/// drag a wall past what the timeline can hold.
///
/// corner-cut: a still cannot be held longer than this. Upgrade path is a
/// per-source length the project file carries, which every `counts` reader
/// would then have to take from the document rather than from the file.
const IMAGE_MAX_SECS: f64 = 600.0;

/// How long a still image is *placed* for: five seconds, the length a picture
/// gets when nobody has said one -- trimmable either way up to
/// [`IMAGE_MAX_SECS`].
const IMAGE_PLACE_SECS: f64 = 5.0;

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
    /// The device position the *current* stream's first real sample was queued
    /// at, `-1` until the feeder has queued one. Written by the feeder under the
    /// device lock, read by [`tick`](PlaybackSession::tick).
    ///
    /// This is what keeps a restart out of the timeline. Between a flush and the
    /// first sample of the new stream the device plays silence -- a decoder open
    /// is tens of milliseconds -- and that silence is elapsed *device* time
    /// ([`crate::ao`] counts it, as it must: it really was played). It is not
    /// elapsed *timeline* time, and a clock anchored on the first poll after a
    /// seek counts it as such, which is a permanent offset between the picture
    /// and the sound, re-rolled by every edit made while playing.
    content_at: Arc<AtomicI64>,
    /// The timeline is *meant* to be playing: what a play or a seek-while-playing
    /// asks for, and what the feeder turns into a running device the moment it
    /// has a real sample to play ([`Audio::set_playing`], [`feed`]).
    ///
    /// The device is not started by the intent, because an inactive stream has
    /// no callbacks and therefore no starved quanta: between the flush of a seek
    /// and the first sample of the new stream -- seconds, on a cold 25 GB film
    /// whose decoder is opened on the feeder -- a device started here would play
    /// its own silence and count every quantum of it as an underrun.
    ///
    /// Written and read under the device lock, beside the [`Audio::content_at`]
    /// stamp, and that is the whole race: whichever of the two sides takes the
    /// lock first, the other sees its half (a stamp already there, or an intent
    /// already set), so the device is started exactly once and a pause landing
    /// during an open can never be overtaken by a late activation.
    wants_active: Arc<AtomicBool>,
    /// The last [`TAP_SAMPLES`] mono samples handed to the device, oldest
    /// first: what a meter or an analyser draws. Written by the feeder, which
    /// is not the device's own callback -- the plugin's RT thread never sees
    /// this lock, and the feeder holds it for one memcpy per write.
    tap: Arc<Mutex<Vec<f32>>>,
    /// Feeder threads started and not yet returned. They are detached, so there
    /// is no handle to count instead, and a scrub leaves dozens of them exiting
    /// at once -- which is precisely what
    /// [`live_workers`](PlaybackSession::live_workers) is asked about.
    feeders: Arc<AtomicUsize>,
}

/// How much of the played signal the tap keeps. A power of two, because what
/// reads it transforms it, and long enough that the lowest band the editor
/// draws (20 Hz) has a whole cycle in the window at any rate we open.
const TAP_SAMPLES: usize = 2048;

impl Audio {
    /// Whether the device has played everything there will ever be, i.e. the
    /// audio clock is about to stop meaning anything.
    fn played_out(&self, position: i64) -> bool {
        self.fed_all.load(Ordering::Acquire)
            && position as u64 * self.channels >= self.fed.load(Ordering::Relaxed)
    }

    /// Says whether the timeline is playing, and starts or stops the device for
    /// it: `true` runs it *if there is already something to play*, and otherwise
    /// leaves it to the feeder's first sample ([`Audio::wants_active`]). `false`
    /// stops it at once -- a pause is never deferred, whatever an open in flight
    /// is about to find.
    fn set_playing(&self, playing: bool) {
        // The lock, not the atomics, is what orders this against the feeder's
        // stamp: the store below and the load after it are one critical section,
        // and so is the feeder's pair.
        let ao = self.ao.lock().unwrap();
        self.wants_active.store(playing, Ordering::Release);
        if !playing || self.content_at.load(Ordering::Acquire) >= 0 {
            ao.set_active(playing);
        }
    }

    /// Keeps the newest [`TAP_SAMPLES`] of what was just queued, downmixed to
    /// mono. Called from the feeder only, once per accepted write.
    fn note_tap(&self, accepted: &[f32]) {
        let channels = self.channels.max(1) as usize;
        let mut tap = self.tap.lock().unwrap();
        // Only the tail can survive anyway: a big write replaces the window
        // outright rather than being summed into it and then thrown away.
        let from = accepted.len().saturating_sub(TAP_SAMPLES * channels);
        for frame in accepted[from..].chunks_exact(channels) {
            tap.push(frame.iter().sum::<f32>() / channels as f32);
        }
        let over = tap.len().saturating_sub(TAP_SAMPLES);
        tap.drain(..over);
    }

    /// Starts a feeder for the current epoch, draining `rx` into the device.
    /// `false` if the thread would not start, i.e. nothing will ever be fed.
    fn spawn_feeder(&self, rx: Receiver<AudioChunk>) -> bool {
        self.spawn_feeder_deferred(move || Some(rx))
    }

    /// As [`spawn_feeder`](Self::spawn_feeder), but the *stream* is opened on
    /// the feeder too: `open` runs on that thread, so nothing here touches the
    /// disk and the caller pays a thread spawn instead of a demux -- seconds off
    /// a cold cache on a big film, which is what a seek used to cost the UI
    /// thread ([`crate::DecodeSession::open_worker_deferred`] is the picture's
    /// half of the same move).
    ///
    /// The price is the same one: there is no [`crate::audio::AudioMeta`] to
    /// hand back and no synchronous answer to "will anything play". `None` from
    /// `open` -- a silent timeline, a source that will not open -- is a feeder
    /// with nothing to feed, which ends exactly as a played-out stream does and
    /// hands the clock to wall time ([`tick`](PlaybackSession::tick)).
    ///
    /// The epoch is captured *here*, at the seek, not when the open returns: a
    /// scrub abandons opens by the dozen and every one of them must still be
    /// the stale stream it was started as, however late it lands.
    ///
    /// corner-cut: an abandoned open is not *cancelled*, though the machinery
    /// for it exists ([`crate::demux::with_cancel`], which the import's Cancel
    /// uses): it runs to the end and its answer is dropped. Tried, and left
    /// out for want of evidence -- 200 cold scrub steps over the 25 GB HEVC
    /// remux measured 101, 137 and 14 live workers on three runs of the *same*
    /// build, so the machine decides that number and not this. There is a
    /// reason to expect it to hurt, too: a Matroska walk writes its sidecar
    /// index only when it finishes (`demux::mkv_blocks`), so a drag that
    /// cancels every step's walk would never write one and every step would
    /// walk the segment again from the top. Ceiling: an abandoned open keeps
    /// reading, and the decoder threads it started live until it does. Upgrade
    /// path is an index the walk publishes as it goes -- then cancelling costs
    /// nothing and can be measured on an idle machine.
    fn spawn_feeder_deferred(
        &self,
        open: impl FnOnce() -> Option<Receiver<AudioChunk>> + Send + 'static,
    ) -> bool {
        let me = self.clone();
        let epoch = self.epoch.load(Ordering::Acquire);
        // Counted before the spawn, so the count is never behind the thread.
        self.feeders.fetch_add(1, Ordering::Release);
        let started = thread::Builder::new()
            .name("audio-feed".into())
            .spawn(move || {
                // Superseded before this thread was even scheduled: do not open
                // anything at all. A scrub leaves all but the last one here.
                if me.epoch.load(Ordering::Acquire) == epoch {
                    if let Some(rx) = open() {
                        feed(rx, &me, epoch);
                    }
                    // Only the current feeder gets to declare the end of the
                    // audio: a superseded one is finished, the stream is not.
                    if me.epoch.load(Ordering::Acquire) == epoch {
                        me.fed_all.store(true, Ordering::Release);
                    }
                }
                me.feeders.fetch_sub(1, Ordering::Release);
            })
            .is_ok();
        if !started {
            self.feeders.fetch_sub(1, Ordering::Release);
        }
        started
    }

    /// Feeder threads still running; see [`Self::feeders`].
    fn live_feeders(&self) -> usize {
        self.feeders.load(Ordering::Acquire)
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
    /// ...and the scaffolding source's own frame rate, kept for exactly that
    /// reason: the project rate starts here, and the list a caller offers rates
    /// from needs the media's own on it or a project moved off it has no way
    /// back ([`PlaybackSession::set_frame_rate`] is not an undo step either).
    native_fps: f64,
    frames: Receiver<Frame>,
    /// The current decode worker.
    worker: Worker,
    /// What that worker opened -- hardware, software, a still, a gap -- written
    /// by the worker itself and read by [`decode_backend`](Self::decode_backend).
    /// Replaced with the stream at every seek, so it always describes the
    /// pictures currently arriving and never the clip before them.
    backend: BackendCell,
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
    /// Picture workers that replaced another one, i.e. every restart the
    /// session's first worker did not cost: a seek, a resync, a clip boundary.
    /// Each carries a demuxer reopen and a VA-API init, so this is what a
    /// playback measurement counts to say how often the picture was thrown away
    /// ([`restarts`](Self::restarts)).
    restarts: u64,
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
    /// How many frames each source actually holds, indexed exactly as
    /// [`Project::sources`] is and grown with it. The project itself does not
    /// know -- a clip names a source by index and carries its own range -- and
    /// [`Self::trim_clip`] is the one edit that could ask for frames past the
    /// end of a file, which is a save that would not open again
    /// ([`Self::open_project`] refuses one by name). Append-only for
    /// `sources`'s reason: an index handed out stays valid, undone import or
    /// not -- with the single exception `sources` itself has,
    /// [`Self::remove_source`], which takes the same entry out of both.
    counts: Vec<u32>,
    /// Audio headers already read, by the `(path, stream)` they were read for.
    /// Every door that checks a file against the timeline asks the same two
    /// questions -- what this file's sound is, and what the timeline's first
    /// source's is ([`audio_matches`], [`first_audio_of`]) -- and each answer is
    /// a container open. Warm that is 5-10 ms; on a film whose pages have been
    /// evicted it is **1.7 s** (measured, 3.1 GB HEVC mkv), and both of those
    /// are spent on the thread that draws, because placing and importing are
    /// what a drag lands on. A header does not change while a session holds the
    /// file, so it is read once and remembered. Errors are not kept: a file that
    /// failed to open is asked again next time.
    probes: std::collections::HashMap<(std::path::PathBuf, usize), Option<crate::AudioProbe>>,
    /// What each source's own frame rate is against this timeline's, indexed
    /// and grown exactly as [`Self::counts`] is (and shortened with it by
    /// [`Self::remove_source`]). [`Rate::REAL_TIME`] for a file shot at the
    /// timeline's rate -- and for a still and a song, which have no rate of
    /// their own -- so a single-rate project converts nothing anywhere.
    ///
    /// A clip counts *timeline* frames ([`Rate`]); this is the one thing that
    /// knows which frames of the file those are, and only
    /// [`start_span`](Self::start_span) and [`try_frame`](Self::try_frame) ask.
    rates: Vec<Rate>,
    /// The rate of the source the current worker is decoding, kept beside
    /// [`Self::span`] because the frames coming back are numbered in *its*
    /// file's frames and nothing else can put them back on the timeline.
    span_rate: Rate,
    /// What the current video worker was opened for: where it sits on the
    /// timeline, how long it runs, and which source frame it started at --
    /// together they rewrite a source frame index into a timeline one. Not a
    /// clip index: a `split` cuts the clip under a running worker, and only the
    /// mapping survives that. A span with no source is a *gap*, and the worker
    /// feeding it emits black frames indexed from zero.
    ///
    /// `None` is the *emptied* timeline -- no clip on any lane, so there is no
    /// stretch to be inside of. It is a state, not a failure: the picture is
    /// black ([`start_span`](Self::start_span) feeds one black frame for it),
    /// the sound is silence, the duration is zero, and placing anything reseeks
    /// straight back out of it.
    span: Option<Span>,
    /// Whether the span now decoding still owes its first picture: set by
    /// every span start ([`start_span`](Self::start_span)), cleared by the
    /// first frame that span hands over ([`try_frame`](Self::try_frame)).
    /// This is the window a clip boundary -- or any picture restart -- spends
    /// reopening the decoder, and a front-end must not answer a picture that
    /// is late by exactly this window's length with another restart of it
    /// ([`picture_priming`](Self::picture_priming)).
    span_priming: bool,
    /// The last clip has been played out; see [`PlaybackSession::is_eos`].
    eos: bool,
    /// The mix the running mixer is reading, when there is one: what lets a
    /// fader and the master ceiling move without rebuilding the audio pipeline
    /// at all ([`crate::audio::MixControls`]). `None` for a timeline that is
    /// not mixed -- one audio lane at unity with the limiter off, which is the
    /// bit-exact single-stream path -- and the first move off that reopens the
    /// stream once, mixed, and lives from there.
    mix: Option<Arc<crate::audio::MixControls>>,
    /// An audio stream has been started and has not yet played a sample: the
    /// window [`tick`](Self::tick) holds the clock still through, so the silence
    /// of a restart is never counted as timeline time. See
    /// [`Audio::content_at`].
    priming: bool,
    /// Whether the decode worker may drop a picture whose moment has already
    /// passed ([`Self::drop_late_pictures`]). Off unless a caller says it is
    /// watching this in real time.
    drop_late: bool,
    /// Whether the picture is decoded from the stand-ins rather than from the
    /// films ([`crate::proxy`]). One switch for the whole project, saved with
    /// it, and *only* the picture: the sound is always the film's own, and so
    /// is everything an export reads.
    ///
    /// Which sources have a stand-in is not kept here -- it is asked of the
    /// cache when a span opens ([`Self::picture_path`]), one `stat` against a
    /// file open that already costs milliseconds, so a proxy made (or deleted)
    /// mid-session is picked up at the next seek and nothing can go stale.
    proxies: bool,
    /// Whether a source that wants a stand-in gets one made for it as it
    /// arrives ([`crate::proxy::wanted`]). On unless a project says otherwise,
    /// which is what every project did before the switch existed. Saved with
    /// the project like the one above it, and read by the front end: the engine
    /// starts no encode of its own.
    auto_proxies: bool,
    /// Which encoder an export of this project writes its picture with
    /// ([`crate::export::EncoderSeat`]). The project's, saved with it like the
    /// two switches above, and read by the front end: the engine starts no
    /// export of its own.
    encoder_seat: crate::export::EncoderSeat,
    /// The rate this project's mix was picked to run at
    /// ([`crate::edith::Document::sample_rate`]), overriding the one the first
    /// audio source would otherwise hand the mixer. `None` -- every project
    /// before the field existed, and any project nobody has picked one for --
    /// leaves the derived rate alone. Saved with the project like the switches
    /// above it, and read by the front end: the engine picks no rate of its own.
    ///
    /// Setting it takes effect at the next audio rebuild (a seek, an edit, a
    /// reopen); there is no live resample of an audio device already playing.
    sample_rate: Option<u32>,
}

/// What an edit dirtied -- which half of the pipeline has to be rebuilt for the
/// change to be seen or heard, and, far more to the point, which half must not
/// be.
///
/// Rebuilding a half is expensive and *audible*: the video worker costs a
/// decoder open (98 ms of VA-API init, worst case, measured) and the audio one
/// costs a flush, a re-open and a hole in the sound. A grade changes no sample
/// and a fader changes no picture, so neither may pay the other's price -- that
/// is the whole of this type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dirty {
    /// The picture only: a grade, a fit policy, the project resolution. The
    /// sound keeps playing, untouched, and the clock never moves.
    Picture,
    /// The sound only: a clip's equalizer. The picture keeps playing.
    Sound,
    /// The timeline itself -- what is where, how long, how fast. Both halves
    /// are rebuilt, because both are decoding the wrong thing.
    Both,
}

/// What an import is held to, off a timeline that is already up: the sources
/// its sound has to agree with and the meta its rate is read against. Owned and
/// `Send`, so the check itself can run on a worker with no session in reach --
/// which is the whole point ([`PlaybackSession::probe_import`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ImportGate {
    sources: Vec<Source>,
    timeline: VideoMeta,
}

/// A file read and accepted, waiting to be registered: what
/// [`PlaybackSession::probe_import`] found and
/// [`PlaybackSession::import_probed`] pushes. Carries the gate it was decided
/// against, so a timeline that moved under it can be noticed rather than
/// trusted.
#[derive(Clone, Debug)]
pub struct ImportProbe {
    gate: ImportGate,
    meta: VideoMeta,
    rate: Rate,
}

impl PlaybackSession {
    /// Opens `path` and starts both decode workers. Only video failure is fatal:
    /// a file we cannot hear is still a file we can watch.
    pub fn open(path: impl AsRef<Path>) -> crate::Result<Self> {
        Self::open_stream(path, 0)
    }

    /// Opens `path` exactly as [`open`](Self::open) does, but binds `stream` --
    /// the index a library row names ([`crate::probe`]'s own numbering of a
    /// file's audio tracks) -- as the one that keeps the clock and plays,
    /// rather than always the first. What a library preview asks for when the
    /// row *is* a particular stream, so a remux with several audio tracks
    /// previews the one that was clicked.
    pub fn open_with_audio_stream(
        path: impl AsRef<Path>,
        audio_stream: usize,
    ) -> crate::Result<Self> {
        Self::open_stream(path, audio_stream)
    }

    fn open_stream(path: impl AsRef<Path>, audio_stream: usize) -> crate::Result<Self> {
        let path = path.as_ref().to_path_buf();
        // A timeline is normally scaffolded from source 0's picture -- its size,
        // its frame rate, its clock. A song has none of that, so it scaffolds
        // the *canvas* instead and its own sound keeps the clock: an audio-only
        // project is a project like any other, it simply plays black.
        if crate::is_audio(&path) {
            return Self::open_audio_only(&path, audio_stream);
        }
        // A still is the other source with no timeline in it: it has a picture
        // (which becomes the canvas) but no rate and no length, so it too
        // scaffolds rather than describes.
        if crate::is_image(&path) {
            return Self::open_image_only(&path);
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
            // The default rendition: a file just opened is a project nobody has
            // picked one for, exactly as it is a project nobody has graded.
            crate::tonemap::Preset::default(),
        )?;
        // A file is opened on its first audio stream by default, like
        // `Project::single` names it -- unless a caller through
        // `open_with_audio_stream` picked another.
        let (audio, audio_disabled) = open_audio(&path, audio_stream);
        let source = match audio {
            Some(_) => ClockSource::Audio,
            None => ClockSource::Wall,
        };
        // One clip per lane covering the file, so timeline == source until the
        // first edit -- and the range opened above is exactly that clip's.
        let project = Project::single(&path, meta.frame_count);
        let span = project.composite_span_at(0);
        Ok(Self {
            meta,
            native: (meta.width, meta.height),
            native_fps: meta.frame_rate,
            frames: stream.frames,
            worker: stream.worker,
            backend: stream.backend,
            retired: Vec::new(),
            restarts: 0,
            clock: PlaybackClock::new(source),
            audio,
            audio_disabled,
            project,
            counts: vec![meta.frame_count],
            probes: std::collections::HashMap::new(),
            // Source 0 *is* the timeline's rate: it defined it.
            rates: vec![Rate::REAL_TIME],
            span_rate: Rate::REAL_TIME,
            span,
            span_priming: true,
            eos: false,
            mix: None,
            priming: false,
            drop_late: false,
            // A file just opened is a project nobody has picked stand-ins for.
            proxies: false,
            auto_proxies: true,
            encoder_seat: crate::export::EncoderSeat::default(),
            sample_rate: None,
        })
    }

    /// Opens `path` into the **library alone**: everything
    /// [`open`](Self::open) scaffolds from it -- the resolution, the frame
    /// rate, the clock, the source entry and its length -- over an *empty*
    /// timeline. What an import into a window that has no session yet makes,
    /// since an import registers a source and never places a clip
    /// ([`import`](Self::import)); the file reaches a lane when it is dragged
    /// there.
    ///
    /// Nothing to undo, like the import it stands in for: the lanes start
    /// empty rather than being emptied, so no snapshot is pushed.
    pub fn open_library(path: impl AsRef<Path>) -> crate::Result<Self> {
        let mut session = Self::open(path)?;
        // The pair of lanes an opened file comes up with (`Project::single`,
        // `open_audio_only`), emptied -- the source list and so the noted
        // counts are carried over untouched, which is what keeps a library row
        // placeable at its full length.
        session.project = Project::from_parts(
            session.project.sources().to_vec(),
            vec![(LaneKind::Video, Vec::new()), (LaneKind::Audio, Vec::new())],
            Vec::new(),
            Vec::new(),
        )?;
        // Onto the emptied mapping: black picture, silence, zero duration.
        session.seek(0.);
        Ok(session)
    }

    /// Opens a standalone audio file ([`crate::is_audio`]) as a timeline of its
    /// own: one clip on `A1`, nothing on the video lane, and a picture that is
    /// the black of an uncovered canvas ([`AUDIO_ONLY_CANVAS`]) for as long as
    /// the song runs. Its own sound keeps the clock, exactly as a video's does.
    ///
    /// Refused when the file has no sound to time it by: a source with neither a
    /// picture nor a playable length is not a timeline, and that is what
    /// [`audio_frames`] answers -- the *device* is a different question, and a
    /// machine without one opens this like any other session, silently.
    fn open_audio_only(path: &Path, stream: usize) -> crate::Result<Self> {
        let (width, height, frame_rate) = AUDIO_ONLY_CANVAS;
        let meta = VideoMeta {
            width,
            height,
            frame_rate,
            // Rounded up to whole frames, which is the only frame count a
            // source with no picture has -- the same length `import_audio`
            // gives a song joining a timeline of video. The demuxer's words are
            // named with the file, since this is the door a file that is not
            // really audio at all comes to.
            frame_count: audio_frames(path, frame_rate)
                .map_err(|e| format!("{}: {e}", path.display()))?,
            codec: Codec::H264,
            // A canvas this engine drew itself, not a stream that was read:
            // the default description is what it is drawn in.
            color: ColorDescription::default(),
        };
        let (audio, audio_disabled) = open_audio(path, stream);
        // Through `from_parts` rather than `Project::single`, which is the
        // *video* open's pair of grouped clips: here there is no picture to
        // group the sound with, so the video lane starts empty.
        let clip = Clip {
            fade_in: 0,
            fade_out: 0,
            start: 0,
            in_frame: 0,
            out_frame: meta.frame_count,
            source: 0,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };
        let project = Project::from_parts(
            vec![Source::new(path, 0)],
            vec![(LaneKind::Video, Vec::new()), (LaneKind::Audio, vec![clip])],
            Vec::new(),
            Vec::new(),
        )?;
        // Every frame of this timeline is a gap, since no video lane covers
        // anything: `composite_span_at` says so and the black worker
        // `start_span` would open is opened here instead, for the reason `open`
        // opens its decoder inline -- the field has to exist for the first seek.
        let span = project.composite_span_at(0);
        let stream = DecodeSession::open_black(width, height, span.map_or(1, |s| s.len));
        Ok(Self {
            meta,
            native: (width, height),
            native_fps: frame_rate,
            frames: stream.frames,
            worker: stream.worker,
            backend: stream.backend,
            retired: Vec::new(),
            restarts: 0,
            // The song keeps the clock, as a video's own sound does -- and wall
            // time keeps it on a machine with no device, where the picture
            // (black) still has to move at some rate.
            clock: PlaybackClock::new(match audio {
                Some(_) => ClockSource::Audio,
                None => ClockSource::Wall,
            }),
            audio,
            audio_disabled,
            project,
            counts: vec![meta.frame_count],
            probes: std::collections::HashMap::new(),
            // Source 0 *is* the timeline's rate: it defined it.
            rates: vec![Rate::REAL_TIME],
            span_rate: Rate::REAL_TIME,
            span,
            span_priming: true,
            eos: false,
            mix: None,
            priming: false,
            drop_late: false,
            // A file just opened is a project nobody has picked stand-ins for.
            proxies: false,
            auto_proxies: true,
            encoder_seat: crate::export::EncoderSeat::default(),
            sample_rate: None,
        })
    }

    /// Opens a still image ([`crate::is_image`]) as a timeline of its own: one
    /// clip on `V1`, [`IMAGE_PLACE_SECS`] long, over silence. The *canvas* is
    /// the image's own picture -- unlike a song, a still has one, and inventing
    /// a 1080p canvas around a 640x360 PNG would letterbox it against nothing.
    /// The frame rate is [`IMAGE_ONLY_RATE`], for the reason an audio-only
    /// timeline has one at all: nothing was shot on it.
    ///
    /// Wall time keeps the clock, as it does for any silent source.
    fn open_image_only(path: &Path) -> crate::Result<Self> {
        // Decoded here rather than probed: this is the door a `.png` that is
        // not one comes to, and the picture is wanted a line later anyway.
        let still = crate::decode::Still::open(path)?;
        let meta = VideoMeta {
            width: still.width,
            height: still.height,
            frame_rate: IMAGE_ONLY_RATE,
            // Its length is the wall a trim is held to, exactly as a song's
            // playing time is -- see [`image_frames`].
            frame_count: image_frames(IMAGE_ONLY_RATE),
            codec: Codec::H264,
            color: ColorDescription::default(),
        };
        let clip = Clip {
            fade_in: 0,
            fade_out: 0,
            start: 0,
            in_frame: 0,
            out_frame: place_frames(meta.frame_count, IMAGE_ONLY_RATE),
            source: 0,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };
        // No group and no audio clip: a still is silent, and a clip on `A1`
        // playing from a PNG is a source the audio worker cannot open.
        let project = Project::from_parts(
            vec![Source::new(path, 0)],
            vec![(LaneKind::Video, vec![clip]), (LaneKind::Audio, Vec::new())],
            Vec::new(),
            Vec::new(),
        )?;
        let span = project.composite_span_at(0);
        // The placeholder the first seek supersedes, opened here for `open`'s
        // reason: the field has to exist before anything can seek.
        let stream = DecodeSession::open_still(
            path,
            0,
            span.map_or(1, |s| s.len),
            ColorParams::default(),
            Composer::passthrough(),
        )?;
        Ok(Self {
            meta,
            native: (meta.width, meta.height),
            native_fps: IMAGE_ONLY_RATE,
            frames: stream.frames,
            worker: stream.worker,
            backend: stream.backend,
            retired: Vec::new(),
            restarts: 0,
            clock: PlaybackClock::new(ClockSource::Wall),
            audio: None,
            audio_disabled: None,
            project,
            counts: vec![meta.frame_count],
            probes: std::collections::HashMap::new(),
            // Source 0 *is* the timeline's rate: it defined it.
            rates: vec![Rate::REAL_TIME],
            span_rate: Rate::REAL_TIME,
            span,
            span_priming: true,
            eos: false,
            mix: None,
            priming: false,
            drop_late: false,
            // A file just opened is a project nobody has picked stand-ins for.
            proxies: false,
            auto_proxies: true,
            encoder_seat: crate::export::EncoderSeat::default(),
            sample_rate: None,
        })
    }

    /// Opens a project file written by [`save_project`](Self::save_project):
    /// the whole timeline, restored to where its playhead stood, paused like
    /// [`open`](Self::open).
    ///
    /// A *new* session rather than a reload of this one, so a load that fails
    /// leaves the caller's current session untouched -- atomic by
    /// construction. Every named file is opened and checked here: the
    /// *scaffolding* source -- the first that is not a still, since a picture
    /// carries no rate -- defines the timeline exactly as `open` does, every
    /// other source has to match it the way [`import`](Self::import) demands
    /// (a save renumbers, so which index that is says nothing), and every clip
    /// has to still be inside the file it plays from. A source that vanished or
    /// shrank since the save is a refusal naming it, not a silently shorter
    /// timeline -- the disappearing-file tolerance elsewhere in this type
    /// applies only *after* a project has loaded.
    ///
    /// The undo history is not saved: `undo` is `false` on a fresh load.
    pub fn open_project(path: &Path) -> crate::Result<Self> {
        let doc = crate::edith::load(path)?;
        // *Which* source scaffolds it: the first that is not a still, and index
        // 0 only when there is no other. A still defines no frame rate -- it is
        // given [`IMAGE_ONLY_RATE`] -- so scaffolding a timeline of 24 fps
        // footage off a PNG that a save happened to renumber to index 0
        // (`Project::without_orphan_sources` numbers by first use, and a
        // library removal moves indexes too) reads the timeline as 30 fps and
        // then refuses every file on it for not matching. The rule this and
        // [`audio_source_of`] share: nothing may assume what source 0 *is*.
        //
        // A v9 file says what rate it was cut at, and that answer beats the
        // scaffold's *where the scaffold has none to give*: a still and a song
        // are both given a made-up rate (`IMAGE_ONLY_RATE`,
        // `AUDIO_ONLY_CANVAS`), so a timeline of nothing but those used to come
        // back at 30 fps however it was cut. A scaffold with a picture keeps
        // defining the timeline as it always did -- it is the file every clip
        // on the lane was conformed to, and the two agree by construction.
        let saved_fps = doc.fps.filter(|f| f.is_finite() && *f > 0.0);
        let scaffold = doc
            .sources
            .iter()
            .position(|s| !crate::is_image(&s.path))
            .unwrap_or(0);
        // Owned: the sources move into the `Project` below, and the device is
        // opened after that (see the comment there).
        let first = doc
            .sources
            .get(scaffold)
            .cloned()
            .ok_or("the project names no sources")?;
        // The scaffolding source both defines the timeline and is what the
        // playhead's first picture comes from, so it is opened for playback
        // rather than merely probed.
        // One value, not two locals: everything below this line can refuse, and
        // locals drop in reverse declaration order -- a bare worker would join
        // its decode thread while the receiver next to it was still holding
        // that thread parked in `send`, which is a hang (see [`FrameStream`]).
        // Ungraded, and superseded before a frame of it is shown: the `seek` at
        // the end of this function reopens the playhead's span through
        // `start_span`, which is where a saved grade reaches the picture.
        // ...unless it has no picture to open: a project whose source 0 is a
        // song scaffolds the canvas the same way a fresh audio-only open does
        // ([`open_audio_only`](Self::open_audio_only)), and the placeholder
        // black stream is superseded by the `seek` at the end of this function
        // like every other worker opened here.
        let (mut meta, stream) = match crate::is_audio(&first.path) {
            true => {
                let (width, height, canvas_fps) = AUDIO_ONLY_CANVAS;
                let frame_rate = saved_fps.unwrap_or(canvas_fps);
                let meta = VideoMeta {
                    width,
                    height,
                    frame_rate,
                    frame_count: audio_frames(&first.path, frame_rate)
                        .map_err(|e| format!("source {}: {e}", first.path.display()))?,
                    codec: Codec::H264,
                    color: ColorDescription::default(),
                };
                (meta, DecodeSession::open_black(width, height, 1))
            }
            // ...and a still scaffolds its own picture as the canvas, exactly
            // as a fresh `open_image_only` does, with the same placeholder
            // stream the `seek` at the end supersedes.
            false if crate::is_image(&first.path) => {
                let still = crate::decode::Still::open(&first.path)
                    .map_err(|e| format!("source {}: {e}", first.path.display()))?;
                let frame_rate = saved_fps.unwrap_or(IMAGE_ONLY_RATE);
                let meta = VideoMeta {
                    width: still.width,
                    height: still.height,
                    frame_rate,
                    frame_count: image_frames(frame_rate),
                    codec: Codec::H264,
                    color: ColorDescription::default(),
                };
                let stream = DecodeSession::open_still(
                    &first.path,
                    0,
                    1,
                    ColorParams::default(),
                    Composer::passthrough(),
                )?;
                (meta, stream)
            }
            false => DecodeSession::open_worker(
                &first.path,
                0,
                u32::MAX,
                ColorParams::default(),
                Composer::passthrough(),
                // The saved rendition, from the document rather than from the
                // project (which is built below): this placeholder is what the
                // window shows until the `seek` at the end reopens the span.
                doc.tone,
            )
            .map_err(|e| format!("source {}: {e}", first.path.display()))?,
        };
        // The project's own resolution, which is source 0's picture unless the
        // file says otherwise -- every dialect before v7 had no way to say it,
        // and that default is exactly what those projects meant.
        let native = (meta.width, meta.height);
        if let Some((width, height)) = doc.resolution {
            meta.width = width;
            meta.height = height;
        }
        // ...and the project's own *rate*, which is the scaffold's unless the
        // file says otherwise. The two made-up rates above already took the
        // saved one (their counts are computed from it); a scaffold with a
        // picture keeps its own here, so a project cut at a rate nobody picked
        // is the file it always was -- and one where a rate *was* picked
        // ([`set_frame_rate`](Self::set_frame_rate)) comes back at it rather
        // than at the media's, which would leave every clip number counted in
        // frames the timeline no longer has.
        let native_fps = match crate::is_audio(&first.path) {
            true => AUDIO_ONLY_CANVAS.2,
            false if crate::is_image(&first.path) => IMAGE_ONLY_RATE,
            false => meta.frame_rate,
        };
        if let Some(fps) = saved_fps {
            meta.frame_rate = fps;
        }
        // A song and a still have no rate of their own to be conformed from:
        // their length above was counted in the timeline's frames already,
        // whatever those are. A file with pictures is read through [`Rate`] like
        // any other source when the project is cut at another rate.
        let made_up = crate::is_audio(&first.path) || crate::is_image(&first.path);
        let scaffold_rate = match made_up || native_fps == meta.frame_rate {
            true => Rate::REAL_TIME,
            false => Rate::from_fps(native_fps, meta.frame_rate)
                .map_err(|e| format!("source {}: {e}", first.path.display()))?,
        };
        let first_audio = first_audio_of(&doc.sources)?;

        // In source order, with the scaffold's own count in its own slot: the
        // clip check below indexes this list by source, so it must line up with
        // `doc.sources` whichever entry the scaffolding came from.
        let mut counts = Vec::with_capacity(doc.sources.len());
        // Beside it, one entry per source, grown at every `push` below: the
        // scaffold defines the rate, so it is read frame for frame; every other
        // one is however long it is *in this timeline's frames*, which is the
        // length `import` noted for it and the wall the clip check below holds
        // it to.
        let mut rates = Vec::with_capacity(doc.sources.len());
        for (i, source) in doc.sources.iter().enumerate() {
            if i == scaffold {
                counts.push(scaffold_rate.timeline_at(meta.frame_count));
                rates.push(scaffold_rate);
                continue;
            }
            // A still has neither a rate to match nor a track to match with:
            // its length is the one it is held to, recomputed exactly as the
            // import that first noted it did ([`image_frames`]), which is what
            // makes the clip check below meaningful on a reload.
            if crate::is_image(&source.path) {
                crate::decode::image_size(&source.path)
                    .map_err(|e| format!("source {}: {e}", source.path.display()))?;
                counts.push(image_frames(meta.frame_rate));
                rates.push(Rate::REAL_TIME);
                continue;
            }
            // A source with no picture is checked on what it does have, and
            // its length is its playing time -- the same two answers `import`
            // gave it when it first joined the timeline.
            if crate::is_audio(&source.path) {
                audio_matches(source, &first_audio)
                    .map_err(|e| format!("source {}: {e}", source.path.display()))?;
                counts.push(audio_frames(&source.path, meta.frame_rate)?);
                rates.push(Rate::REAL_TIME);
                continue;
            }
            let (other, _) = Demuxer::open(&source.path)
                .map_err(|e| format!("source {}: {e}", source.path.display()))?;
            let rate = matches_timeline(source, &other, &meta, &first_audio)
                .map_err(|e| format!("source {}: {e}", source.path.display()))?;
            counts.push(rate.timeline_at(other.frame_count));
            rates.push(rate);
        }
        // The video lane can only play files that have pictures. Hand-written
        // (or hand-edited) project files are the one door this can come in
        // through, so it is refused by name here rather than becoming a clip
        // that decodes to nothing.
        for (kind, clip) in doc
            .lanes
            .iter()
            .flat_map(|(kind, clips)| clips.iter().map(move |clip| (*kind, clip)))
        {
            let path = &doc.sources[clip.source].path;
            if kind == LaneKind::Video && crate::is_audio(path) {
                return Err(format!(
                    "{} has no picture: it can only play on an audio lane",
                    path.display()
                )
                .into());
            }
            // ...and the mirror of it: a still is silent, so a clip playing one
            // on an audio lane is a segment the audio worker would open a PNG
            // for. Refused by name here, which is the one door it can arrive
            // through (`place_stream_at` never puts one there).
            if kind == LaneKind::Audio && crate::is_image(path) {
                return Err(format!(
                    "{} is a still image: it can only play on a video lane",
                    path.display()
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
        // The subtitle files are read here rather than in the parser: a
        // `.edith` names them and holds no cues. One that has gone missing, or
        // whose track is a codec of pictures, comes back listed and refused by
        // name ([`crate::subtitle::open_all`]) -- a project does not stop
        // opening over a subtitle, and a re-save does not lose the row.
        //
        // All of them in one call, in saved order, because a file is walked
        // whole to reach one track: a film whose project names many of its
        // tracks is one walk here rather than one per row.
        let subtitles = crate::subtitle::open_all(&doc.subtitles);
        let project = Project::from_parts(doc.sources, doc.lanes, doc.eq, doc.color)?
            .with_mix(&doc.gains, doc.limiter)
            .with_tone(doc.tone)
            // The palette before what is placed on it: a caption names a row of
            // it, and [`Project::with_subs`] refuses one naming a row that is
            // not there.
            .with_subtitles(subtitles)
            .with_subs(doc.subs)?;
        let span = project.composite_span_at(0);
        // Last, because it is the one thing here that cannot be taken back: the
        // feeder thread outlives the `Audio` value (it holds its own clones) and
        // only a session's `drop` retires it, so a refusal above this line would
        // leave a PipeWire stream and a thread behind for a project that never
        // opened. Nothing before it needs the device.
        //
        // On the source the timeline's *sound* is defined by, which is the
        // scaffold unless that one is a still ([`audio_source_of`]): opening a
        // PNG as the device is a session with no audio at all, however many
        // clips with sound are on its lanes.
        let (audio, audio_disabled) = match audio_source_of(project.sources()) {
            Some(source) => open_audio(&source.path, source.audio_stream),
            None => (None, None),
        };
        let mut session = Self {
            meta,
            native,
            native_fps,
            frames: stream.frames,
            worker: stream.worker,
            backend: stream.backend,
            retired: Vec::new(),
            restarts: 0,
            clock: PlaybackClock::new(match audio {
                Some(_) => ClockSource::Audio,
                None => ClockSource::Wall,
            }),
            audio,
            audio_disabled,
            project,
            counts,
            probes: std::collections::HashMap::new(),
            rates,
            span_rate: Rate::REAL_TIME,
            span,
            span_priming: true,
            eos: false,
            mix: None,
            priming: false,
            drop_late: false,
            // What the file says, and `false` for every dialect before v12:
            // a project cut on the films themselves opens on them.
            proxies: doc.proxy,
            // ...and what it says about making them, which is "yes" for every
            // dialect before v13 and for one that leaves the line out.
            auto_proxies: doc.auto_proxy,
            // ...and which encoder it exports with, which is the seat this
            // machine has for every dialect before v14.
            encoder_seat: doc.encoder,
            sample_rate: doc.sample_rate,
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

    /// What the pictures now arriving are being decoded by: the clip under the
    /// playhead, since one video worker feeds the composite at a time. An
    /// atomic load -- a front-end may ask it every repaint -- and it is what
    /// *opened*, so a clip whose hardware session fell back to software reads
    /// [`Backend::Software`] from the frame that fallback happened.
    pub fn decode_backend(&self) -> Backend {
        self.backend.get()
    }

    /// What an export of this timeline would encode the *sound* with, before
    /// one is started: the copy an mp4 normally makes, or the software encoder
    /// the format (or an equalized lane) forces. Pure -- no probe, no file
    /// opened -- so a card may ask it per repaint; the picture's half costs a
    /// plugin open and lives in [`crate::export::planned_video`].
    pub fn planned_audio(&self, format: crate::export::Format, ranged: bool) -> &'static str {
        crate::export::planned_audio(&self.project, format, ranged)
    }

    /// The timeline an export started right now would be run against, owned:
    /// what lets a front-end ask [`crate::export::planned_seats`] what that
    /// export would open *off* its render thread, which is the only way to ask
    /// a question that opens files. The very snapshot
    /// [`export_to_with`](Self::export_to_with) hands the worker, so the answer
    /// is about the export that would really start.
    pub fn export_snapshot(&self) -> (crate::project::Project, VideoMeta) {
        (self.project.export_snapshot(), self.meta)
    }

    /// What an export would do about the tracks at `picks` --
    /// [`crate::export::planned_subtitles`], asked of the project a front-end is
    /// holding, and pure for the same reason. `picks` is any run of rows of
    /// [`subtitles`](Self::subtitles): the whole
    /// [`ExportSettings::subtitles`](crate::export::ExportSettings::subtitles)
    /// list a card is about to send (`picks.iter().copied()`) as much as a
    /// single `Some(row)`.
    pub fn planned_subtitles(
        &self,
        format: crate::export::Format,
        picks: impl IntoIterator<Item = usize>,
    ) -> String {
        crate::export::planned_subtitles(&self.project, format, picks)
    }

    /// Where the cues of the track at `pick` land on *this* timeline --
    /// [`crate::export::timeline_cues`], the very map an export writes the file
    /// with, so a front-end that draws these draws what the file will say. On a
    /// rippled or cut timeline that is the whole point: an embedded track rides
    /// the spans that still play it, and a standalone `.srt` keeps the
    /// timeline's own clock, clipped to its length.
    ///
    /// Empty for a row that is not there and for a track that could not be read.
    /// Pure and file-free, so it may be asked per repaint; the cost is a walk of
    /// the spans and of the cues.
    pub fn timeline_cues(&self, pick: usize) -> Vec<crate::subtitle::Cue> {
        self.project
            .subtitles()
            .get(pick)
            .map_or_else(Vec::new, |t| {
                crate::export::timeline_cues(&self.project, t, self.meta.frame_rate)
            })
    }

    /// Why the sound's rate is not a choice for this timeline in this format --
    /// [`crate::export::audio_rate_refusal`], asked of the project a front-end
    /// is holding, and pure for the same reason.
    pub fn audio_rate_refusal(
        &self,
        format: crate::export::Format,
        ranged: bool,
    ) -> Option<&'static str> {
        crate::export::audio_rate_refusal(&self.project, format, ranged)
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

    /// The subtitle tracks on this timeline, in the order they were added: what
    /// a front-end lists and what [`save_project`](Self::save_project) writes.
    /// A track that could not be read is *in* this list, saying why
    /// ([`crate::subtitle::SubtitleTrack::refused`]) -- listed and skipped, not
    /// dropped.
    pub fn subtitles(&self) -> &[crate::subtitle::SubtitleTrack] {
        self.project.subtitles()
    }

    /// Adds subtitles from `path`: every subtitle track of a Matroska file, or
    /// the one track a standalone `.srt`/`.vtt`/`.ass` is. Hands back how many
    /// were added -- 0 for a file whose tracks are all on the timeline already,
    /// since importing the same file twice is the same subtitles.
    ///
    /// An error only when the file cannot be read *at all*; a track this cannot
    /// parse is added refused by name, for the reason
    /// [`crate::subtitle::open`] gives. Nothing else about the timeline moves:
    /// subtitles are not clips and land on no lane.
    pub fn import_subtitles(&mut self, path: &Path) -> crate::Result<usize> {
        let tracks = Self::parse_subtitles(path)?;
        // A file that was walked and carries none is refused as *that*, because
        // the alternative -- 0 added -- is the very sentence a file whose tracks
        // are already on this timeline gets, and the two are opposite answers a
        // person acts differently on. Said only after the walk actually looked
        // ([`crate::subtitle::of_media`]); a *file* that is not a container at
        // all never reaches here, [`parse_subtitles`](Self::parse_subtitles)
        // refuses it in its own words.
        if tracks.is_empty() {
            return Err(Self::no_subtitles_in(path));
        }
        Ok(self.add_subtitle_tracks(tracks))
    }

    /// The reading half of [`import_subtitles`](Self::import_subtitles), with no
    /// session in it: every subtitle track of a Matroska file, or the one track
    /// a standalone `.srt`/`.vtt`/`.ass` is, cues and all. Same refusal rule.
    ///
    /// This is the half that costs -- a walk of the whole container, measured at
    /// 210 ms on a 25 GB 35-track file and 1.3 s on a 3 GB one whose pages were
    /// cold -- so a front-end runs *this* on its background executor and hands
    /// what comes back to [`add_subtitle_tracks`](Self::add_subtitle_tracks),
    /// which costs a push. Both doors are this one function, so the import
    /// button and the background one cannot drift apart.
    pub fn parse_subtitles(path: &Path) -> crate::Result<Vec<crate::subtitle::SubtitleTrack>> {
        // The containers that carry subtitle tracks *inside* them: every
        // Matroska one ([`crate::demux::is_matroska`], which is where `.mks` --
        // the subtitles alone -- comes in) and the mp4 family, whose `tx3g`
        // timed text is what an mp4 export of this project's own writes, so a
        // file edith wrote is a file edith imports back.
        let tracks = match crate::demux::is_matroska(path)
            || path.extension().is_some_and(|e| {
                matches!(
                    e.to_string_lossy().to_ascii_lowercase().as_str(),
                    "mp4" | "m4v" | "mov"
                )
            }) {
            true => crate::subtitle::of_media(path)?,
            false => vec![crate::subtitle::open(path, None)],
        };
        // A standalone file that could not be parsed is a refusal here rather
        // than a row nobody asked for: the import is the moment to say so, and
        // the load is the moment to keep it.
        if let [one] = &tracks[..]
            && one.track.is_none()
            && let Some(why) = &one.refused
        {
            return Err(why.clone().into());
        }
        Ok(tracks)
    }

    /// What a file the walk found no subtitle track in is refused with,
    /// wherever the refusal is worded: the engine's own door
    /// ([`import_subtitles`](Self::import_subtitles)) and a front-end that
    /// splits the walk from the push ([`parse_subtitles`](Self::parse_subtitles)
    /// then [`add_subtitle_tracks`](Self::add_subtitle_tracks)) must not tell a
    /// person two different stories about the same file.
    pub fn no_subtitles_in(path: &Path) -> crate::Error {
        format!("no subtitle tracks in {}", path.display()).into()
    }

    /// Puts tracks already read by [`parse_subtitles`](Self::parse_subtitles)
    /// on this timeline, in the order given. Hands back how many actually went
    /// on: [`Project::add_subtitles`] drops the ones already here -- same file,
    /// same track number -- so 0 is "this file's subtitles are on the timeline
    /// already", which is the sentence a front-end says.
    ///
    /// The whole of what an import costs the thread that calls it, which is why
    /// the parse is a separate door: nothing here opens, seeks or decodes.
    /// Subtitles are not clips and land on no lane, so nothing playable changes
    /// and no undo step is pushed ([`Project::remove_subtitles`] says why).
    pub fn add_subtitle_tracks(&mut self, tracks: Vec<crate::subtitle::SubtitleTrack>) -> usize {
        tracks
            .into_iter()
            .filter(|t| self.project.add_subtitles(t))
            .count()
    }

    /// Takes the track at `idx` off this timeline -- the door a subtitle row's
    /// own remove goes through, the way [`import_subtitles`](
    /// Self::import_subtitles) is the door that puts one on. Refused in
    /// [`Project::remove_subtitles`]'s words for a row this timeline does not
    /// have, and for one a caption on a subtitle lane still plays -- delete
    /// those placements first, or the words under them would change.
    ///
    /// Nothing playable changes and no worker was opened against a cue, so
    /// unlike [`remove_source`](Self::remove_source) this does not reseek. Rows
    /// past `idx` move down by one: a caller holding a picked row (the export's
    /// subtitle pick) has to fix it up or drop it; the placements on the lanes
    /// are walked down with it and need no fixing. Not an undo step, for the
    /// reason [`Project::remove_subtitles`] gives -- the way back is
    /// [`import_subtitles`](Self::import_subtitles), which reads a file's
    /// subtitles and touches nothing else on the timeline -- and it *empties*
    /// the undo history, because the steps in it name the tracks by the indexes
    /// this call just changed.
    pub fn remove_subtitles(&mut self, idx: usize) -> crate::Result<()> {
        self.project.remove_subtitles(idx)
    }

    /// Writes the timeline to `path` as a `.edith`, atomically (see
    /// [`crate::edith`]). Sources no clip plays from are left out, and the
    /// playhead is saved with it so a reopened project resumes where it stood.
    ///
    /// Refused, writing nothing, for a session whose library is empty (every
    /// row removed, [`Project::remove_source`]): a file naming no source has no
    /// timeline in it and [`open_project`](Self::open_project) could only
    /// refuse it, so the refusal belongs here, where nothing has been
    /// overwritten yet.
    pub fn save_project(&self, path: &Path) -> crate::Result<()> {
        if self.project.sources().is_empty() {
            return Err("this project names no file: there is nothing to save".into());
        }
        let (sources, lanes, eq, color) = self.project.without_orphan_sources();
        let playhead = secs_to_frame(self.now(), self.meta.frame_rate)
            .min(self.project.timeline_frames().saturating_sub(1));
        crate::edith::save(
            path,
            &sources,
            &lanes,
            // The lane volumes and the limiter with them: a mix that vanished
            // on a reload would be a mix nobody could keep, and both are the
            // project's, not this machine's (the monitoring volume is not
            // written and never will be).
            &self.project.lane_gains(),
            // ...and what is placed on each lane, in the same order again: the
            // captions, which are the only thing a subtitle lane holds.
            &self.project.lane_subs(),
            // The subtitle tracks with them, by reference: which file the cues
            // are in, never the cues (see [`crate::edith`]).
            self.project.subtitles(),
            &eq,
            &color,
            (self.meta.width, self.meta.height),
            // ...and the rate it was cut at, which nothing else in the file
            // says: a timeline of stills and songs used to come back at
            // whatever its scaffold implied (`open_project`).
            Some(self.meta.frame_rate),
            // ...and which rendition its HDR media is watched in, for the same
            // reason: a picked look that vanished on a reload is a look nobody
            // could keep.
            self.project.tone(),
            // ...and whether it is cut on the stand-ins, for the same reason.
            // *Which* files have one is not saved: [`crate::proxy`] finds that
            // out from the film itself, so nothing here can go stale.
            self.proxies,
            // ...and whether it makes them by itself, which is a different
            // question from whether it cuts on them and the only one an import
            // asks ([`Self::auto_proxies`]).
            self.auto_proxies,
            // ...and which encoder an export of it opens, for the same reason:
            // a project delivered on the software encoder is delivered on it
            // again after a reload.
            self.encoder_seat,
            self.project.limiter(),
            // ...and the rate the mix was picked to run at, for the same
            // reason: a project delivered at a chosen rate is delivered at it
            // again after a reload.
            self.sample_rate,
            playhead,
        )
    }

    /// The next decoded frame, its `index` rewritten from a source frame to a
    /// *timeline* frame -- the only frame space that leaves the engine, and the
    /// one [`PlaybackSession::now`] is in.
    ///
    /// `None` means "nothing right now": the decoder is behind, or a clip
    /// boundary is being reopened -- and *that* now happens entirely on the new
    /// worker ([`DecodeSession::open_worker_deferred`]), so it costs this
    /// thread a thread spawn and the caller simply keeps showing its last frame
    /// for however long the source takes to open (hundreds of milliseconds on a
    /// big film, seconds off a cold cache). End of stream is
    /// [`PlaybackSession::is_eos`].
    pub fn try_frame(&mut self) -> Option<Frame> {
        self.publish_playhead();
        loop {
            match self.frames.try_recv() {
                Ok(mut frame) => {
                    // A gap's worker indexes from zero, a decoder's from its in
                    // point, and the span itself knows which: [`Span::timeline_at`]
                    // is the *stamp*, and it is the ceil half of the pair whose
                    // floor half an export encodes with -- so the frame the pump
                    // is holding at any timeline frame is the very frame the
                    // export writes there, at every rate
                    // (`speed_maps_both_ways`). An empty timeline has no span,
                    // and its one black frame is frame 0. Real time is the
                    // arithmetic this always did, frame for frame.
                    // ...through the file's own rate first, which is the ceil
                    // half of *that* pair: the picture at source frame `i` is
                    // due at the first timeline-rate frame that shows it, and a
                    // slower file's picture is therefore held for the frames in
                    // between. Real time for a gap and for a single-rate
                    // project, where this is the identity.
                    frame.index = self.span.map_or(frame.index, |s| {
                        s.timeline_at(self.span_rate.timeline_at(frame.index))
                    });
                    // The first picture out of the span now decoding ends its
                    // prime ([`Self::span_priming`]).
                    self.span_priming = false;
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

    /// Whether the timeline has been played out: the picture worker ran to the
    /// end and there was no next clip. It is exactly that and nothing more,
    /// which is what decides who clears it -- **starting a picture does**
    /// (`start_picture`), so a seek clears it and so does every edit that
    /// rebuilds the picture (`Dirty::Picture`, `Dirty::Both` -- a paste or a
    /// placement past the end revives the session, and it has a new picture to
    /// show for it).
    ///
    /// A **sound-only** edit does not (`Dirty::Sound`: an equalizer, a fader):
    /// it rebuilds the sound where the playhead stands and starts no picture
    /// worker, so the last frame is still the last frame and this is still the
    /// end. A front-end's `Ended` therefore survives an EQ tweak made after the
    /// timeline ran out -- the parameters are on the project either way, and the
    /// next press restarts from the top with them, which is what `Ended` means.
    pub fn is_eos(&self) -> bool {
        self.eos
    }

    /// Tells the decode worker which frame of its *file* the clock has reached,
    /// so the pictures it is holding that are already behind the playhead are
    /// dropped where they are cheap to drop -- before the conversion and before
    /// the queue -- instead of being converted, queued and thrown away by the
    /// caller one repaint later. The whole of the late-frame policy: a decoder
    /// that has fallen behind catches up by not painting what nobody can see,
    /// rather than by being restarted every two seconds (`Player::pump`).
    ///
    /// The stamp is the exact inverse of the one [`try_frame`](Self::try_frame)
    /// puts on a decoded frame: timeline frame -> the clip's own frames
    /// ([`Span::speed`]) -> the file's ([`Rate`]). A gap and the emptied
    /// timeline have no file to be inside of, and publish nothing.
    fn publish_playhead(&self) {
        if !self.drop_late {
            return;
        }
        let Some(span) = self.span else { return };
        let Some((_, in_frame)) = span.from else {
            return;
        };
        let now = secs_to_frame(self.clock.now(), self.meta.frame_rate);
        let offset = now.saturating_sub(span.start).min(span.len);
        self.worker
            .playhead(self.span_rate.source_at(in_frame + span.speed.source_at(offset)));
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
        // A *replacement*, which is now the narrow case: a seek onto the file
        // the worker already has open steers it instead ([`Worker::reseek`])
        // and never comes here, so this count is the picture restarts that
        // really cost a thread and a decoder open -- which is what makes it
        // worth counting.
        self.restarts += 1;
        let mut outgoing = std::mem::replace(&mut self.worker, replacement);
        // Terminal, unlike the [`Worker::abandon`] a seek does: this worker is
        // being replaced rather than steered, so its thread has to end -- and
        // the cancel is what drops its command channel, without which a worker
        // parked between spans would sit in `recv` forever, never reaped here
        // and never joined in any bounded time at exit.
        outgoing.cancel();
        self.retired.push(outgoing);
        self.retired.retain(|w| !w.is_finished());
    }

    /// Picture restarts so far; see [`restarts`](Self::restarts). The one place
    /// a worker is ever replaced counts them, so a seek, a resync and a clip
    /// boundary all land here and a measurement need not guess which it saw.
    #[doc(hidden)]
    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    /// Whether the picture span now decoding has yet to hand over a frame:
    /// true from the moment a span is started -- a seek, a clip boundary, a
    /// picture restart -- until that span's first frame arrives. Whatever
    /// lateness that first frame carries is the span's own reopen, not a
    /// decoder falling behind, so a front-end gating its late-picture restart
    /// on this answers a prime with the decode already on its way rather than
    /// with another restart of it.
    pub fn picture_priming(&self) -> bool {
        self.span_priming
    }

    /// Starved audio callbacks since the device opened, and how far into the
    /// device's own playback (seconds) the last of them was -- `None` for a
    /// session with no device at all. The counter the plugin prints on drop,
    /// readable while the session runs: read it either side of a seek and the
    /// difference is what that seek cost the ear.
    #[doc(hidden)]
    pub fn audio_underruns(&self) -> Option<(u64, Option<f64>)> {
        let audio = self.audio.as_ref()?;
        let (count, last) = audio.ao.lock().unwrap().underruns();
        Some((count, last.map(|p| p as f64 / f64::from(audio.sample_rate))))
    }

    /// How many threads this session still has running of its own: the decode
    /// worker, every retired one that has not returned yet, and the audio
    /// feeders still in the air. One for a session playing along quietly.
    ///
    /// The oracle a seek storm is judged by. `/proc/self/status` counts the
    /// *process*, and a suite decoding twenty files at once moves that number
    /// under a test that never touched them -- this counts only what the
    /// session in hand owns, so it says the same thing alone and in a crowd.
    #[doc(hidden)]
    pub fn live_workers(&self) -> usize {
        usize::from(!self.worker.is_finished())
            + self.retired.iter().filter(|w| !w.is_finished()).count()
            + self.audio.as_ref().map_or(0, Audio::live_feeders)
    }

    /// Starts feeding whatever follows the current span on the timeline --
    /// another clip, or a gap, which is black frames from a worker that opens no
    /// file. `false` past the end. The next timeline frame is derived rather
    /// than remembered as a clip index, because a `split` while playing cuts the
    /// clip under the running worker and only the mapping stays true.
    fn next_clip(&mut self) -> bool {
        // No span is the emptied timeline: it was played out the moment its one
        // black frame went by, and there is no "next" to walk to.
        let next = match self.span {
            Some(span) => span.end(),
            None => return false,
        };
        let Some(span) = self.project.composite_span_at(next) else {
            return false;
        };
        // We only get here on a disconnect, so the old span is already done;
        // abandon anyway, so `start_span` treats every path alike -- and the
        // *worker* is kept, because the next clip is very often the same file
        // (a split cuts one clip in two) and it already has it open.
        self.worker.abandon();
        self.start_span(Some(span));
        true
    }

    /// Points the video worker at `span`: a decoder over its source range, or a
    /// black-frame generator for a gap -- and for `None`, the emptied timeline,
    /// which is one frame of black so the viewer shows the nothing that is
    /// there rather than the last picture of the clip that was deleted. The old
    /// worker must already have been cancelled -- this is the half both `seek`
    /// and `next_clip` share.
    ///
    /// A source that will not open leaves the *span* installed anyway: the
    /// timeline still moves, there are simply no more pictures, and the
    /// disconnected receiver carries the session on to the next span. For a
    /// video that is now the worker's own doing -- it opens the file, so it is
    /// the one that finds out -- and nothing on this thread waits to hear it.
    fn start_span(&mut self, span: Option<Span>) {
        // Which file's frames the worker about to be opened will number its
        // pictures in: the span's own source, and real time for a gap, whose
        // black worker counts timeline frames already. Set before the open,
        // because the open is the first thing that converts with it.
        self.span_rate = match span.and_then(|s| s.from) {
            Some((source, _)) => self.rates.get(source).copied().unwrap_or(Rate::REAL_TIME),
            None => Rate::REAL_TIME,
        };
        // The new span owes its first picture from the moment it is asked for:
        // every path below, the reused worker as much as a fresh one
        // ([`Self::span_priming`]).
        self.span_priming = true;
        let opened = match span {
            // The grade is the composite's at this frame -- the same clip the
            // span itself came from -- and it is constant across the span, so
            // the worker carries it and every frame it converts wears it.
            Some(Span {
                start,
                from: Some((source, in_frame)),
                ..
            }) if crate::is_image(&self.project.sources()[source].path) => {
                // A still: the same grade and the same canvas as any other
                // clip, over a picture decoded once and repeated for the span.
                // As many pictures as the span reads *source* frames, so a still
                // is numbered like a decoder's output and a speed reaches it
                // through the same rewrite (`try_frame`) as any other clip.
                DecodeSession::open_still(
                    &self.project.sources()[source].path,
                    in_frame,
                    span.expect("matched above").source_len(),
                    self.project
                        .composite_color_at(start)
                        .copied()
                        .unwrap_or_default(),
                    Composer::new(
                        self.meta.width,
                        self.meta.height,
                        self.project.composite_fit_at(start),
                    ),
                )
                .inspect_err(|e| eprintln!("timeline frame {start}: image open failed: {e}"))
            }
            Some(Span {
                start,
                from: Some((source, in_frame)),
                speed,
                ..
            }) => {
                // The stand-in where there is one and the switch is on, the
                // film itself otherwise -- the one place the picture's file is
                // decided ([`Self::picture_path`]). The sound is opened
                // elsewhere and is always the film's.
                //
                // It decides for the reuse door below as well, and that is what
                // makes the switch flippable while watching: a worker holding
                // the film is asked for a span of the stand-in, declines on the
                // path it already has ([`Worker::reseek`]), and the seek
                // [`Self::set_proxies`] does becomes a respawn on the other
                // file. Asking the *project* here instead would have left every
                // proxied span opening a worker from scratch.
                let path = self.picture_path(source);
                // The file's own frames, which is the only place they exist: a
                // clip counts the timeline's ([`Rate`]).
                let start_frame = self.span_rate.source_at(in_frame);
                // Source frames, which a speed makes more (or fewer) than the
                // timeline frames the span covers -- and which a file shot at
                // another rate makes fewer (or more) again. One past the last
                // frame this span shows, not one past its length: at another
                // rate those are two different frames.
                let end_frame = self.span_rate.source_at(
                    in_frame + span.expect("matched above").source_len().saturating_sub(1),
                ) + 1;
                let color = self
                    .project
                    .composite_color_at(start)
                    .copied()
                    .unwrap_or_default();
                // ...and the canvas it is placed on: the project's resolution
                // and this clip's own fit policy, constant across the span for
                // the reason the grade is. Built where it is handed over rather
                // than kept in a local, because a `Composer` owns the scratch
                // buffers it places through -- the value is not copied about.
                let fit = self.project.composite_fit_at(start);
                let canvas = || Composer::new(self.meta.width, self.meta.height, fit);
                // ...and the rendition an HDR source among them is mapped to,
                // the project's own and constant across the span for that
                // reason too.
                let tone = self.project.tone();
                // At faster than real time several source frames decode to the
                // same timeline frame ([`Speed::timeline_at`]) and only the last
                // of a run is ever shown; the worker skips converting the rest
                // ([`crate::decode::skip_for_speed`]) rather than paying a full
                // colour convert and scale for a picture nothing displays.
                //
                // corner-cut: correct only when this file plays at the
                // timeline's own rate -- `skip_for_speed`'s own math is in the
                // file's frame numbers, which a `Rate` conversion would make a
                // different sequence than the timeline's. `Speed::NORMAL` (never
                // skip) covers that rarer case exactly as every frame was
                // delivered before this.
                let decode_speed = if self.span_rate.is_real_time() {
                    speed
                } else {
                    Speed::NORMAL
                };
                // The worker already decoding this very file takes the new span
                // itself: no thread, no container parse, no VA-API init -- the
                // whole of what a seek used to cost. It answers `None` for any
                // other file (and for a worker whose thread has ended), and the
                // deferred opener below is then exactly the path it always was.
                if let Some((frames, backend)) = self.worker.reseek(
                    &path,
                    start_frame,
                    end_frame,
                    color,
                    canvas(),
                    tone,
                    decode_speed,
                ) {
                    self.frames = frames;
                    self.backend = backend;
                    self.span = span;
                    return;
                }
                Ok(DecodeSession::open_worker_deferred(
                    &path,
                    start_frame,
                    end_frame,
                    color,
                    canvas(),
                    tone,
                    decode_speed,
                ))
            }
            // A gap: black for as long as it runs. An emptied timeline has no
            // span at all and gets one frame of it -- enough to put black on
            // screen, and it ends where the timeline does, at once.
            gap => Ok(DecodeSession::open_black(
                self.meta.width,
                self.meta.height,
                gap.map_or(1, |s| s.len),
            )),
        };
        let Ok(stream) = opened else {
            // The file would not open. The span still goes in -- the timeline
            // moves on, there are simply no more pictures -- but the *frames*
            // must not: the old channel's pictures are numbered in the old
            // source's frames, and presenting them under the new mapping would
            // put the wrong picture at the wrong time (and leave
            // [`decode_backend`](Self::decode_backend) describing a worker that
            // is no longer feeding anything). A dead receiver instead, which is
            // what carries the session on to the next span. The old worker stays
            // parked in `self.worker`, where the caller has already abandoned
            // the span it was decoding ([`Worker::abandon`]) without ending the
            // thread; the next span reseeks it or retires it.
            self.frames = std::sync::mpsc::channel().1;
            self.backend = BackendCell::new(Backend::Gap);
            self.span = span;
            return;
        };
        // Receiver first, worker second: the drop of the *old* receiver is
        // what wakes the outgoing worker if it is parked in `send`, and only
        // then can it return and be reaped by a later sweep. Nothing here
        // joins, so this ordering costs the seek nothing.
        self.frames = stream.frames;
        self.backend = stream.backend;
        self.retire(stream.worker);
        self.span = span;
    }

    /// Length of the edited timeline in seconds -- what a ruler shows, and it
    /// shrinks with every delete.
    pub fn timeline_duration(&self) -> f64 {
        f64::from(self.project.timeline_frames()) / self.meta.frame_rate
    }

    /// Whether no lane holds anything: the emptied timeline, which plays black
    /// and silent and is zero seconds long. A state, not a failure -- but the
    /// one a caller with nothing to render (an export) has to refuse by name
    /// rather than write a file of no frames.
    pub fn is_empty(&self) -> bool {
        self.project.timeline_frames() == 0
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

    /// Drops that empty lane again, in [`Project::remove_lane`]'s words when it
    /// refuses. One undo step, and no reseek for the same reason the add needs
    /// none: an empty lane was playing nothing.
    pub fn remove_lane(&mut self, lane: Lane) -> crate::Result<()> {
        self.project.remove_lane(lane)
    }

    /// Moves a whole track to display position `to`, clips and all
    /// ([`Project::move_lane`]), and hands back the handle it answers to
    /// afterwards -- `None` when nothing moved. One undo step.
    ///
    /// A *video* track that moves rebuilds the picture where the playhead
    /// stands, because display order is the stack and the frame on screen may
    /// be another lane's from here on; the sound plays on through it, untouched
    /// -- a mix is a sum, and a lane's gain travelled with it.
    pub fn move_lane(&mut self, lane: Lane, to: usize) -> Option<Lane> {
        let moved = self.project.move_lane(lane, to)?;
        if lane.kind == LaneKind::Video {
            self.invalidate(Dirty::Picture);
        }
        Some(moved)
    }

    /// The subtitle lanes in display order -- `S1..Sn`, the rows a front-end
    /// lays out beside the picture and sound ones
    /// ([`Project::subtitle_lanes`]). Empty until something adds one, which is
    /// [`add_lane`](Self::add_lane) with [`LaneKind::Subtitle`], and they come
    /// off, move and reorder through the very same doors every other lane does.
    pub fn subtitle_lanes(&self) -> Vec<Lane> {
        self.project.subtitle_lanes()
    }

    /// What is placed on one subtitle lane, in timeline order: what a front-end
    /// draws as clips, each naming the palette row it plays
    /// ([`Self::subtitles`]) and the window of it it keeps
    /// ([`Project::sub_lane`]).
    pub fn sub_lane(&self, lane: Lane) -> &[SubClip] {
        self.project.sub_lane(lane)
    }

    /// Places a stretch of a subtitle track on `lane` at timeline frame `at`
    /// ([`Project::place_sub`], whose words a refusal is shown in). One undo
    /// step.
    ///
    /// No reseek and no invalidation, which is what the whole wrapper layer is
    /// here for: cues are drawn from [`sub_lane_cues`](Self::sub_lane_cues)
    /// over whatever picture is on screen, so nothing already decoded stops
    /// being right.
    pub fn place_sub(&mut self, lane: Lane, at: u32, sub: SubClip) -> crate::Result<()> {
        self.project.place_sub(lane, at, sub)
    }

    /// Drags a placement to another subtitle lane or another frame
    /// ([`Project::move_sub`]). One undo step, and `Ok` with none for a drop
    /// that changed nothing. A caption in a group drags the group's clips with
    /// it -- a mapping change -- so this reseeks like a clip's drag does; a
    /// caption in no group moves nothing that plays and disturbs nothing.
    pub fn move_sub(&mut self, from: Lane, idx: usize, to: Lane, start: u32) -> crate::Result<()> {
        let grouped = self.caption_grouped_with_clips(from, idx);
        let moved = self.project.move_sub(from, idx, to, start);
        if moved.is_ok() && grouped {
            self.invalidate(Dirty::Both);
        }
        moved
    }

    /// Drags one edge of a placement to timeline frame `to`
    /// ([`Project::trim_sub`]), **at this timeline's own frame rate** -- a
    /// placement's position is in frames while its words are in microseconds,
    /// and the rate that joins them is the one thing a [`Project`] does not
    /// know. That is why a front-end trims through this door and never converts
    /// a cue's microseconds itself. One undo step. A caption in a group trims
    /// the group's clips with it, and that half reseeked; a lone caption's trim
    /// changes only the words, which are drawn over whatever is decoded.
    pub fn trim_sub(&mut self, lane: Lane, idx: usize, edge: Edge, to: u32) -> crate::Result<()> {
        let grouped = self.caption_grouped_with_clips(lane, idx);
        let trimmed = self.project.trim_sub(lane, idx, edge, to, self.meta.frame_rate);
        if trimmed.is_ok() && grouped {
            self.invalidate(Dirty::Both);
        }
        trimmed
    }

    /// How far that edge may travel, `(first, last)` timeline frame inclusive
    /// ([`Project::trim_sub_room`]) -- what a front-end drawing the box during
    /// a drag asks, at this timeline's rate.
    pub fn trim_sub_room(&self, lane: Lane, idx: usize, edge: Edge) -> Option<(u32, u32)> {
        self.project.trim_sub_room(lane, idx, edge, self.meta.frame_rate)
    }

    /// Takes one placement off a subtitle lane, leaving a gap
    /// ([`Project::lift_sub`]). One undo step; the palette row it played stays
    /// on this timeline's list.
    pub fn lift_sub(&mut self, lane: Lane, idx: usize) -> bool {
        self.project.lift_sub(lane, idx)
    }

    /// What that subtitle lane shows on *this* timeline, in start order
    /// ([`Project::sub_lane_cues`] at this timeline's rate) -- the placed-clip
    /// counterpart of [`timeline_cues`](Self::timeline_cues), which carries a
    /// whole palette track through the spans that play its media. Pure, so a
    /// front-end may ask it per repaint.
    pub fn sub_lane_cues(&self, lane: Lane) -> Vec<crate::subtitle::Cue> {
        self.project.sub_lane_cues(lane, self.meta.frame_rate)
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

    /// Takes that clip out of its group, so its picture and its sound are edited
    /// apart from here on. Metadata only like a cut -- no reseek -- and one undo
    /// step. `false` for a bad index and for a clip that is not grouped with
    /// anything, which is already detached.
    pub fn ungroup(&mut self, lane: Lane, idx: usize) -> bool {
        self.project.ungroup(lane, idx)
    }

    /// Puts two clips covering the same frames back into one group: the regroup
    /// of what [`ungroup`](Self::ungroup) took apart. Metadata only, one undo
    /// step, and the error says why when it refuses.
    pub fn group(&mut self, a: Lane, a_idx: usize, b: Lane, b_idx: usize) -> crate::Result<()> {
        self.project.group(a, a_idx, b, b_idx)
    }

    /// Puts every placement the picks name into one group
    /// ([`Project::group_all`]) -- the ctrl-click selection's own door, which
    /// may name clips and captions alike. Metadata only like the pair above:
    /// no reseek, one undo step, and the error says why when it refuses.
    pub fn group_all(&mut self, picks: &[(Lane, usize)]) -> crate::Result<()> {
        self.project.group_all(picks)
    }

    /// [`delete_clip`](Self::delete_clip) for the caption the pick named: a
    /// caption in no group lifts exactly as [`lift_sub`](Self::lift_sub) lifts
    /// it -- no reseek, because nothing that plays has changed -- and a caption
    /// in a group takes the group with it, which ripples every member's own
    /// lane and reseeks like a delete. `false` for a bad index.
    pub fn delete_sub(&mut self, lane: Lane, idx: usize) -> bool {
        // Which of the two, asked before anything moves: the reseek a grouped
        // delete owes is the whole difference between the paths. A group of
        // captions alone (no clip on another lane) still ripples every
        // member's *own* sub lane closed (`Project::delete_sub_in`, since
        // cdc53a6), and a sub lane counts toward `timeline_frames` exactly
        // like a clip lane does -- so the timeline can shrink out from under
        // the playhead from a caption-only group too, not only a media one.
        // `caption_grouped_with_clips` answers a narrower question than the
        // one this dispatch needs; a caption carries a link at all only once
        // grouped (`place_sub` always starts one at `None`), so that alone is
        // "this delete ripples more than its own lane".
        match self.sub_lane(lane).get(idx).is_some_and(|s| s.link.is_some()) {
            true => self.edit(Dirty::Both, |p| p.delete_sub_in(lane, idx)),
            // A caption in no group is the plain lift that touches nothing
            // else: no reseek owed.
            false => self.project.delete_sub_in(lane, idx),
        }
    }

    /// Whether the caption at `idx` of `lane` is grouped with clips on other
    /// lanes: the one question every caption edit that can move media has to
    /// ask first, because the answer is whether the edit owes a reseek -- and
    /// the front-end owes the same answer, asked before the edit moves the
    /// indices, so its flag reset matches the engine's reseek exactly.
    pub fn caption_grouped_with_clips(&self, lane: Lane, idx: usize) -> bool {
        self.project
            .sub_lane(lane)
            .get(idx)
            .and_then(|s| s.link)
            .is_some_and(|id| {
                self.project
                    .lanes()
                    .into_iter()
                    .any(|l| l != lane && self.project.lane(l).iter().any(|c| c.link == Some(id)))
            })
    }

    /// What the clip at `idx` of `lane` is equalized with, or `None` for one
    /// that plays flat -- what a card shows before it lets anyone drag a band.
    pub fn eq_of(&self, lane: Lane, idx: usize) -> Option<&EqParams> {
        self.project.eq_of(lane, idx)
    }

    /// Gives that clip an equalizer, or takes it off with `None`. One undo step,
    /// like every other edit -- and it rebuilds the **sound** where the playhead
    /// is, so a band changed during playback is heard from the next chunk on
    /// rather than at the next play. The picture is not touched: the video
    /// worker keeps decoding through it ([`Dirty`]). `false` for a bad index or
    /// a non-finite band, and nothing changes.
    ///
    /// corner-cut: the audio rebuild is what makes it live, so the cost of a
    /// change is a decoder restart -- inaudible at a drag's end, but too much to
    /// call once per pointer sample (a caller commits one change per gesture).
    /// Upgrade path is the per-segment twin of [`crate::audio::MixControls`],
    /// which the lane worker would poll per chunk the way the mixer polls the
    /// gains.
    pub fn set_eq(&mut self, lane: Lane, idx: usize, params: Option<EqParams>) -> bool {
        self.edit(Dirty::Sound, |p| p.set_eq(lane, idx, params))
    }

    /// How loud that lane plays, in dB -- everything on it, every frequency of
    /// it. See [`Project::lane_gain_db`] for what it is *not*.
    pub fn lane_gain_db(&self, lane: Lane) -> f32 {
        self.project.lane_gain_db(lane)
    }

    /// Sets it. One undo step -- and **nothing is rebuilt**: a lane's volume
    /// lives at the sum, so the running mixer is handed the new number and
    /// picks it up at its next block ([`crate::audio::MixControls`]). A fader
    /// held down under an arrow key therefore moves the level and nothing else:
    /// no flush, no decoder restart, no hole in the sound and no blink in the
    /// picture.
    ///
    /// The one exception is the timeline that is not mixed at all -- a single
    /// audio lane at unity with the limiter off, which plays through the
    /// bit-exact single-stream path and has no mixer to talk to. Moving *that*
    /// off unity opens one, once; every move after it is live.
    pub fn set_lane_gain_db(&mut self, lane: Lane, db: f32) -> bool {
        if !self.project.set_lane_gain_db(lane, db) {
            return false;
        }
        self.push_mix();
        true
    }

    /// Hands the mix the running mixer is reading its new settings -- the whole
    /// of what a fader and a ceiling cost. With no mixer listening (the
    /// single-lane flat path, and a session with no sound at all) the sound is
    /// rebuilt instead, which is what opens one.
    fn push_mix(&mut self) {
        let gains = self.project.audio_gains();
        let limiter = self.project.limiter();
        // A mixer reading these, or an open on its way to being one: either way
        // the number is heard without a rebuild, and *that* is what a handle
        // left over from an open that found nothing cannot do
        // ([`crate::audio::MixControls::is_reachable`]).
        if let Some(mix) = self.mix.as_ref().filter(|mix| mix.is_reachable()) {
            mix.set(gains, limiter);
            return;
        }
        // Nothing to hand it to. Only a setting that *needs* a mixer is worth
        // the flush and the re-open: a ceiling dragged with the limiter off, or
        // a fader put back to unity, changes not one sample of the flat
        // single-stream path the timeline is already playing
        // ([`AudioSession::is_mixed`]), and reseeking per nudge was a hole in
        // the sound for a change nobody could hear.
        if crate::audio::AudioSession::is_mixed(gains.len(), &gains, limiter) {
            self.reseek_audio();
        }
    }

    /// The master limiter the whole mix is summed through.
    pub fn limiter(&self) -> crate::limiter::Limiter {
        self.project.limiter()
    }

    /// Sets it, live like a lane volume and for the same reason -- the ceiling
    /// a person is dragging has to be the ceiling they are hearing. Nothing is
    /// rebuilt: the running limiter is *retuned* between two blocks
    /// ([`crate::limiter::LimiterState::retune`]), so its delay line keeps
    /// running and the sample count the master clock is made of does not move.
    ///
    /// corner-cut: not an undo step, and deliberately -- [`Project::undo`]
    /// restores the lane list, and the limiter is not in it, so snapshotting
    /// here would push a step whose undo changes nothing a person can see or
    /// hear (the twin [`set_lane_gain_db`](Self::set_lane_gain_db) *is*
    /// undoable only because a lane volume lives in that list). The project
    /// resolution is outside undo for the same reason and says so. Upgrade path
    /// is a history that carries the mix beside the lanes, which every existing
    /// snapshot site would then have to fill in.
    pub fn set_limiter(&mut self, limiter: crate::limiter::Limiter) -> bool {
        if !self.project.set_limiter(limiter) {
            return false;
        }
        self.push_mix();
        true
    }

    /// Lifts one lane's clip out, leaving a gap: black frames on the video lane,
    /// silence on the audio one, and nothing else moves. Reseeks, because what
    /// the playhead sits on has changed. `false` for a bad index -- the lift of
    /// the last placement there is empties the timeline, which is a state
    /// ([`is_empty`](Self::is_empty)) and one undo away.
    pub fn lift_clip(&mut self, lane: Lane, idx: usize) -> bool {
        self.edit(Dirty::Both, |p| p.lift(lane, idx))
    }

    /// Moves that clip onto lane `to` with its head at timeline frame `start` --
    /// the drag that carries a take along its track or onto another one, and its
    /// whole group with it. See [`Project::move_clip`] for the walls `start` is
    /// clamped to and the ways a drop is refused. One undo step, and it reseeks
    /// like every other edit: which lane a clip sits on is what the compositor's
    /// topmost-lane-wins rule reads, and where it starts is what the playhead
    /// reads, so the frame on screen is recomposed at once.
    ///
    /// A *frame*, where the rest of this type takes seconds, for
    /// [`trim_clip`](Self::trim_clip)'s reason: the drag has already put the
    /// pointer on a frame, and converting it back through seconds would be a
    /// rounding step between the box let go of and the box committed.
    pub fn move_clip_to(&mut self, from: Lane, idx: usize, to: Lane, start: u32) -> bool {
        self.edit(Dirty::Both, |p| p.move_clip(from, idx, to, start))
    }

    /// Drags one end of that clip to timeline frame `to`, changing how much of
    /// its source it plays and nothing else on the lane -- see [`Project::trim`]
    /// for the walls it is clamped by, which this fills the source lengths in
    /// for. One undo step per call, so a front-end calls it once, at the release
    /// of the drag. Reseeks like every other edit, which is what makes the
    /// picture (and the sound) follow a new in-point straight away. `false` for
    /// a bad index and for an edge already where it was asked to go.
    ///
    /// A *frame*, where the rest of this type takes seconds: a drag has already
    /// asked [`trim_room`](Self::trim_room) where the edge may land, and that
    /// answer is in frames -- converting it back through seconds would be a
    /// rounding step between the width drawn and the width committed.
    pub fn trim_clip(&mut self, lane: Lane, idx: usize, edge: Edge, to: u32) -> bool {
        // Spelled out rather than through `edit`, whose closure cannot hold a
        // second borrow of the session while it has the project.
        if !self.project.trim(lane, idx, edge, to, &self.counts) {
            return false;
        }
        let now = self.now();
        self.seek(now);
        true
    }

    /// How fast the clip at `idx` of `lane` plays -- what a card shows before
    /// anyone drags it.
    pub fn speed_of(&self, lane: Lane, idx: usize) -> Speed {
        self.project.speed_of(lane, idx)
    }

    /// Sets it, for that clip and its whole group ([`Project::set_speed`]): one
    /// undo step for the lot. Reseeks like every other edit, so the picture runs
    /// -- and the sound is resampled -- from the next frame on rather than at the
    /// next play. The `Err` names the clip in the way when slowing one down
    /// would run it into its neighbour, and nothing changes.
    pub fn set_speed(&mut self, lane: Lane, idx: usize, speed: Speed) -> crate::Result<()> {
        // Spelled out rather than through `edit`, whose closure hands back a
        // bool and would drop the refusal's own words. The playhead moves
        let at = self.project.speeded_playhead(
            lane,
            idx,
            speed,
            secs_to_frame(self.now(), self.meta.frame_rate),
        );
        self.project.set_speed(lane, idx, speed)?;
        let now = at.map_or_else(
            || self.now(),
            |frame| f64::from(frame) / self.meta.frame_rate,
        );
        self.seek(now);
        Ok(())
    }

    /// [`set_speed`](Self::set_speed) without the undo step -- the samples inside
    /// one drag ([`Project::set_speed_live`]). Reseeks like the committing one,
    /// which is what makes the bar move the picture under the hand.
    pub fn set_speed_live(&mut self, lane: Lane, idx: usize, speed: Speed) -> crate::Result<()> {
        // The mapping on every sample, not just at the commit: the bar moves
        // the rate live, and the scene has to stay put across the whole
        // gesture or the last picture before the release is not the one the
        // commit lands on.
        let at = self.project.speeded_playhead(
            lane,
            idx,
            speed,
            secs_to_frame(self.now(), self.meta.frame_rate),
        );
        self.project.set_speed_live(lane, idx, speed)?;
        let now = at.map_or_else(
            || self.now(),
            |frame| f64::from(frame) / self.meta.frame_rate,
        );
        self.seek(now);
        Ok(())
    }

    /// The picture's source frame at `timeline_secs`: which source, and which
    /// frame of it -- the composite a viewer is looking at, through the same
    /// span math the decoder walks. What a test asks to prove a reseek kept
    /// the scene, and nothing in the editor itself reads.
    pub fn video_source_frame_at(&self, timeline_secs: f64) -> Option<(usize, u32)> {
        self.project
            .composite_span_at(secs_to_frame(timeline_secs, self.meta.frame_rate))
            .and_then(|s| s.from)
    }

    /// Cuts every one of `regions` -- `(start, len)` in timeline frames -- out
    /// of the lanes in `scope` and closes each hole, as **one** edit
    /// ([`Project::cut_regions`]): the jumpcut a silence scan
    /// ([`crate::silence`]) asks for, one undo press however many silences it
    /// found. A lane outside `scope` does not move. Reseeks like every other
    /// edit, so what plays is the cut timeline from the playhead on. The `Err`
    /// names what is in the way and nothing changes.
    pub fn cut_regions(&mut self, regions: &[(u32, u32)], scope: &[Lane]) -> crate::Result<()> {
        // Spelled out rather than through `edit`, whose closure hands back a
        // bool and would drop the refusal's own words.
        self.project.cut_regions(regions, scope)?;
        let now = self.now();
        self.seek(now);
        Ok(())
    }

    /// Plays every one of those regions at `speed` instead of cutting them,
    /// closing the room each one no longer needs ([`Project::speed_regions`]) --
    /// one edit, one undo press, and the same scope rule. The `Err` names the
    /// lane and frame in the way and nothing changes.
    pub fn speed_regions(
        &mut self,
        regions: &[(u32, u32)],
        speed: Speed,
        scope: &[Lane],
    ) -> crate::Result<()> {
        self.project.speed_regions(regions, speed, scope)?;
        let now = self.now();
        self.seek(now);
        Ok(())
    }

    /// Where that edge may land, `(first, last)` timeline frame inclusive: what
    /// a drag clamps the pointer to so the box it draws is the box
    /// [`trim_clip`](Self::trim_clip) will commit. `None` for a bad index.
    pub fn trim_room(&self, lane: Lane, idx: usize, edge: Edge) -> Option<(u32, u32)> {
        self.project.trim_room(lane, idx, edge, &self.counts)
    }

    /// Records how long a source is and what rate it was shot at, for a source
    /// index that may be one `Project::import` just made or one it handed back.
    /// See [`Self::counts`] and [`Self::rates`], which are one table in two
    /// vectors and grow together.
    fn note_frames(&mut self, source: usize, frames: u32, rate: Rate) {
        if source == self.counts.len() {
            self.counts.push(frames);
            self.rates.push(rate);
        }
        debug_assert_eq!(self.counts.len(), self.project.sources().len());
        debug_assert_eq!(self.rates.len(), self.counts.len());
    }

    /// The rate noted for whichever source plays `path`, by
    /// [`file_frames`](Self::file_frames)'s rule: one file is one rate however
    /// many audio tracks it carries. [`Rate::REAL_TIME`] for a file this session
    /// has never taken in.
    fn file_rate(&self, path: &Path) -> Rate {
        self.source_of(path)
            .map_or(Rate::REAL_TIME, |i| self.rates[i])
    }

    /// Which file this source's **picture** is decoded from: its stand-in when
    /// the project is cut on stand-ins and one has been made for it, and the
    /// film itself in every other case ([`crate::proxy`]).
    ///
    /// One `stat` per span open, against a decoder open that costs milliseconds
    /// -- so a proxy that appeared (or was deleted) since the last seek is
    /// picked up at this one, and no table of attachments exists to disagree
    /// with the cache.
    ///
    /// The sound never comes through here: [`open_audio`] is given the
    /// project's own source path, so a mix is the film's however this answers.
    pub fn picture_path(&self, source: usize) -> PathBuf {
        let path = &self.project.sources()[source].path;
        let Some(proxy) = self.proxies.then(|| crate::proxy::cached(path)).flatten() else {
            return path.clone();
        };
        // Said out loud, once per span opened, because "the picture came off
        // the stand-in and the delivery did not" is a claim somebody has to be
        // able to check: `export` prints the file *it* reads by the same rule.
        eprintln!("picture: proxy {}", proxy.display());
        proxy
    }

    /// Whether the picture comes off the stand-ins ([`Self::set_proxies`]).
    pub fn proxies(&self) -> bool {
        self.proxies
    }

    /// Cut on the stand-ins, or on the films themselves. Reseeks, so the very
    /// next picture comes from the other file -- which is what makes this a
    /// switch a person can flip while watching.
    ///
    /// Says nothing about *which* films have one: a source with no stand-in
    /// keeps playing its own pictures either way, and one made while this is on
    /// is picked up at the next seek.
    pub fn set_proxies(&mut self, on: bool) {
        if self.proxies == on {
            return;
        }
        self.proxies = on;
        let now = self.now();
        self.seek(now);
    }

    /// Which encoder an export of this project would open its picture on
    /// ([`crate::export::EncoderSeat`]). What a front-end puts into the
    /// [`ExportSettings`](crate::export::ExportSettings) it starts an export
    /// with -- and what the export card shows -- so the pick, the prediction
    /// and the file are one answer.
    pub fn encoder_seat(&self) -> crate::export::EncoderSeat {
        self.encoder_seat
    }

    /// Pick the seat. Nothing to reseek and nothing to re-probe here: this
    /// decides what an *export* opens, never what plays.
    pub fn set_encoder_seat(&mut self, seat: crate::export::EncoderSeat) {
        self.encoder_seat = seat;
    }

    /// The rate this project's mix was picked to run at
    /// ([`Self::set_sample_rate`]), or `None` for the rate the first audio
    /// source derives it to.
    pub fn sample_rate(&self) -> Option<u32> {
        self.sample_rate
    }

    /// Pick the mix's own rate, overriding the derived one. Nothing to reseek
    /// here: it takes effect at the next audio rebuild (a seek, an edit, a
    /// reopen), not on the device already playing.
    pub fn set_sample_rate(&mut self, rate: Option<u32>) {
        self.sample_rate = rate;
    }

    /// Whether an imported film that wants a stand-in gets one made for it
    /// there and then ([`Self::set_auto_proxies`]).
    pub fn auto_proxies(&self) -> bool {
        self.auto_proxies
    }

    /// Make the stand-ins on import, or make none until asked. Nothing to
    /// reseek: this decides what is *started*, never what is decoded -- a proxy
    /// already in the cache is still cut on while [`Self::proxies`] is on.
    pub fn set_auto_proxies(&mut self, on: bool) {
        self.auto_proxies = on;
    }

    /// Which source `path` is, canonicalising only if it has to: a source's own
    /// path is canonical ([`Source::new`]), so a caller handing one back from
    /// [`sources`](Self::sources) -- which is every library row -- hits on the
    /// first walk and never pays for the `canonicalize` syscall the second one
    /// makes. One rule, so a path that names a length names a rate too.
    fn source_of(&self, path: &Path) -> Option<usize> {
        let at = |wanted: &Path| self.project.sources().iter().position(|s| s.path == wanted);
        at(path).or_else(|| at(&Source::new(path, 0).path))
    }

    /// How long the whole of `path` is, in timeline frames: the length noted
    /// when it became a source, so a file sitting in the library with no clip
    /// anywhere still knows how much of itself there is to place. `0` for a
    /// file this session has never taken in.
    ///
    /// By path and not by stream, because a file is one length however many
    /// audio tracks it carries -- the picture's frame count for an mp4, the
    /// playing time for a song.
    pub fn file_frames(&self, path: &Path) -> u32 {
        self.source_of(path).map_or(0, |i| self.counts[i])
    }

    /// What rate the file at `path` was *shot* at, in frames per second -- which
    /// is the timeline's own rate for every source that shares it, and is not
    /// for one placed at another ([`Rate`]). By path like
    /// [`file_frames`](Self::file_frames), and the timeline's rate for a file
    /// this session has never taken in (and for a still and a song, which were
    /// shot at no rate at all).
    ///
    /// What a library row names the file with: the timeline's rate there would
    /// be a lie about a 23.976 fps file the moment one can join.
    pub fn file_fps(&self, path: &Path) -> f64 {
        self.file_rate(path).as_f64() * self.meta.frame_rate
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
        self.edit(Dirty::Picture, |p| p.set_color(lane, idx, params))
    }

    /// The same grade and the same reseek without the undo step
    /// ([`Project::set_color_live`]): what the samples inside one slider drag go
    /// through, so the frame regrades under the hand and the whole gesture is
    /// still a single `z`.
    pub fn set_color_live(&mut self, lane: Lane, idx: usize, params: Option<ColorParams>) -> bool {
        self.edit(Dirty::Picture, |p| p.set_color_live(lane, idx, params))
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

    /// Sets it. Rebuilds the **picture** where the playhead is, so the frame on
    /// screen is recomposed at the new size at once, paused or playing -- and
    /// the sound is not touched at all: the size of the canvas is not something
    /// a sample can carry, so resizing a 4K film mid-play costs neither a hole
    /// in the audio nor an offset against it ([`Dirty`]). `false` for a size
    /// that is not a picture -- zero either way, or past 8K, which is where the
    /// per-frame buffers stop being a sane thing to allocate from a keystroke.
    ///
    /// corner-cut: not an undo step. The project resolution is not in the lane
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
        self.invalidate(Dirty::Picture);
        true
    }

    /// Which HDR-to-SDR rendition this project is watched and exported in
    /// ([`crate::tonemap::Preset`]) -- the reference one until somebody picks
    /// another, and a setting on the project rather than on any clip.
    pub fn tone(&self) -> crate::tonemap::Preset {
        self.project.tone()
    }

    /// Picks one. Rebuilds the **picture** where the playhead is, exactly as
    /// [`set_resolution`](Self::set_resolution) does -- the frame on screen is
    /// remapped at once, paused or playing, and the sound is not touched: a
    /// rendition is not something a sample can carry. `false` for the rendition
    /// already in force.
    ///
    /// An SDR project is unmoved by this by construction: no clip of it builds a
    /// tone map at all ([`crate::tonemap`]), so the reseek recomposes the very
    /// same bytes.
    ///
    /// corner-cut: not an undo step, for the reason the resolution is not
    /// ([`Project::set_tone`]).
    pub fn set_tone(&mut self, preset: crate::tonemap::Preset) -> bool {
        if !self.project.set_tone(preset) {
            return false;
        }
        self.invalidate(Dirty::Picture);
        true
    }

    /// The scaffolding source's own frame rate -- what the project rate started
    /// as, and the one rate a caller offering a list of them must not leave out
    /// ([`native_resolution`](Self::native_resolution)'s reason).
    pub fn native_frame_rate(&self) -> f64 {
        self.native_fps
    }

    /// Cuts this timeline at `fps` from here on: every clip is *conformed* to
    /// the new rate ([`Project::retime`]), every source is read through a
    /// [`Rate`] against it, and an export written from this session comes out at
    /// it. The edit survives as the same seconds of the same footage on a finer
    /// or coarser grid -- a 30 fps take on a timeline moved to 24 keeps its
    /// length and its sync, it is simply counted in 24ths now.
    ///
    /// Reseeks like [`set_resolution`](Self::set_resolution), so the picture and
    /// the sound under the playhead are already the new rate's when this
    /// returns. `false` -- changing nothing -- for a rate that is not a rate, for
    /// the one already in force, and for one no timescale can name
    /// ([`crate::mux::frame_timing`], the same wall a file of an unnameable rate
    /// is refused at).
    ///
    /// corner-cut: not an undo step, exactly as the project resolution is not --
    /// and this one *moves the frame numbers*, so the way back is picking the
    /// old rate (or the media's, [`native_frame_rate`](Self::native_frame_rate))
    /// rather than a `z`, and it lands within a frame of where it started rather
    /// than on it (see [`Project::retime`]). Upgrade path is the same one: the
    /// project's settings snapshotted beside the lanes.
    pub fn set_frame_rate(&mut self, fps: f64) -> bool {
        if !fps.is_finite() || fps <= 0.0 || fps == self.meta.frame_rate {
            return false;
        }
        // Old timeline frames per new one, exactly -- the one map every frame
        // number on every lane goes through. Built before anything is touched,
        // since a rate no timescale can name is a refusal and not a half-retimed
        // project.
        let Ok(k) = Rate::from_fps(self.meta.frame_rate, fps) else {
            return false;
        };
        // Lengths first: the retime holds every clip inside its source's *new*
        // length, so a clip that played to the last frame of its file still
        // does rather than naming one past the end.
        for count in &mut self.counts {
            *count = k.timeline_at(*count).max(1);
        }
        self.project.retime(k, &self.counts);
        // A file's own rate has not changed; what it is *against* has.
        for rate in &mut self.rates {
            *rate = rate.then(k);
        }
        self.meta.frame_rate = fps;
        self.meta.frame_count = k.timeline_at(self.meta.frame_count).max(1);
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
        self.edit(Dirty::Picture, |p| p.set_fit(lane, idx, fit))
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
        // The clipboard's own rule, and only the clipboard's: a paste past the
        // end is an append, never a clip with black in front of it. The frame
        // itself is honoured in [`Project::paste`], because a *drop* names a
        // place on the bed and means it.
        let at = secs_to_frame(timeline_secs, self.meta.frame_rate)
            .min(self.project.timeline_frames());
        self.edit(Dirty::Both, |p| p.paste(at, clip))
    }

    /// Places the whole of `path` played on its audio `stream` at
    /// `timeline_secs`, the way [`paste_at`](Self::paste_at) places a copy --
    /// the door a library row goes through, and the only way a stream other
    /// than the one an import brought in reaches the timeline. The `(file,
    /// stream)` pair becomes a source if it is not one already.
    ///
    /// Refused, changing nothing, unless that stream can join *this* timeline:
    /// same audio parameters as the first source, in the same words
    /// [`import`](Self::import) refuses a file with. One output device and one
    /// copied AAC track mean one *layout* for the whole timeline, so a mono
    /// track cannot join a stereo one. Its sample rate may be its own: the
    /// segment's resampler conforms it at the decoder's door.
    /// A front-end greys such a row out; this is the backstop that keeps a
    /// stale one from making the whole timeline silent.
    ///
    /// Which lane it lands on is decided here, from the file and from `onto` --
    /// the lane it was let go over, if it was let go over one at all: a file
    /// with a picture asked for by no lane, or for one of the first pair, is
    /// pasted across `V1` and `A1` as a grouped take; asked for by a further
    /// *video* lane it is *placed* there with its sound on that lane's own audio
    /// row (`V2` -> `A2`, added if it is not there yet), overwriting what the two
    /// halves land on and rippling nothing; asked for by a further audio lane it
    /// is that file's sound alone. A file with no picture ([`crate::is_audio`]) is placed
    /// on the audio lane it was asked for, or on `A1`, and never on a video
    /// lane. A still image ([`crate::is_image`]) is its mirror: the video lane
    /// it was asked for or `V1`, never an audio one, and it goes down
    /// [`IMAGE_PLACE_SECS`] long rather than at the length it is *allowed* to
    /// be trimmed out to. A caller never has to ask.
    pub fn place_stream_at(
        &mut self,
        timeline_secs: f64,
        path: &Path,
        stream: usize,
        onto: Option<Lane>,
    ) -> crate::Result<bool> {
        let wanted = Source::new(path, stream);
        // Another stream of a file this session has taken in: that is what a
        // library row is, and it is why nothing here has to check dimensions or
        // frame rate -- the file passed that at import. Its length is the one
        // the import noted, so a source that has never been placed is as
        // placeable as one that is already on a lane.
        let frames = self.file_frames(&wanted.path);
        if frames == 0 {
            return Err(format!("{} is not on this timeline", path.display()).into());
        }
        // A still has no sound to hold to the timeline's -- and no length
        // either, so it goes down at [`IMAGE_PLACE_SECS`] rather than at the
        // ten minutes it is *allowed* to be dragged out to.
        let image = crate::is_image(path);
        if !image {
            let first = self.first_audio()?;
            self.audio_matches_cached(&wanted, &first)?;
        }
        let rate = self.file_rate(&wanted.path);
        let source = self.project.import(path, stream);
        self.note_frames(source, frames, rate);
        let clip = Clip {
            fade_in: 0,
            fade_out: 0,
            start: 0,
            in_frame: 0,
            out_frame: match image {
                true => place_frames(frames, self.meta.frame_rate),
                false => frames.max(1),
            },
            source,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };
        // Which lane a source may land on is decided here and only here, so a
        // front-end never has to make the same call twice: a file with no
        // picture goes on an audio lane alone, overwriting and rippling
        // nothing, because there is nothing on the video lane to move along
        // with it. A lane of the source's own kind takes it as it is dropped;
        // only `V1` means the grouped take, since that is the pair the paste
        // spans and a second video lane is a layer of its own.
        // ...and a still is the mirror of a song: a picture and no sound, so a
        // video lane and nothing else -- never the grouped take, which would
        // put a clip on `A1` for a source the audio worker cannot open.
        if image {
            let lane = match onto {
                Some(lane) if lane.kind == LaneKind::Video => lane,
                _ => Lane::V1,
            };
            return Ok(self.place_at(lane, timeline_secs, clip));
        }
        let onto = match (crate::is_audio(path), onto) {
            // No picture: an audio lane and nothing else, whichever one was
            // asked for -- `A1` when none was.
            (true, Some(lane)) if lane.kind == LaneKind::Audio => Some(lane),
            (true, _) => Some(Lane::A1),
            // A picture: the lane it was let go over, unless that is one of the
            // first pair. Those two are the grouped take a paste spans, and a
            // further lane is a layer of its own -- its picture on `V2`, its
            // sound on `A2` (below).
            (false, Some(lane)) if lane.ord > 0 => Some(lane),
            (false, _) => None,
        };
        Ok(match onto {
            // A picture let go over a further *video* track lands there with the
            // sound it came with -- `V2`'s picture, `A2`'s sound, one take and
            // one undo step ([`Project::place_take`]). Dropped on a further
            // audio lane it is the sound alone, which is what asking an audio
            // row for an mp4 means.
            Some(lane) if lane.kind == LaneKind::Video => {
                let at = secs_to_frame(timeline_secs, self.meta.frame_rate);
                self.edit(Dirty::Both, |p| p.place_take(lane, at, clip))
            }
            Some(lane) => self.place_at(lane, timeline_secs, clip),
            // The grouped take, at the frame it was let go on -- past the last
            // clip included, where the bed is black and the ghost was drawn.
            // Not [`Self::paste_at`]: that door clamps to the end for the
            // clipboard, and a drop that named a place is not a paste.
            None => {
                let at = secs_to_frame(timeline_secs, self.meta.frame_rate);
                self.edit(Dirty::Both, |p| p.paste(at, clip))
            }
        })
    }

    /// Takes `path` played on `stream` out of the library -- the door a library
    /// row's own remove goes through, naming the row exactly as
    /// [`place_stream_at`](Self::place_stream_at) does. Refused in
    /// [`Project::remove_source`]'s words while a clip still plays from it, and
    /// for a row this timeline does not have.
    ///
    /// Reseeks like an edit even though nothing playable changed: the clip
    /// indexes into the source list have just moved, and the running workers
    /// were opened against the old numbering.
    ///
    /// The index that went, because everything past it moved down: a caller
    /// holding a source index of its own (a clipboard) has to fix it up or drop
    /// it, or it names a different file afterwards. The **last** row goes like
    /// any other and leaves a session with an empty library -- silent, empty of
    /// clips, and unsaveable ([`save_project`](Self::save_project)); a
    /// front-end showing one is showing an empty window.
    pub fn remove_source(&mut self, path: &Path, stream: usize) -> crate::Result<usize> {
        let wanted = Source::new(path, stream);
        let idx = self
            .project
            .sources()
            .iter()
            .position(|s| *s == wanted)
            .ok_or_else(|| format!("{} is not on this timeline", path.display()))?;
        self.project.remove_source(idx)?;
        // The one thing that shortens the source list, so the one place
        // [`Self::counts`] shortens with it: left behind, the gone file's length
        // would become the wall a *surviving* source is trimmed against
        // ([`trim_room`](Self::trim_room)) and the next import would land its
        // count one index late.
        if idx < self.counts.len() {
            self.counts.remove(idx);
            self.rates.remove(idx);
        }
        let now = self.now();
        self.seek(now);
        Ok(idx)
    }

    /// What the timeline's audio *is*: the chosen stream of the first source
    /// that could have any ([`audio_source_of`]), probed. Every other source is
    /// held to it.
    ///
    /// Through the session's own memo ([`probes`](Self::probes)), because this
    /// is asked on the render thread: the free [`first_audio_of`] is the one a
    /// worker with no session to reach for calls.
    fn first_audio(&mut self) -> crate::Result<Option<crate::AudioProbe>> {
        match audio_source_of(self.project.sources()) {
            Some(first) => {
                let (path, stream) = (first.path.clone(), first.audio_stream);
                self.probe_cached(&path, stream)
            }
            None => Ok(None),
        }
    }

    /// One audio header, read at most once per session per `(path, stream)`.
    fn probe_cached(
        &mut self,
        path: &Path,
        stream: usize,
    ) -> crate::Result<Option<crate::AudioProbe>> {
        let key = (path.to_path_buf(), stream);
        if let Some(probe) = self.probes.get(&key) {
            return Ok(*probe);
        }
        let probe = AudioSession::probe(path, stream)?;
        self.probes.insert(key, probe);
        Ok(probe)
    }

    /// [`audio_matches`] with this session's memo behind the new file's own
    /// header too -- the second of the two opens a place or an import pays.
    fn audio_matches_cached(
        &mut self,
        source: &Source,
        first: &Option<crate::AudioProbe>,
    ) -> crate::Result<()> {
        let probe = self.probe_cached(&source.path, source.audio_stream)?;
        audio_matches_probed(probe, first)
    }

    /// Places `clip` on `lane` alone at the playhead, overwriting what it lands
    /// on and rippling nothing -- the one-lane insert a source with no picture
    /// makes, and the only way an audio-only file reaches the timeline. The
    /// placement belongs to no group (see [`Project::place`]). One undo step,
    /// and it reseeks like every other edit.
    pub fn place_at(&mut self, lane: Lane, timeline_secs: f64, clip: Clip) -> bool {
        let at = secs_to_frame(timeline_secs, self.meta.frame_rate);
        self.edit(Dirty::Both, |p| p.place(lane, at, clip))
    }

    /// Removes `lane`'s clip at `idx` and everything under it, closing the gap
    /// on every lane. Unlike a split this *does* move every following frame, so
    /// the session reseeks to wherever the playhead now points.
    /// [`lift_clip`](Self::lift_clip) is the one that leaves a hole instead.
    /// `false` for a bad index; the last remaining clip goes like any other and
    /// leaves the timeline empty ([`is_empty`](Self::is_empty)).
    ///
    /// The lane travels because the index is a lane's own: `V2`'s third clip is
    /// not `V1`'s, and a front-end that could only say "the third clip" would
    /// delete the wrong one the moment a second lane exists.
    pub fn delete_clip(&mut self, lane: Lane, idx: usize) -> bool {
        self.edit(Dirty::Both, |p| p.delete_in(lane, idx))
    }

    /// Undoes the last successful edit, and reseeks like a delete.
    pub fn undo(&mut self) -> bool {
        self.edit(Dirty::Both, Project::undo)
    }

    /// Redoes the last edit [`undo`](Self::undo) took back, and reseeks like
    /// a delete.
    pub fn redo(&mut self) -> bool {
        self.edit(Dirty::Both, Project::redo)
    }

    /// Takes `path` into the **library**: it becomes a source of this session,
    /// with its length noted ([`file_frames`](Self::file_frames)), and nothing
    /// is placed on any lane. What reaches the timeline is decided afterwards,
    /// by dragging the row onto a lane
    /// ([`place_stream_at`](Self::place_stream_at)) -- importing a file is not
    /// a decision about where it plays.
    ///
    /// The source index, new or the one this file already had: importing twice
    /// registers once. *Not* an undo step -- a source entry alone changes
    /// nothing playable, so there is nothing for [`undo`](Self::undo) to take
    /// back, and nothing reseeks.
    ///
    /// Refused unless it can join the timeline -- a decoder this machine can
    /// open, and audio parameters that agree or none at all; the `Err` names
    /// the property that disagrees,
    /// for a caller to show. Nothing is changed by a refusal. A *resolution* of
    /// its own is not a refusal: the clip is placed on the project canvas by its
    /// fit policy ([`PlaybackSession::set_fit`]) -- and neither is a *frame
    /// rate* of its own: the file is placed for the seconds it lasts and read at
    /// [`Rate`] against the timeline's rate, so it plays at the speed it was
    /// shot at.
    pub fn import(&mut self, path: &Path) -> crate::Result<usize> {
        if crate::is_audio(path) {
            return self.import_audio(path);
        }
        if crate::is_image(path) {
            return self.import_image(path);
        }
        // Both halves on this thread, which is what a caller with nothing else
        // to do wants; a front-end splits them across a worker instead
        // ([`probe_import`](Self::probe_import)).
        let probe = Self::probe_import(self.import_gate(), path)?;
        Ok(self.take_probe(path, probe))
    }

    /// Everything an import is *checked against*, taken off this timeline in
    /// one owned piece: the sources its audio must agree with and the meta its
    /// rate is read against. Costs two clones and touches no disk, so the
    /// thread holding the session can hand it to a worker and go on painting.
    pub fn import_gate(&self) -> ImportGate {
        ImportGate {
            sources: self.project.sources().to_vec(),
            timeline: self.meta,
        }
    }

    /// The reading half of [`import`](Self::import), with no session in it: the
    /// container's header, the decoder this machine would open it with, and the
    /// audio check against the gate -- every read an import pays.
    ///
    /// This is the half that costs. Measured on a 24 GB 4K HEVC remux: 21.4 s of
    /// header with the pages cold, 429 ms warm, plus 1-4 s of probing the
    /// timeline's own first source. So a front-end runs *this* on its background
    /// executor and hands what comes back to
    /// [`import_probed`](Self::import_probed), which costs a push -- the same
    /// split [`parse_subtitles`](Self::parse_subtitles) makes, for the same
    /// reason.
    ///
    /// The container fork only: a song and a still fork before the demuxer
    /// ([`import`](Self::import)) and pay a header read of their own.
    ///
    /// The `Err` is the refusal [`import`](Self::import) would have given, in
    /// the same words -- nothing is refused twice and nothing is worded twice.
    pub fn probe_import(gate: ImportGate, path: &Path) -> crate::Result<ImportProbe> {
        let (meta, _) = Demuxer::open(path)?;
        let first = first_audio_of(&gate.sources)?;
        // Stream 0: an import brings a file in on its first audio track, and
        // [`place_stream_at`](Self::place_stream_at) is how any other one of
        // its streams reaches the timeline afterwards.
        // ...and the rate it will be read at comes back from the check that
        // accepted it, so the number the refusal was decided on is the number
        // the file is played by.
        let rate = matches_timeline(&Source::new(path, 0), &meta, &gate.timeline, &first)?;
        Ok(ImportProbe { gate, meta, rate })
    }

    /// Registers a file already read by [`probe_import`](Self::probe_import).
    /// The whole of what an import costs the thread that calls it: a source
    /// entry and its length, no open, no seek, no decode -- which is why the
    /// probe is a separate door.
    ///
    /// The timeline may have moved while the worker read: a source removed or a
    /// file opened is a different set of things the probe was checked against,
    /// so the probe is re-read here ([`ImportGate`]) and a stale one is simply
    /// imported the slow way rather than trusted. Same answer either way, so no
    /// caller has to know which arm it took.
    pub fn import_probed(&mut self, path: &Path, probe: ImportProbe) -> crate::Result<usize> {
        match probe.gate.sources == self.project.sources() && probe.gate.timeline == self.meta {
            true => Ok(self.take_probe(path, probe)),
            false => self.import(path),
        }
    }

    /// The push both import doors end at.
    fn take_probe(&mut self, path: &Path, probe: ImportProbe) -> usize {
        let source = self.project.import(path, 0);
        // Its length *on this timeline*: a file shot slower than the timeline
        // runs covers more frames than it holds, which is the whole of what
        // placing it at another rate means ([`Rate`]).
        self.note_frames(
            source,
            probe.rate.timeline_at(probe.meta.frame_count),
            probe.rate,
        );
        source
    }

    /// The same registration for a standalone audio file: a song has no
    /// picture, so the length noted for it is its playing time rounded up to
    /// whole frames, which is the only frame count such a source has.
    ///
    /// Refused in the same words as any other import when its rate or layout
    /// disagrees with the timeline's -- one output device, one set of
    /// parameters -- and refused outright on a silent timeline, which has no
    /// device open at all. Nothing is changed by a refusal.
    fn import_audio(&mut self, path: &Path) -> crate::Result<usize> {
        let first = self.first_audio()?;
        // Stream 0, and there is no other: a standalone audio file carries one
        // track (`AudioSession::probe_streams`), so nothing here has a stream
        // to pick the way `place_stream_at` does for an mp4.
        self.audio_matches_cached(&Source::new(path, 0), &first)?;
        let frames = audio_frames(path, self.meta.frame_rate)?;
        let source = self.project.import(path, 0);
        self.note_frames(source, frames, Rate::REAL_TIME);
        Ok(source)
    }

    /// The same registration for a still image, and the shortest of the three:
    /// a picture has nothing to agree with. No frame rate (it is one picture),
    /// no audio (it is silent), and a resolution of its own is what every
    /// import is allowed -- the clip is placed on the project canvas by its fit
    /// policy. The one refusal left is a file that is not a picture at all, or
    /// one too big to compose ([`crate::is_resolution`]), and the header alone
    /// answers both.
    ///
    /// The length noted is the wall a trim is held to, not a duration the file
    /// has: see [`image_frames`].
    fn import_image(&mut self, path: &Path) -> crate::Result<usize> {
        let (width, height) = crate::decode::image_size(path)?;
        if !crate::is_resolution(width, height) {
            return Err(format!(
                "{} is {width}x{height}, which is not a picture this engine composes",
                path.display()
            )
            .into());
        }
        let source = self.project.import(path, 0);
        self.note_frames(source, image_frames(self.meta.frame_rate), Rate::REAL_TIME);
        Ok(source)
    }

    /// Applies an edit and, if it took, rebuilds **what it dirtied** and nothing
    /// else ([`Dirty`]). A structural edit reseeks: `seek` clamps to the new
    /// duration and keeps playing playing, so a delete that shortens the
    /// timeline past the playhead lands on the last frame. A grade or a fit
    /// policy rebuilds the picture where it stands, with the sound running
    /// through it untouched -- no flush, no re-open, no hole -- and an
    /// equalizer the other way round.
    fn edit(&mut self, dirty: Dirty, f: impl FnOnce(&mut Project) -> bool) -> bool {
        if !f(&mut self.project) {
            return false;
        }
        self.invalidate(dirty);
        true
    }

    /// Rebuilds the dirtied half (or both) at the playhead.
    fn invalidate(&mut self, dirty: Dirty) {
        match dirty {
            Dirty::Picture => {
                let (_, target) = self.landing(self.now());
                self.start_picture(target);
            }
            Dirty::Sound => self.reseek_audio(),
            Dirty::Both => {
                let now = self.now();
                self.seek(now);
            }
        }
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
            audio.set_playing(true);
        }
    }

    pub fn pause(&mut self) {
        if !self.clock.is_playing() {
            return;
        }
        if let Some(audio) = &self.audio {
            audio.set_playing(false);
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

    /// Stops the transport where the timeline stops: the clock is paused and put
    /// on the out point.
    ///
    /// Playing past the last frame is wall time counting on past the end of the
    /// media -- there is no picture left to pace it -- so a session left at EOF
    /// reports a `now()` that walks off the end of the timeline in real time,
    /// and a caller that reads the playhead (a cut, a paste, an insert) then
    /// acts at a position no clip is at. How far off depends only on how long
    /// the window was left open, so no clamp in a caller is worth writing.
    ///
    /// End of stream is deliberately left set: the picture is still the last
    /// frame and this is still the end. That is what makes it safe to call on
    /// the crossing into it -- the state the transport shows does not change,
    /// only the running clock stops.
    pub fn halt_at_end(&mut self) {
        self.pause();
        self.clock.seek_to(self.timeline_duration());
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

    /// A copy of the most recent mono samples the device was handed, and the
    /// rate they play at: the signal *after* every clip's equalizer, speed and
    /// the mix, which is what an analyser drawn behind an EQ curve has to show.
    /// `None` with no audio output at all, and empty until the feeder has
    /// written once (right after a seek, for instance).
    ///
    /// Never blocks anything that matters: the feeder holds this lock for one
    /// memcpy and the device's own callback lives in the plugin, on the far
    /// side of a ring buffer, where it cannot see this at all.
    pub fn audio_tap(&self) -> Option<(Vec<f32>, u32)> {
        let audio = self.audio.as_ref()?;
        let tap = audio.tap.lock().unwrap().clone();
        Some((tap, audio.sample_rate))
    }

    /// Says that this session is being **watched in real time**, so a picture
    /// whose moment has passed is worth nothing and the decode worker may drop
    /// it where it is cheap to drop -- before the conversion (a full BGRA pass
    /// over 8 MB at 1080p) and before the queue.
    ///
    /// Off by default, and that is the contract every *pull* consumer relies on:
    /// an export, the file-level [`DecodeSession`] API and a test draining as
    /// fast as it can all ask for a range of frames and must be given every one
    /// of them, in order. Only a viewer has a moment for a picture to be past.
    ///
    /// A front-end that turns this on was dropping those very frames itself, one
    /// repaint later (`Player::pump` takes everything due and shows the last):
    /// what changes is where the work stops, and therefore how quickly a decoder
    /// that fell behind catches up.
    ///
    /// It does not replace [`resync_picture`](Self::resync_picture): both
    /// mechanisms are live and this one is the first line. It catches up without
    /// restarting anything, which is why the front-end's resync -- a picture
    /// restart, once per `RESYNC_GAP` -- now fires only where dropping was not
    /// enough (a decoder so far behind that even one picture in nine
    /// ([`crate::decode`]'s `LATE_RUN`) cannot close the gap).
    pub fn drop_late_pictures(&mut self, on: bool) {
        self.drop_late = on;
    }

    /// Repositions the timeline to `secs`, clamped to the *timeline*. Both decode
    /// workers are replaced rather than steered: a fresh channel cannot hold a
    /// stale frame, and it works just as well after EOF, where the old workers
    /// have already exited.
    ///
    /// Playing stays playing and paused stays paused -- but a paused seek still
    /// decodes, so the caller can show the frame it landed on.
    pub fn seek(&mut self, secs: f64) {
        let (t, target) = self.landing(secs);
        let was_playing = self.clock.is_playing();
        self.stop_audio();
        self.start_picture(target);
        let audio_running = self.start_audio(target);
        // Same invariant as `play`: position the clock before audio restarts.
        if audio_running && self.clock.source() != ClockSource::Audio {
            self.clock.switch_to_audio();
        }
        self.clock.seek_to(t);
        self.resume(was_playing);
    }

    /// Abandons whatever the picture worker still owes and restarts it at the
    /// clock. The sound and the clock are untouched -- this is the *video*
    /// catching up to its master, not a seek -- so nothing the ear is following
    /// moves, and a caller that is behind by more than it can watch calls this
    /// instead of waiting for a decoder that only falls further back
    /// (`Player::pump`).
    pub fn resync_picture(&mut self) {
        self.invalidate(Dirty::Picture);
    }

    /// Where a seek to `secs` lands: the clamped timeline position and the
    /// timeline frame it means. The one place the clamp lives, so every
    /// invalidation below lands on the same frame a seek would.
    fn landing(&self, secs: f64) -> (f64, u32) {
        let fps = self.meta.frame_rate;
        let total = self.project.timeline_frames();
        let t = secs.clamp(0.0, f64::from(total) / fps);
        (t, secs_to_frame(t, fps).min(total.saturating_sub(1)))
    }

    /// Rebuilds the **picture** at `target` and touches nothing else: the sound
    /// keeps playing out of the same stream, the clock keeps running, and a
    /// grade dragged during playback is therefore silent.
    fn start_picture(&mut self, target: u32) {
        // Abandon first, drop second: the old worker may be parked in `send` on
        // the bounded channel, where only the disconnect wakes it -- and it
        // then finds the span already superseded. The *thread* is kept, and
        // with it the open file and the open decoder, so a seek onto the same
        // source costs a demuxer seek and a decoder flush rather than the
        // VA-API init (98 ms) a new worker would pay; a worker that cannot take
        // the new span is retired by [`retire`](Self::retire) instead, and
        // nothing on this thread joins either way.
        self.worker.abandon();
        // The span runs from `target` to the end of whatever it landed in -- a
        // clip, or a gap, which starts a black-frame worker instead. `None` is
        // the emptied timeline, black too, and `start_span` says so.
        self.start_span(self.project.composite_span_at(target));
        self.eos = false;
    }

    /// Silences the device and supersedes whatever is feeding it: the first half
    /// of every audio restart, and the only thing that ever discards queued
    /// sound.
    fn stop_audio(&mut self) {
        self.mix = None;
        if let Some(audio) = &self.audio {
            let ao = audio.ao.lock().unwrap();
            ao.set_active(false);
            // Bumped and flushed while holding the device lock, because the
            // feeder re-checks the epoch inside that same lock: whatever it is
            // carrying can no longer reach the ring after this point.
            audio.epoch.fetch_add(1, Ordering::Release);
            // Nothing of the new stream has been played yet, by definition.
            audio.content_at.store(-1, Ordering::Release);
            ao.flush();
            // The flushed samples are not being played any more, so nothing may
            // still be drawn from them.
            audio.tap.lock().unwrap().clear();
        }
    }

    /// Puts the device back where it was: playing if it was playing, and with
    /// the clock held until the new stream is really audible (see
    /// [`Audio::content_at`]).
    ///
    /// Only the *intent* is set here: the open runs on the feeder
    /// ([`start_audio`](Self::start_audio)), so the device is started by that
    /// stream's first real sample instead ([`Audio::wants_active`]). A device
    /// made active over an empty ring plays its own silence and counts one
    /// underrun per starved quantum -- a scripted cold play-seek-scrub session
    /// used to go from 5 to 1721 of them -- while the ear hears the same silence
    /// either way, so nothing is gained for the count that is lost.
    fn resume(&mut self, was_playing: bool) {
        self.priming = true;
        if let Some(audio) = &self.audio {
            audio.set_playing(was_playing);
        }
    }

    /// Rebuilds the **sound** from `target` -- the second half of a restart --
    /// and says whether a feeder took the job. Not whether it will find
    /// anything to play: the stream is opened on that feeder
    /// ([`Audio::spawn_feeder_deferred`]), so a silent timeline is heard of
    /// there, not here. Nothing on this thread reads the file.
    ///
    /// The clock is unaffected by the wait, which is the reason it may be a
    /// long one: it holds where the seek put it until the *first real sample*
    /// is queued ([`Audio::content_at`], [`tick`](Self::tick)), whether that is
    /// ten milliseconds later or ten seconds.
    ///
    /// `true` is therefore *optimistic*, and the callers' `switch_to_audio` with
    /// it: a timeline that turns out to have nothing to play ends its feeder at
    /// once, which sets `fed_all` and hands the clock back to wall time at the
    /// next tick, from where it stood ([`PlaybackClock::switch_to_wall`] is
    /// continuous, so nothing jumps).
    fn start_audio(&mut self, target: u32) -> bool {
        let fps = self.meta.frame_rate;
        let mut audio_running = false;
        let mut live = None;
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
            // ...and the rates, the same way again: a speeded segment is
            // resampled inside its own worker, before the mix, so what the ear
            // hears is what the timeline shows. Empty for a project nobody has
            // speeded, which is the path that decodes the same samples it always
            // did.
            let speeds = self.project.audio_speeds_from(target, fps);
            // ...and the fades, the same way again: a clip's own envelope runs
            // inside its worker, after the rate and the equalizer, so a fade
            // edit is audible at once exactly as an EQ edit is.
            let fades = self.project.audio_fades_from(target, fps);
            // Each source on the stream it was placed with: what plays is what
            // the library row said, and what an export copies (`export::run`).
            let sources = self.project.audio_sources();
            // ...and the mix settings over the lot: each lane's own volume on
            // its way into the sum and the master limiter over the sum. These
            // two are handed over *live* as well as at the open: they sit at
            // the sum, where nothing is decoded, so a fader moved later is a
            // number the running mixer picks up rather than a stream rebuilt
            // under the ear ([`crate::audio::MixControls`]).
            let gains = self.project.audio_gains();
            let limiter = self.project.limiter();
            let controls = crate::audio::MixControls::new(gains.clone(), limiter);
            // Only a mixed timeline has a mixer to talk to; the single-lane
            // flat path is the bit-exact one and has none. Decided here from
            // the very lists the open is about to be handed
            // ([`AudioSession::is_mixed`]) rather than read off the handle
            // afterwards, because there *is* no afterwards on this thread any
            // more -- the open runs on the feeder.
            let mixed = crate::audio::AudioSession::is_mixed(segs.len(), &gains, limiter);
            let worker_controls = Arc::clone(&controls);
            // ...and the rate the mix runs at, if the project picked one: the
            // same override [`crate::edith::save`] writes and [`Self::open_project`]
            // reads back.
            let sample_rate = self.sample_rate;
            // ...and the open itself -- every track a segment names, its packet
            // table, its priming -- happens there too. That is the whole point:
            // one `pread` on a cold 25 GB film is seconds, and this is called
            // from a ruler drag ([`Audio::spawn_feeder_deferred`]).
            audio_running = audio.spawn_feeder_deferred(move || {
                match AudioSession::open_mixed_streams_live_fade(
                    &sources,
                    &segs,
                    &eqs,
                    &speeds,
                    &fades,
                    &gains,
                    limiter,
                    Some(&worker_controls),
                    sample_rate,
                ) {
                    Ok(Some((_, rx))) => Some(rx),
                    // A silent timeline, and a source that will not open: both
                    // feed nothing, which the feeder itself reports by ending.
                    // The mix goes with them -- no mixer was started, so a fader
                    // moved later must rebuild the sound rather than hand its
                    // number to a thread that does not exist
                    // ([`crate::audio::MixControls::detach`]).
                    Ok(None) => {
                        worker_controls.detach();
                        None
                    }
                    Err(e) => {
                        worker_controls.detach();
                        // A read this session abandoned on purpose -- the seek
                        // after it, or the session going away -- is not a file
                        // that would not open, and says nothing.
                        if !crate::demux::is_cancelled(&e) {
                            eprintln!("timeline audio open failed: {e}");
                        }
                        None
                    }
                }
            });
            live = (audio_running && mixed).then_some(controls);
            if !audio_running {
                // Nothing will ever be fed again; let `tick` fall to wall time
                // instead of waiting on a device that has no more work coming.
                audio.fed_all.store(true, Ordering::Release);
            }
        }
        self.mix = live;
        audio_running
    }

    /// Rebuilds the sound alone, from where the playhead is: what a change that
    /// no picture can show goes through. The video worker is left decoding, so
    /// the picture does not blink -- and the clock, held still by
    /// [`resume`](Self::resume) until the new stream is audible, puts the sound
    /// back exactly where the picture already is.
    ///
    /// End of stream is deliberately left alone -- no picture worker is started
    /// here, so a timeline that was played out still is; see
    /// [`is_eos`](Self::is_eos).
    fn reseek_audio(&mut self) {
        let (t, target) = self.landing(self.now());
        let was_playing = self.clock.is_playing();
        self.stop_audio();
        let audio_running = self.start_audio(target);
        if audio_running && self.clock.source() != ClockSource::Audio {
            self.clock.switch_to_audio();
        }
        self.clock.seek_to(t);
        self.resume(was_playing);
    }

    /// Renders the edited timeline to `out` on a worker thread. The session is
    /// untouched -- the export decodes the source file for itself -- but a
    /// caller should still [`pause`](PlaybackSession::pause) first, because
    /// playback and export otherwise fight over the same decoder hardware.
    ///
    /// Later edits do not reach a running export: it works from a snapshot of
    /// the edit list taken here.
    ///
    /// corner-cut: this defaults the settings; the app still calls it that way,
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
        crate::export::start(
            self.project.export_snapshot(),
            self.meta,
            out,
            settings,
            self.sample_rate,
        )
    }

    /// Moves the clock forward; the caller runs this once per rendered frame.
    /// Costs an atomic load, and once per session a mode switch.
    pub fn tick(&mut self) {
        // Before every early return below: where the playhead stands is what the
        // decode worker drops its stale pictures against, and a front-end that
        // repaints without draining (a slow frame, a busy render thread) must
        // still be moving it -- otherwise the queue fills with pictures whose
        // moment passed while nobody was taking them.
        self.publish_playhead();
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
            //
            // The device is stopped with it: nothing will ever be queued for
            // this stream again, so every quantum it would go on playing is
            // silence counted as a decoder that fell behind -- the tail of a
            // stream, where [`crate::ao`]'s own `primed` covers the head of
            // one. A seek from here starts it again like any other
            // ([`resume`](Self::resume)), which is why the stamp goes with it:
            // there is no sample of *this* stream left to start on.
            audio.set_playing(false);
            audio.content_at.store(-1, Ordering::Release);
            self.clock.switch_to_wall();
            return;
        }
        if self.priming {
            // A stream that has been started and has not been heard yet: the
            // device is playing the silence of its own restart, and none of it
            // is timeline time ([`Audio::content_at`]). The clock holds where
            // the seek left it -- so the picture holds too, on the frame it
            // landed on, rather than sliding ahead of a sound that has not
            // started.
            let started = audio.content_at.load(Ordering::Acquire);
            if started < 0 {
                return;
            }
            // ...and then the *first sample of the stream* becomes the
            // reference, not whatever position this poll happened to catch: the
            // clock's own anchor is its first report ([`PlaybackClock`]), and
            // anchoring it here is what makes a mid-play edit cost no offset.
            //
            // corner-cut: `started` is the position when the sample was queued,
            // and it is heard at the next callback -- up to one quantum (5-21
            // ms) later, so that much of the restart is still counted. Upgrade
            // path is the position at the first *pop* of real audio, which only
            // the plugin's RT thread can see.
            self.clock
                .set_audio_position(started as u64, audio.sample_rate);
            self.priming = false;
        }
        self.clock
            .set_audio_position(position as u64, audio.sample_rate);
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
/// parameters are `timeline` and whose audio probes as `first`: a decoder this
/// machine can open, audio parameters that agree (or no audio at all), and a
/// frame rate that can be named
/// against the timeline's. The [`Rate`] it will be read at comes back with the
/// answer, so the number the check was decided on is the number it plays by.
///
/// *Not* the same size any more. A project has its own resolution
/// ([`PlaybackSession::set_resolution`]) and every clip is placed on it by its
/// own fit policy, so a 640x360 file joining a 1920x1080 timeline is a
/// letterboxed clip rather than a refusal, and so is a frame rate of its own:
/// the timeline keeps its own rate and the file is read at [`Rate`] against it
/// ([`PlaybackSession::import`] notes how long it is *here*). Nor the same
/// codec: every clip opens its own decoder, so a mixed-codec timeline is a
/// decoder each rather than a refusal.
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
) -> crate::Result<Rate> {
    // The codec is *not* held to the timeline's -- every clip opens its own
    // decoder, so an H.264 take and an HEVC one play on one timeline. What the
    // codec gate was really protecting against stays, in the one place that can
    // actually answer it: on a machine without the VA-API plugin an HEVC or VP9
    // clip would be black frames with the refusal on stderr alone, so the file
    // is asked for a decoder *here*, at the door, where the `Err` is still
    // something a front-end can show. A zero-length range probes and starts no
    // worker, and H.264 costs nothing at all -- [`DecodeSession::open_worker`]
    // only enters VA-API off that path.
    DecodeSession::open_worker(
        &source.path,
        0,
        0,
        ColorParams::default(),
        Composer::passthrough(),
        // A zero-length range decodes nothing, so no table is built and the
        // rendition cannot matter here.
        crate::tonemap::Preset::default(),
    )?;
    // The frame rate is *not* a refusal any more: a file shot at another rate is
    // placed for the seconds it lasts and read through [`Rate`] at the
    // decoder's door, so it plays at the speed it was shot at on a timeline
    // that counts frames faster or slower.
    //
    // A rate that cannot be *named* still is, and this is the one place it is
    // caught: `Rate::from_fps` builds itself out of the muxer's own timescales,
    // and a file whose rate has none of those (a container that says nothing and
    // holds one block, so the demuxer measures 0 fps) would otherwise be read
    // 1:1 in silence -- the old rate gate refused those too, by arithmetic
    // accident. The `Err` is `mux::frame_timing`'s own words.
    let rate = Rate::from_fps(meta.frame_rate, timeline.frame_rate)?;
    audio_matches(source, first)?;
    Ok(rate)
}

/// Whether a candidate's audio may join a timeline whose first source probes as
/// `first`: same layout -- or no audio of its own at all, which is silence over
/// the clip's span and agrees with everything. One output device and one
/// exported track is all there is, so one width; the *rate* is conformed rather
/// than matched ([`crate::audio::Resample`]), which is why it is not asked
/// about here.
///
/// The audio half of [`matches_timeline`], which a stream placed on its own
/// ([`PlaybackSession::place_stream_at`]) has to pass while the picture it
/// comes with is already on the timeline.
///
/// The format itself is deliberately *not* part of this: an mp3 that agrees on
/// layout plays alongside a timeline of mp4s perfectly well. What it
/// cannot do is be copied into an export, which is a refusal of its own, at
/// export time (`AudioSession::copy_multi_streams`).
/// What a timeline's audio is held to: the probe of the first source that could
/// have any. A still image is never it -- a picture defines no rate and no
/// layout, so a PNG at index 0 would otherwise be *probed* as a broken mp4 and
/// fail the open outright.
///
/// `Ok(None)` for a timeline whose first such source is silent, and for one of
/// nothing but stills. That is a silent timeline, and a file with sound is
/// still refused by [`audio_matches`] in the words it always used ("the file
/// has audio, the timeline is silent") -- the device was opened on source 0's
/// track and there is none to open. This function widens which source is asked,
/// not what the answer means.
fn first_audio_of(sources: &[Source]) -> crate::Result<Option<crate::AudioProbe>> {
    match audio_source_of(sources) {
        Some(first) => AudioSession::probe(&first.path, first.audio_stream),
        None => Ok(None),
    }
}

/// *Which* source that is: the first one that is not a still. The single answer
/// to "what does this timeline's sound come from", so that probing it
/// ([`first_audio_of`]) and opening the device on it
/// ([`PlaybackSession::open_project`]) can never disagree -- source 0 is only
/// the scaffolding file until a save renumbers it or a library removal takes it
/// away ([`crate::Project::remove_source`]), and a PNG at index 0 opened as the
/// device is a timeline that plays silent although every clip on it has sound.
///
/// `None` for a library of nothing but stills, and for one with no file at all:
/// both are silent timelines.
fn audio_source_of(sources: &[Source]) -> Option<&Source> {
    sources.iter().find(|s| !crate::is_image(&s.path))
}

fn audio_matches(source: &Source, first: &Option<crate::AudioProbe>) -> crate::Result<()> {
    let probe = AudioSession::probe(&source.path, source.audio_stream)?;
    audio_matches_probed(probe, first)
}

/// The verdict alone, once the file's own header is in hand: the half a session
/// with a memo behind it ([`PlaybackSession::audio_matches_cached`]) does not
/// have to open the file for.
fn audio_matches_probed(
    probe: Option<crate::AudioProbe>,
    first: &Option<crate::AudioProbe>,
) -> crate::Result<()> {
    // The layout, which the audio worker holds every source to anyway -- and not
    // the rate any more: a file written at another one is resampled to the
    // timeline's at the decoder's door ([`crate::audio::Resample`]), exactly as
    // a file shot at another frame rate is read through [`Rate`]. Both silent is
    // a match.
    if probe.map(|p| p.channels) == first.map(|p| p.channels) {
        return Ok(());
    }
    // ...and a *silent* file joins whatever the timeline is: it contributes
    // silence over its span, which is the very thing both audio paths already
    // synthesise for a gap ([`AudioSession::open_multi_streams_speed`] plays
    // it, `AudioSession::copy_multi_streams` writes it). There is no rate and
    // no layout to disagree about, so there is nothing left to refuse.
    let Some(probe) = probe else {
        return Ok(());
    };
    Err(match first {
        None => "the file has audio, the timeline is silent".to_string(),
        Some(b) => format!(
            "audio {} ch does not match the timeline's {} ch",
            probe.channels, b.channels
        ),
    }
    .into())
}

/// How many timeline frames a still image is *held to*: [`IMAGE_MAX_SECS`] of
/// them, since an image has no length of its own. The number a source's own
/// entry in [`PlaybackSession::counts`] gets, so a trim may drag an image out
/// to ten minutes and no further -- and so a saved project reloads, because
/// [`PlaybackSession::open_project`] recomputes exactly this and refuses a clip
/// that ends past it.
fn image_frames(fps: f64) -> u32 {
    ((IMAGE_MAX_SECS * fps)
        .ceil()
        .clamp(1.0, f64::from(u32::MAX))) as u32
}

/// How long a still is placed for: [`IMAGE_PLACE_SECS`], never past the length
/// it is held to and never zero (a clip is never empty).
fn place_frames(count: u32, fps: f64) -> u32 {
    ((IMAGE_PLACE_SECS * fps)
        .ceil()
        .clamp(1.0, f64::from(count.max(1)))) as u32
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
        content_at: Arc::new(AtomicI64::new(-1)),
        wants_active: Arc::new(AtomicBool::new(false)),
        tap: Arc::new(Mutex::new(Vec::with_capacity(TAP_SAMPLES))),
        feeders: Arc::new(AtomicUsize::new(0)),
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
                Err(_) => {
                    // Decoder finished or went away. What is in the ring still
                    // plays out; what comes after it is the end of the timeline
                    // and not a decoder that fell behind, so the device is told
                    // to stop counting it. Under the lock and past the epoch,
                    // like every other word this thread has with the device: a
                    // superseded feeder must not say this about the stream that
                    // replaced it.
                    let ao = audio.ao.lock().unwrap();
                    if audio.epoch.load(Ordering::Acquire) == epoch {
                        ao.stream_ended();
                    }
                    return;
                }
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
            let accepted = match ao.write(&pending) {
                Some(n) => n,
                None => {
                    // Output died; flag it so `tick` moves the clock to wall time.
                    audio.died.store(true, Ordering::Release);
                    return;
                }
            };
            // The first sample of this stream to reach the ring, stamped with
            // what the device had played by then: everything before it was the
            // silence of the restart, and [`PlaybackSession::tick`] anchors the
            // clock here so that silence is never timeline time. Under the
            // device lock, with the epoch already checked, so the stamp and the
            // sample belong to the same stream.
            if accepted > 0 && audio.content_at.load(Ordering::Acquire) < 0 {
                let at = ao.position().unwrap_or(0).max(0);
                audio.content_at.store(at, Ordering::Release);
                // ...and the device position this stream's samples are counted
                // from ([`Audio::played_out`]). Rebased *here* rather than at
                // the seek, because the open runs on this thread now: every
                // millisecond it took is device time nobody fed, and counting
                // it would make the stream read as played out that long before
                // it really is -- an early hand-off to wall time at the end of
                // the timeline. The seek's own rebase still stands for the
                // stream that never queues a sample at all.
                audio
                    .fed
                    .store(at as u64 * audio.channels, Ordering::Relaxed);
                // ...and *this* is where a playing timeline's device starts: on
                // the first sample there is to play, under the same lock and
                // past the same epoch check as the stamp, so the silence of the
                // open was never played and never counted
                // ([`Audio::wants_active`]).
                if audio.wants_active.load(Ordering::Acquire) {
                    ao.set_active(true);
                }
            } else if accepted == 0 && audio.content_at.load(Ordering::Acquire) < 0 {
                // Nothing taken with nothing yet queued is the *flush* of the
                // restart still standing: the ring refuses a write until a
                // callback has consumed the flag (`engine_audio::accept`), and
                // only a running device has callbacks. So the device is started
                // here too -- there is a sample in hand, which is what the wait
                // was for -- and the quantum or two of that handshake is the
                // silence `Shared::primed` does not count.
                if audio.wants_active.load(Ordering::Acquire) {
                    ao.set_active(true);
                }
            }
            accepted
        };
        audio.fed.fetch_add(accepted as u64, Ordering::Relaxed);
        audio.note_tap(&pending[..accepted]);
        pending.drain(..accepted);
        if !pending.is_empty() {
            // Ring full: nothing to do but let the device drain it. Except when
            // *nothing* was taken, which after a flush means the ring is still
            // waiting for the device to consume the flush flag rather than
            // being full: that is the head of a restart, and every millisecond
            // spent parked there is a millisecond of silence.
            match accepted {
                0 => thread::sleep(FLUSH_WAIT),
                _ => thread::sleep(RING_FULL_WAIT),
            }
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
    use super::{Edge, Lane, PlaybackSession, Source, audio_source_of, secs_to_frame};
    use std::path::PathBuf;

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    /// A picked project rate overrides the derived one, and its absence leaves
    /// the source's own -- the seam every explicit-sample-rate caller
    /// (playback rebuild, both export encoders) goes through
    /// ([`crate::AudioSession::open_multi_streams_speed_at`]). The per-segment
    /// conform chain resamples to whatever `meta` names, so the meta *is* the
    /// behaviour.
    #[test]
    fn a_picked_sample_rate_overrides_the_derived_one() {
        let file = asset("test_av.mp4");
        let sources = [(file, 0)];
        let segs = [(Some(0), 0., 1.)];
        let derived = crate::AudioSession::open_multi_streams_speed_at(&sources, &segs, &[], &[], None)
            .expect("open the fixture")
            .expect("the fixture has sound")
            .0
            .sample_rate;
        let picked =
            crate::AudioSession::open_multi_streams_speed_at(&sources, &segs, &[], &[], Some(32000))
                .expect("open the fixture")
                .expect("the fixture has sound")
                .0
                .sample_rate;
        assert_ne!(derived, 32000, "a fixture at the picked rate proves nothing");
        assert_eq!(picked, 32000);
    }

    /// Which source the timeline's sound is opened on -- the one rule
    /// [`open_project`](PlaybackSession::open_project), the probe every import
    /// is held to and the audio worker all read through. A still at index 0 is
    /// a picture, not a silent timeline: opening the device on it is a project
    /// that plays silent although every clip on it has sound.
    #[test]
    fn the_audio_reference_is_never_a_still() {
        let (video, still) = (asset("test_av.mp4"), asset("test_still.png"));
        let sources = [Source::new(&still, 0), Source::new(&video, 3)];
        assert_eq!(
            audio_source_of(&sources).map(|s| (&s.path, s.audio_stream)),
            Some((&sources[1].path, 3)),
            "the first source that could have a track, with its own stream"
        );
        assert!(
            audio_source_of(&sources[..1]).is_none(),
            "nothing but stills"
        );
        assert!(audio_source_of(&[]).is_none(), "and no file at all");
    }

    /// What a front end's "resolution/rate picked before any file is open"
    /// setting costs the engine: nothing new. `open` then `set_resolution`/
    /// `set_frame_rate` right after construction -- the app's technique for
    /// applying a pre-import pick to the session it just made, mirroring
    /// [`PlaybackSession::open_project`]'s own `doc.resolution`/`doc.fps`
    /// override -- lands exactly where either setter lands it whenever it is
    /// called, and a later `import` of another file leaves both untouched: an
    /// import registers a source and its length, and never writes `meta.width`,
    /// `meta.height` or `meta.frame_rate`.
    #[test]
    fn a_pre_import_setting_survives_construction_and_a_later_import() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        let native = session.native_resolution();
        let native_fps = session.native_frame_rate();
        // The explicit picks a pre-import setting would apply, distinct from
        // the fixture's own so a no-op would be caught.
        let (picked_w, picked_h) = (1920, 1080);
        assert_ne!((picked_w, picked_h), native, "the test needs a real change");
        let picked_fps = 24.0;
        assert_ne!(picked_fps, native_fps, "the test needs a real change");
        assert!(session.set_resolution(picked_w, picked_h));
        assert!(session.set_frame_rate(picked_fps));
        assert_eq!(session.resolution(), (picked_w, picked_h));
        assert_eq!(session.meta().frame_rate, picked_fps);

        // A second file joining the timeline is exactly the case the front end
        // worried a later import could silently override the explicit pick --
        // it cannot, because `import` never touches the canvas.
        session
            .import(&asset("test_av2.mp4"))
            .expect("av2 matches the timeline it was set to");
        assert_eq!(
            session.resolution(),
            (picked_w, picked_h),
            "an import must not move the project's own canvas size"
        );
        assert_eq!(
            session.meta().frame_rate, picked_fps,
            "an import must not move the project's own rate"
        );
    }

    /// The seam between the two lists a source index reaches: dropping a file
    /// from the library shortens `sources`, and `counts` -- the frame length
    /// per source, which is what a trim is walled by -- has to lose the same
    /// entry. Left behind, the dead file's length would wall the survivor.
    #[test]
    fn removing_a_source_takes_its_frame_count_with_it() {
        // Source 0 is 5 s, source 1 is 4 s: two different walls, so a stale
        // count cannot pass for the right one.
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        // Imported, then dragged onto the end -- an import alone places nothing
        // ([`PlaybackSession::import`]), and this test needs both takes there.
        session.import(&asset("test_av2.mp4")).expect("av2 matches");
        let end = session.timeline_duration();
        assert!(
            session
                .place_stream_at(end, &asset("test_av2.mp4"), 0, None)
                .expect("a file just imported is on this timeline")
        );
        let (long, short) = (session.counts[0], session.counts[1]);
        assert!(long > short, "{long} vs {short}");

        // Clear source 0's take, then take the file itself out: av2's clip is
        // the only one left and it is source 0 now.
        assert!(session.delete_clip(Lane::V1, 0));
        session
            .remove_source(&asset("test_av.mp4"), 0)
            .expect("nothing plays it any more");
        assert_eq!(session.counts, vec![short], "the dead length went with it");
        assert_eq!(session.sources().len(), session.counts.len());

        // The wall is the surviving file's own length, not the gone one's: a
        // whole-file clip cannot be dragged out any further at all.
        let clip = session.lane_clips(Lane::V1)[0];
        assert_eq!(clip.len(), short, "av2's take, whole");
        assert_eq!(
            session.trim_room(Lane::V1, 0, Edge::End),
            Some((clip.start + 1, clip.end()))
        );
        assert!(
            !session.trim_clip(Lane::V1, 0, Edge::End, clip.end() + 30),
            "past the file's last frame is refused, by ITS length"
        );

        // ...and the next import still lines the two lists up, which is the
        // alignment `note_frames` asserts on.
        session
            .import(&asset("test_av.mp4"))
            .expect("it may come back");
        assert_eq!(session.counts, vec![short, long]);
        assert_eq!(session.sources().len(), session.counts.len());
    }

    /// The two headers a place reads are read *once*: an mp4 dropped on a lane
    /// used to open its own container and the timeline's first source every
    /// time, both on the thread that draws, and a film whose pages have gone
    /// cold is 1.7 s of that.
    ///
    /// Proved by answering, not by timing: the memo is filled with a layout the
    /// file does not have, and the place is refused. Only a door that read the
    /// remembered header rather than the disk can refuse it.
    #[test]
    fn a_place_reads_each_audio_header_once() {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        session.set_gain(0.0);
        session.import(&asset("test_av2.mp4")).expect("av2 matches");
        let end = session.timeline_duration();
        assert!(
            session
                .place_stream_at(end, &asset("test_av2.mp4"), 0, None)
                .expect("a file just imported is on this timeline")
        );
        assert_eq!(
            session.probes.len(),
            2,
            "the timeline's first source and the file placed, once each"
        );

        // Now say av2 is 5.1. Nothing on disk changed, so a door that opens the
        // file still matches; one that trusts the memo refuses by layout.
        // Keyed exactly as a [`Source`] names a file -- canonical, symlinks
        // resolved -- which is the key the doors look up.
        let av2 = Source::new(asset("test_av2.mp4"), 0).path;
        session.probes.insert(
            (av2, 0),
            Some(crate::AudioProbe {
                sample_rate: 48_000,
                channels: 6,
            }),
        );
        let refused = session
            .place_stream_at(end, &asset("test_av2.mp4"), 0, None)
            .expect_err("the remembered layout disagrees with the timeline's");
        assert!(
            refused.to_string().contains("6 ch"),
            "refused in the layout's own words: {refused}"
        );
        assert_eq!(session.probes.len(), 2, "and nothing was opened to say so");
    }

    /// The door a library preview opens a chosen stream through: `test_multiaudio.mp4`
    /// has stream 0 stereo and stream 1 mono
    /// ([`crate::waveform::tests::each_stream_of_a_file_has_its_own_envelope`]
    /// names the same fixture), so a session that actually bound the stream it
    /// was asked for plays a different layout than one that did not -- and
    /// plain [`PlaybackSession::open`] still lands on stream 0, unchanged.
    #[test]
    fn open_with_audio_stream_binds_the_named_track() {
        let multi = asset("test_multiaudio.mp4");
        let zero = PlaybackSession::open_with_audio_stream(&multi, 0).expect("stream 0 opens");
        let one = PlaybackSession::open_with_audio_stream(&multi, 1).expect("stream 1 opens");
        let default = PlaybackSession::open(&multi).expect("default open");
        let (Some(a0), Some(a1), Some(ad)) =
            (zero.audio.as_ref(), one.audio.as_ref(), default.audio.as_ref())
        else {
            // No audio device on this machine: nothing further to check --
            // the device is the machine's business, not this door's.
            return;
        };
        assert_eq!(a0.channels, 2, "stream 0 is the stereo pair");
        assert_eq!(a1.channels, 1, "stream 1 is the mono track");
        assert_ne!(a0.channels, a1.channels, "the two streams must differ");
        assert_eq!(ad.channels, a0.channels, "open() still defaults to stream 0");
    }

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
