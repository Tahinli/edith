//! Background decode worker. Frames leave over a bounded channel so the caller's
//! thread (the UI thread) never blocks on decode and memory stays capped.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread;

use rusty_h264::Decoder;

use crate::color::ColorParams;
use crate::colorspace::{ColorDescription, Matrix, Transfer};
use crate::convert::{i420_to_bgra, i420_to_bgra_with};
use crate::demux::{Codec, Demuxer, VideoMeta};
use crate::hw::HwSession;
use crate::project::Speed;
use crate::scale::Composer;
use crate::tonemap::{self, ToneMapper};
use crate::transform::TransformParams;

/// One decoded picture, ready to hand to a renderer.
pub struct Frame {
    /// Position in decode order, starting at 0.
    pub index: u32,
    pub width: u32,
    pub height: u32,
    /// BGRA8, straight alpha, tightly packed.
    pub bgra: Vec<u8>,
}

/// Which decoder a source is really running on. Read-only introspection: it
/// decides nothing, it *reports* what [`DecodeSession::open_worker`] chose --
/// a hardware session that opened and then failed before its first picture
/// falls back to software and this says so, because it is written where the
/// fallback happens rather than where it was hoped for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// The worker has not reached its decoder yet: the plugin's VA-API init is
    /// ~90 ms, and this is what a caller reads in that window.
    #[default]
    Opening,
    /// The VA-API plugin (`libengine_hw.so`).
    Hardware,
    /// `rusty_h264`, in this process.
    Software,
    /// A still image: one `image` decode, no stream and no decoder to pick.
    Still,
    /// A gap: black frames, nothing decoded at all.
    Gap,
}

impl Backend {
    /// Short enough for a library row and a transport line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Opening => "opening",
            Self::Hardware => "HW",
            Self::Software => "SW",
            Self::Still => "still",
            Self::Gap => "gap",
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Opening => 0,
            Self::Hardware => 1,
            Self::Software => 2,
            Self::Still => 3,
            Self::Gap => 4,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Hardware,
            2 => Self::Software,
            3 => Self::Still,
            4 => Self::Gap,
            _ => Self::Opening,
        }
    }
}

/// Where a worker publishes the decoder it opened, for whoever holds the other
/// end of its channel. One atomic byte, written once at open (and once more if
/// hardware hands the range back to software), read whenever a caller repaints
/// -- never per frame on either side.
#[derive(Clone, Default)]
pub struct BackendCell(Arc<AtomicU8>);

impl BackendCell {
    pub(crate) fn new(backend: Backend) -> Self {
        Self(Arc::new(AtomicU8::new(backend.code())))
    }

    fn set(&self, backend: Backend) {
        self.0.store(backend.code(), Ordering::Relaxed);
    }

    pub fn get(&self) -> Backend {
        Backend::from_code(self.0.load(Ordering::Relaxed))
    }
}

/// Which decoder `path` would open, decided exactly as [`DecodeSession::open_worker`]
/// decides it: the same `VE_SW` pin and the same plugin probe against this very
/// file. What lets a front-end name the decoder *before* anything plays.
///
/// The codec is `None` for a still image, which has no coded stream. A stream
/// no decoder here can take is an `Err` naming the plugin, exactly as opening
/// it would be -- the same refusal, one VA-API init earlier.
///
/// Costs that one init (~90 ms) for a stream the plugin takes: ask it once per
/// file, off a render thread, and keep the answer.
pub fn probe(path: &Path) -> crate::Result<(Option<Codec>, Backend)> {
    if crate::is_image(path) {
        return Ok((None, Backend::Still));
    }
    let path = path.to_path_buf();
    let (meta, _demuxer) = Demuxer::open(&path)?;
    match hw_decodes(&path, 0, meta.codec) {
        Ok(()) => return Ok((Some(meta.codec), Backend::Hardware)),
        Err(e) if meta.codec != Codec::H264 => return Err(e.into()),
        Err(_) => {}
    }
    Ok((Some(meta.codec), Backend::Software))
}

/// A running decode worker: the flag that stops it, and the handle to wait on.
///
/// Dropping one cancels *and joins*. A worker abandoned mid-`vaInitialize`
/// outlives the process otherwise, and Mesa's `atexit` handlers free the state
/// it is still reading -- a SIGSEGV at exit, long after the last test passed.
pub(crate) struct Worker {
    cancel: Arc<AtomicBool>,
    /// `None` once detached, and for a range with nothing to decode: either way
    /// there is no thread left to wait for.
    handle: Option<thread::JoinHandle<()>>,
    /// How a *later* span reaches this same thread ([`Worker::reseek`]), for the
    /// workers that decode a file: the still, the gap and a detached worker have
    /// no such door and are the one-span threads they always were.
    reuse: Option<Reuse>,
}

/// The door a persistent decode worker is steered through: the file it has open,
/// the channel its next span arrives on, and the generation counter that tells
/// the span it is decoding *now* that nobody is waiting for it any more.
///
/// The point of all three is that a seek onto the same file costs a demuxer
/// seek and a decoder flush ([`crate::hw::HwSession::seek`]) rather than a
/// thread, a container parse and a VA-API initialisation (98 ms measured).
struct Reuse {
    path: PathBuf,
    spans: std::sync::mpsc::Sender<SpanCmd>,
    /// Bumped by every reseek and by [`Worker::abandon`]; the decode loop stops
    /// the moment it no longer matches the span it was handed.
    generation: Arc<AtomicU64>,
    /// The worker's decoder, published once and read across every span it
    /// decodes -- so a reseek onto a file already open says "hardware" from the
    /// first repaint instead of going back through `opening`.
    backend: BackendCell,
    /// Where the caller's playhead stands, in this file's own frames
    /// ([`Worker::playhead`]): the worker converts and sends nothing behind it.
    /// Rewritten to the target by every reseek, so it can never carry one span's
    /// position into the next.
    floor: Arc<AtomicU32>,
}

/// One span asked of a persistent worker: the range in the file's own frames,
/// everything a picture is graded, placed and mapped by, and the channel the
/// pictures go down. Carries its generation, so a command already superseded on
/// its way through the queue is dropped rather than decoded.
struct SpanCmd {
    generation: u64,
    start: u32,
    end: u32,
    color: ColorParams,
    transform: TransformParams,
    canvas: Composer,
    tone: tonemap::Preset,
    /// The clip's own playback speed ([`crate::project::Clip::speed`]), for
    /// [`skip_for_speed`]: at faster than real time several source frames in a
    /// row land on the same timeline frame, and only the last of a run is ever
    /// shown. [`Speed::NORMAL`] for a file-level open ([`DecodeSession::open`]
    /// and friends), which has no clip and skips nothing.
    speed: Speed,
    tx: SyncSender<Frame>,
}

/// What stops a decode loop: the worker's terminal flag (the thread itself is
/// going away) and the generation of the span it was handed (only *this* span is
/// going away). One check, so every loop treats them alike.
///
/// ...and where the playhead has reached ([`Abort::late`]), which is the same
/// question one picture at a time: a frame due before it is a frame nobody can
/// be shown any more.
struct Abort<'a> {
    cancel: &'a AtomicBool,
    generation: &'a AtomicU64,
    mine: u64,
    floor: &'a AtomicU32,
}

impl Abort<'_> {
    fn hit(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.generation.load(Ordering::Acquire) != self.mine
    }

    /// Whether the picture at source frame `index` is already past due: the
    /// caller's playhead has gone by it, so converting it (a full BGRA pass over
    /// 8 MB at 1080p) and queueing it would only push a picture nobody sees in
    /// front of the one they are waiting for.
    ///
    /// It is still *decoded* -- the pictures after it reference it -- and the
    /// floor only ever moves with the playhead the session publishes, so a
    /// paused session (and every seek, which republishes its own target) drops
    /// nothing at all.
    fn late(&self, index: u32) -> bool {
        self.floor.load(Ordering::Relaxed) > index
    }
}

/// Whether the picture at source frame `due` (of a span starting at `start` and
/// ending at `end`) is redundant *by construction* at `speed`, and its convert
/// and send can be skipped without ever waiting on the playhead
/// ([`Abort::late`]): at faster than real time several source frames in a row
/// map to the same timeline frame ([`Speed::timeline_at`]), and only the last
/// of such a run is ever shown -- the newest one always lands after the others
/// in `try_frame`'s stamp comparison. Unlike a late drop this needs no run
/// limit: the gap between two kept frames is at most `speed` source frames, so
/// the picture stream this leaves is never silent, only sparser.
///
/// The last frame of the whole span is never skipped by this: it is the
/// picture [`crate::project::Span::source_len`] was built to land exactly on,
/// and this is what proves that promise here instead of trusting it blind.
///
/// corner-cut: `due` and `start` are the file's own frame numbers, so this
/// is only correct when the file plays at the timeline's own rate
/// ([`crate::project::Rate::is_real_time`]) -- callers pass [`Speed::NORMAL`]
/// (never skip) otherwise. Upgrade path is composing `Rate` into this the way
/// [`crate::project::Span::timeline_at`] does.
fn skip_for_speed(speed: Speed, start: u32, due: u32, end: u32) -> bool {
    if speed.is_normal() || due + 1 >= end {
        return false;
    }
    let rel = due - start;
    speed.timeline_at(rel) == speed.timeline_at(rel + 1)
}

impl Worker {
    /// Stops the worker *and its thread* at their next check, without waiting.
    /// Callers that replace a worker should do this first and drop the receiver
    /// second: a worker parked in `send` only wakes on the disconnect, so the
    /// join below would otherwise wait for a consumer that is never coming.
    ///
    /// The command channel goes with it, which is what wakes a persistent worker
    /// parked between spans -- without that it would sit in `recv` forever and a
    /// retired worker would never be reaped (nor joined at exit in any bounded
    /// time, which is the Mesa `atexit` hazard this whole type exists for).
    pub(crate) fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.reuse = None;
    }

    /// Stops the span the worker is decoding, keeping the *thread* -- and with
    /// it the open file and the open decoder -- for the span that follows. What
    /// a seek does, since the next thing it says is [`Worker::reseek`].
    ///
    /// Falls back to [`cancel`](Self::cancel) for a worker with nothing to
    /// reuse (a still, a gap, a detached one), where the two mean the same
    /// thing: this worker has no next span.
    pub(crate) fn abandon(&mut self) {
        match &self.reuse {
            Some(reuse) => {
                reuse.generation.fetch_add(1, Ordering::Release);
            }
            None => self.cancel(),
        }
    }

    /// Tells the worker where the caller's playhead stands, in the file's own
    /// frames: it converts and queues nothing behind that, because a picture
    /// whose moment has passed cannot be shown and would only delay the one that
    /// can (see [`Abort::late`]). A worker with no reuse door -- a still, a gap
    /// -- has nothing to tell, and this costs it nothing to say.
    pub(crate) fn playhead(&self, source_frame: u32) {
        if let Some(reuse) = &self.reuse {
            reuse.floor.store(source_frame, Ordering::Relaxed);
        }
    }

    /// Points this worker at another range of the file it already has open,
    /// handing back the channel the new span's pictures arrive on and the cell
    /// that says what is decoding them. `None` when this worker cannot take the
    /// job -- another file, no reuse door, or a thread that has already returned
    /// -- and the caller then opens a worker of its own, exactly as it always
    /// did.
    ///
    /// The pictures of the *old* span are left in their own channel, which the
    /// caller drops: that disconnect is what wakes this worker if it is parked
    /// in `send`, and only then does it reach the command below. So the order at
    /// the call site is unchanged -- install the new receiver, and the old
    /// worker finds out by losing its consumer.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reseek(
        &mut self,
        path: &Path,
        start: u32,
        end: u32,
        color: ColorParams,
        transform: TransformParams,
        canvas: Composer,
        tone: tonemap::Preset,
        speed: Speed,
    ) -> Option<(Receiver<Frame>, BackendCell)> {
        let reuse = self.reuse.as_ref()?;
        if reuse.path != path || self.is_finished() {
            return None;
        }
        let (tx, rx) = sync_channel(canvas.queue_depth());
        // The new span's own playhead, before its first picture is decoded: a
        // floor left over from where the timeline *was* would drop every
        // picture of a seek that went backwards.
        reuse.floor.store(start, Ordering::Relaxed);
        // Bumped before the command is queued, so the span running now stops at
        // its next picture whatever order the two threads are scheduled in.
        let generation = reuse.generation.fetch_add(1, Ordering::Release) + 1;
        reuse
            .spans
            .send(SpanCmd {
                generation,
                start,
                end,
                color,
                transform,
                canvas,
                tone,
                speed,
                tx,
            })
            .ok()?;
        Some((rx, reuse.backend.clone()))
    }

    /// Whether the thread has already returned, i.e. dropping this would not
    /// wait. `true` for a worker that never had a thread. What lets a caller
    /// park cancelled workers and reap them without ever blocking.
    pub(crate) fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }

    /// Lets the worker run on unwatched -- what the public API does, since it
    /// hands out the flag alone.
    ///
    /// The reuse door goes with the handle: nothing can steer a worker nobody
    /// holds, and dropping the command channel is what makes such a thread end
    /// at its span rather than park waiting for a next one it can never be
    /// given -- with a VA-API session open, into Mesa's `atexit`.
    fn detach(mut self) -> Arc<AtomicBool> {
        self.handle = None;
        self.reuse = None;
        Arc::clone(&self.cancel)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // `cancel` drops the command channel too, so a worker parked between
            // spans wakes on the disconnect and this join is bounded.
            self.cancel();
            let _ = handle.join();
        }
    }
}

/// A running worker and the frames it is sending, handed out as one value
/// because they must be *dropped* as one, in this order: the receiver
/// disconnects first, which is the only thing that wakes a worker parked in a
/// full `send`, and the join inside [`Worker::drop`] runs second.
///
/// Rust drops fields in declaration order, so this struct *is* that rule, and
/// it holds wherever the value goes: an early return out of a half-built
/// session, an unwind, a caller not written yet. Two separate locals do not
/// hold it -- they drop in reverse order of declaration, so the worker there
/// joins a thread nobody will ever drain, which is a hang and not a slow path.
pub(crate) struct FrameStream {
    pub(crate) frames: Receiver<Frame>,
    pub(crate) worker: Worker,
    /// What this stream's pictures are being decoded by. Dropped last and read
    /// by the caller, so it outlives nothing and blocks nothing.
    pub(crate) backend: BackendCell,
}

pub struct DecodeSession;

impl DecodeSession {
    /// Opens `path`, returning stream metadata immediately and a receiver that
    /// yields frames as the worker decodes them. The worker stops when the
    /// receiver is dropped, at end of stream, or on the first decode error.
    pub fn open(path: impl AsRef<Path>) -> crate::Result<(VideoMeta, Receiver<Frame>)> {
        let (meta, rx, _cancel) = Self::open_at(path, 0)?;
        Ok((meta, rx))
    }

    /// As [`DecodeSession::open`], but the first frame delivered is
    /// `start_frame` (0-based display index; [`Frame::index`] stays absolute).
    /// Setting the returned flag stops the worker at its next access unit,
    /// which is how a caller abandons a session it no longer wants to drain.
    pub fn open_at(
        path: impl AsRef<Path>,
        start_frame: u32,
    ) -> crate::Result<(VideoMeta, Receiver<Frame>, Arc<AtomicBool>)> {
        // `u32::MAX` is clamped to the stream's frame count below, so this is
        // "to the end of the file".
        Self::open_range(path, start_frame, u32::MAX)
    }

    /// As [`DecodeSession::open_at`], but the worker also stops on its own
    /// after sending `end_frame - 1`: the range is half-open `[start, end)` in
    /// absolute source frames, which is what a clip of a longer file needs.
    /// `end_frame` is capped at the stream's frame count, and an empty range
    /// yields a receiver that is simply already disconnected.
    pub fn open_range(
        path: impl AsRef<Path>,
        start_frame: u32,
        end_frame: u32,
    ) -> crate::Result<(VideoMeta, Receiver<Frame>, Arc<AtomicBool>)> {
        // Ungraded: the public API opens a *file*, and a colour grade belongs to
        // a clip of a timeline. `PlaybackSession` is what has one to pass.
        let (meta, stream) = Self::open_worker(
            path,
            start_frame,
            end_frame,
            ColorParams::default(),
            // No clip, no transform either: the file-level API hands back the
            // pictures the file holds, untouched.
            TransformParams::default(),
            // No project, no canvas: the file-level API hands back the
            // pictures the file holds, at the size it holds them.
            Composer::passthrough(),
            // ...and no project means no picked rendition either: an HDR file
            // opened through this door is mapped the published way.
            tonemap::Preset::default(),
        )?;
        // The public API hands out the flag alone, so the worker is detached and
        // the receiver goes to the caller: nothing here joins anything.
        Ok((meta, stream.frames, stream.worker.detach()))
    }

    /// A worker that decodes nothing and emits `len` black frames, indexed from
    /// zero -- what a *gap* in the video lane looks like. It goes down the same
    /// channel as decoded frames, so a gap costs the caller no branch at all
    /// beyond choosing this opener: same bounded channel, same backpressure,
    /// same cancel, same retire path.
    pub(crate) fn open_black(width: u32, height: u32, len: u32) -> FrameStream {
        let (tx, rx) = sync_channel(2);
        let cancel = Arc::new(AtomicBool::new(false));
        if len == 0 {
            return FrameStream {
                frames: rx,
                worker: Worker {
                    cancel,
                    handle: None,
                    reuse: None,
                },
                backend: BackendCell::new(Backend::Gap),
            };
        }
        let worker_cancel = Arc::clone(&cancel);
        // One buffer, cloned per frame: opaque black is a constant, and the
        // clone is a memcpy against a decode this replaces entirely.
        let black = vec![[0u8, 0, 0, 255]; (width as usize) * (height as usize)].concat();
        let handle = thread::Builder::new()
            .name("black".into())
            .spawn(move || {
                for index in 0..len {
                    if worker_cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let frame = Frame {
                        index,
                        width,
                        height,
                        bgra: black.clone(),
                    };
                    if tx.send(frame).is_err() {
                        return; // caller moved on
                    }
                }
            })
            .ok();
        FrameStream {
            frames: rx,
            worker: Worker {
                cancel,
                handle,
                reuse: None,
            },
            backend: BackendCell::new(Backend::Gap),
        }
    }

    /// A worker for a still image ([`crate::is_image`]): the file is decoded
    /// *once* and the same picture goes down the channel `len` times, indexed
    /// from `start_frame` exactly as a decoder's pictures are -- so a placed
    /// image is a clip like any other to everything downstream (`try_frame`
    /// rewrites those indices, `next_clip` walks off the end of the range).
    ///
    /// Graded and placed on the canvas once too, before the repeat: a still is
    /// the same pixels every frame, so the whole cost of one is one conversion
    /// plus a memcpy per frame -- what [`open_black`](Self::open_black) pays.
    pub(crate) fn open_still(
        path: &Path,
        start_frame: u32,
        len: u32,
        color: ColorParams,
        transform: TransformParams,
        canvas: Composer,
    ) -> crate::Result<FrameStream> {
        let still = Still::open(path)?;
        let (tx, rx) = sync_channel(2);
        let cancel = Arc::new(AtomicBool::new(false));
        if len == 0 {
            return Ok(FrameStream {
                frames: rx,
                worker: Worker {
                    cancel,
                    handle: None,
                    reuse: None,
                },
                backend: BackendCell::new(Backend::Still),
            });
        }
        // The one conversion, on this thread: it is a few milliseconds and the
        // error it could raise has already been raised by `Still::open`.
        // BT.601 limited, because that is what `rgb_to_i420` above wrote these
        // planes in: a still round-trips through the pair, not through the
        // colour of whatever file is on the timeline beside it.
        // No preset argument, and none wanted: a still is decoded into BT.601
        // SDR planes, so the tone map is not built at all whichever rendition
        // the project is watching.
        let first = Render::new(
            color,
            transform,
            ColorDescription::default(),
            canvas,
            tonemap::Preset::default(),
            // ...and no declared peak either, for the same reason: there is no
            // tone map on this path to hand one to.
            None,
        )
        .frame(
            start_frame,
            &still.y,
            &still.u,
            &still.v,
            still.width,
            still.height,
        );
        let worker_cancel = Arc::clone(&cancel);
        let handle = thread::Builder::new()
            .name("still".into())
            .spawn(move || {
                let (width, height, bgra) = (first.width, first.height, first.bgra);
                for index in 0..len {
                    if worker_cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let frame = Frame {
                        index: start_frame + index,
                        width,
                        height,
                        bgra: bgra.clone(),
                    };
                    if tx.send(frame).is_err() {
                        return; // caller moved on
                    }
                }
            })
            .ok();
        Ok(FrameStream {
            frames: rx,
            worker: Worker {
                cancel,
                handle,
                reuse: None,
            },
            backend: BackendCell::new(Backend::Still),
        })
    }

    /// As [`DecodeSession::open_range`], but the caller gets the whole
    /// [`Worker`] and can therefore *wait* for it, and the range is graded by
    /// `color` -- the clip's own setting, folded into the conversion the frames
    /// go through anyway (see [`i420_to_bgra_with`]). It is captured here rather
    /// than read per frame because a worker *is* one clip's range: a grade
    /// changed while this one runs reaches the picture when the session reseeks
    /// onto the new one, which every edit does.
    ///
    /// In-process only: a caller that outlives its workers has to be able to
    /// join them at exit.
    ///
    /// `canvas` is the project's own resolution and this clip's fit policy: the
    /// frames come out at *that* size, whatever the file's is. A
    /// [`Composer::passthrough`] (or one already the source's size) leaves every
    /// picture exactly as it was decoded.
    ///
    /// `tone` is the project's HDR rendition ([`tonemap::Preset`]), captured
    /// here for the reason the grade is: it is constant across one span, and a
    /// preset picked while this worker runs reaches the picture when the session
    /// reseeks onto the new one. An SDR source ignores it entirely.
    pub(crate) fn open_worker(
        path: impl AsRef<Path>,
        start_frame: u32,
        end_frame: u32,
        color: ColorParams,
        transform: TransformParams,
        canvas: Composer,
        tone: tonemap::Preset,
    ) -> crate::Result<(VideoMeta, FrameStream)> {
        let path = path.as_ref().to_path_buf();
        let (meta, demuxer) = Demuxer::open(&path)?;
        // No software HEVC or VP9 decoder exists, so such a file the plugin will
        // not take is refused *here*, where the caller still has somewhere to
        // show it -- a worker that opened and then produced nothing is a black
        // screen with no explanation. The probe session is opened, made to
        // decode one picture and dropped: it costs one extra VA-API init
        // (~90 ms) plus that frame, and only off the H.264 path.
        if meta.codec != Codec::H264 {
            hw_decodes(&path, start_frame, meta.codec)?;
        }
        let end_frame = end_frame.min(meta.frame_count);
        if end_frame <= start_frame {
            // Nothing to decode: a disconnected receiver and no thread at all,
            // so the caller sees an immediate end of stream rather than an error.
            let (_, rx) = sync_channel(1);
            return Ok((
                meta,
                FrameStream {
                    frames: rx,
                    worker: Worker {
                        cancel: Arc::new(AtomicBool::new(false)),
                        handle: None,
                        reuse: None,
                    },
                    backend: BackendCell::default(),
                },
            ));
        }
        Ok((
            meta,
            span_worker(
                path,
                Some(Source::demuxed(demuxer, meta)),
                start_frame,
                end_frame,
                color,
                transform,
                canvas,
                tone,
                // A file-level open has no clip and no speed to skip frames
                // for: every picture in range is delivered, as it always was.
                Speed::NORMAL,
            ),
        ))
    }

    /// As [`open_worker`](Self::open_worker), but the *file* is opened on the
    /// worker too: nothing here touches the disk, so the caller pays a thread
    /// spawn and not a demux -- 550-750 ms warm on a 25 GB film, seconds cold,
    /// which is what a seek or a clip boundary used to cost the UI thread.
    ///
    /// The price is that there is no [`VideoMeta`] to return and no synchronous
    /// refusal either: a file that will not open, and a codec no decoder here
    /// takes (the plugin probe is skipped -- the worker's own refusal below
    /// covers it), simply drop the sender. The disconnected receiver is what
    /// carries a session on to the next span, which is exactly what a failed
    /// open did on this path before. Callers that must *tell* the user why --
    /// an import at the door -- keep the sync opener.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_worker_deferred(
        path: impl AsRef<Path>,
        start_frame: u32,
        end_frame: u32,
        color: ColorParams,
        transform: TransformParams,
        canvas: Composer,
        tone: tonemap::Preset,
        speed: Speed,
    ) -> FrameStream {
        span_worker(
            path.as_ref().to_path_buf(),
            None,
            start_frame,
            end_frame,
            color,
            transform,
            canvas,
            tone,
            speed,
        )
    }
}

/// Spawns a decode worker over `path` and gives it its first span. The thread
/// **outlives that span**: it parks on its command channel afterwards, so the
/// next seek onto the same file reaches it through [`Worker::reseek`] and costs
/// a demuxer seek and a decoder flush instead of a thread, a container parse and
/// a VA-API initialisation (98 ms measured worst case).
///
/// `opened` is the file when the caller already read it (the sync opener, which
/// demuxes on the caller's thread to have metadata to return); `None` is the
/// deferred one, where the worker opens it and nothing here touches the disk.
#[allow(clippy::too_many_arguments)]
fn span_worker(
    path: PathBuf,
    opened: Option<Source>,
    start_frame: u32,
    end_frame: u32,
    color: ColorParams,
    transform: TransformParams,
    canvas: Composer,
    tone: tonemap::Preset,
    speed: Speed,
) -> FrameStream {
    // The depth is the size of the pictures this worker will *emit*, which for a
    // pass-through canvas is the stream's own -- and the sync opener has already
    // read that on the caller's thread. Asking the canvas alone is how the file
    // door came up with 2: every session opens pass-through (a freshly opened
    // file *is* the project resolution), so a whole file would play at the
    // decode-ahead this engine had before there was one, until the first seek
    // built a real canvas.
    let depth = match (&opened, canvas.places_nothing()) {
        (Some(source), true) => crate::scale::queue_depth(source.meta.width, source.meta.height),
        _ => canvas.queue_depth(),
    };
    let (tx, rx) = sync_channel(depth);
    let cancel = Arc::new(AtomicBool::new(false));
    // Written by the worker the moment it knows, so a caller reading it sees
    // what opened rather than what was hoped for -- and kept across the spans
    // that follow, since they are decoded by that very decoder.
    let backend = BackendCell::default();
    let generation = Arc::new(AtomicU64::new(0));
    // The playhead starts where the span does: nothing is behind it yet.
    let floor = Arc::new(AtomicU32::new(start_frame));
    let (spans, commands) = std::sync::mpsc::channel();
    let first = SpanCmd {
        generation: 0,
        start: start_frame,
        end: end_frame,
        color,
        transform,
        canvas,
        tone,
        speed,
        tx,
    };
    let worker_cancel = Arc::clone(&cancel);
    let worker_backend = backend.clone();
    let worker_generation = Arc::clone(&generation);
    let worker_floor = Arc::clone(&floor);
    let worker_path = path.clone();
    let handle = thread::Builder::new()
        .name("decode".into())
        .spawn(move || {
            let mut source = opened;
            let mut cmd = first;
            loop {
                let abort = Abort {
                    cancel: &worker_cancel,
                    generation: &worker_generation,
                    mine: cmd.generation,
                    floor: &worker_floor,
                };
                // Superseded (or cancelled) before this thread was even
                // scheduled: do not open the file, and do not enter VA-API
                // initialisation, which is the one stretch a cancel cannot
                // interrupt. A scrub abandons spans by the dozen and this is
                // where all but the last of them stop.
                if !abort.hit() {
                    run_span(&worker_path, &mut source, cmd, &abort, &worker_backend);
                }
                // Parked between spans. A seek and a clip boundary arrive in
                // milliseconds and find the decoder still open, which is the
                // whole point of this thread -- but a session nobody comes back
                // to (the timeline played out, the window was left alone) must
                // not sit on the driver: it would hold a live libva thread into
                // whatever exit path the process takes, which is the SIGSEGV in
                // Mesa's `atexit` the retired pool exists to prevent, and
                // holding it *while idle* would widen that from "quit during a
                // decode" to "quit any time after opening a file".
                //
                // So the wait is in two halves: the fast one keeps everything,
                // and past [`IDLE`] the hardware session is closed. The demuxer
                // stays -- it is a file handle and an index, not driver state,
                // and rebuilding it is what costs seconds on a big Matroska --
                // so the reseek after a long pause pays one VA-API init and
                // nothing else.
                match commands.recv_timeout(IDLE) {
                    Ok(next) => cmd = next,
                    Err(RecvTimeoutError::Timeout) => {
                        if let Some(source) = &mut source
                            && source.hw.take().is_some()
                        {
                            eprintln!("decode worker idle: hardware session closed");
                        }
                        // The session dropped the command channel: this worker
                        // is retired (or was detached), so the file goes now.
                        match commands.recv() {
                            Ok(next) => cmd = next,
                            Err(_) => return,
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .ok();
    FrameStream {
        frames: rx,
        worker: Worker {
            cancel,
            handle,
            reuse: Some(Reuse {
                path,
                spans,
                generation,
                backend: backend.clone(),
                floor,
            }),
        },
        backend,
    }
}

/// How long a parked worker keeps its hardware session before closing it. Long
/// enough that a seek, a scrub step and a clip boundary all find the decoder
/// where they left it; short enough that a session left alone is not holding
/// libva when the process exits.
const IDLE: std::time::Duration = std::time::Duration::from_secs(2);

/// How many pictures in a row a worker may drop for being late before it hands
/// one over regardless. The policy needs a floor of its own: a machine that
/// cannot decode a file in real time is late on *every* picture, and dropping
/// every picture is a black screen rather than a stutter -- so the slowest seat
/// still gets one in nine, which is roughly what it could paint before (the
/// front-end restarted the decoder once every two seconds to the same effect,
/// and paid a VA-API init for each restart).
const LATE_RUN: u32 = 8;

/// What a decode worker keeps **between** the spans it is asked for: the
/// container it read, what the stream says about itself, and the hardware
/// session -- the three things a seek used to throw away and buy again.
struct Source {
    meta: VideoMeta,
    /// What the film says its brightest picture is ([`Demuxer::light`]): the
    /// tone map's assumed peak on the reference rendition, so a 1759 cd/m^2
    /// grade is converted at 1759 rather than at the 1000 a file that declared
    /// nothing is assumed at. A property of the stream, read once with it.
    peak: Option<f32>,
    demuxer: Demuxer,
    /// The plugin's session, opened on this thread (its VA-API state is not
    /// `Send`-safe across a hand-off) and *kept*. `None` for software: forced by
    /// `VE_SW`, refused by the plugin, or handed back by a session that opened
    /// and then decoded nothing.
    hw: Option<HwSession>,
}

impl Source {
    /// The file as the sync opener already read it.
    fn demuxed(demuxer: Demuxer, meta: VideoMeta) -> Self {
        Self {
            meta,
            peak: demuxer.light().peak(),
            demuxer,
            hw: None,
        }
    }

    /// ...and as the deferred opener reads it, on the worker. `None` is a file
    /// that would not open: the caller drops the sender, and the disconnected
    /// receiver is the session's "walk on to the next span".
    fn open(path: &Path) -> Option<Self> {
        match Demuxer::open(path) {
            Ok((meta, demuxer)) => Some(Self::demuxed(demuxer, meta)),
            Err(e) => {
                eprintln!("video open failed: {e}");
                None
            }
        }
    }

    /// Puts the hardware decoder at `start`, keeping the session open where the
    /// plugin can reposition it. `None` when there is no hardware here at all;
    /// otherwise whether the session was **reused** -- which is what lets the
    /// caller tell a driver that cannot decode this stream from one that merely
    /// did not follow a reseek.
    fn position_hw(&mut self, path: &Path, start: u32) -> Option<bool> {
        if let Some(hw) = &mut self.hw {
            if hw.seek(start) {
                eprintln!("decode backend: hardware (VA-API plugin, session kept, seek to {start})");
                return Some(true);
            }
            // A plugin too old to reposition, or a decoder that would not
            // flush: closed and opened again, which is what every seek did.
            self.hw = None;
        }
        self.hw = open_hw(path, start);
        self.hw.as_ref().map(|_| {
            eprintln!("decode backend: hardware (VA-API plugin)");
            false
        })
    }
}

/// One span on a worker's thread: position the decoder, decode
/// `[start, end)`, send the pictures. Every opener ends here -- the sync one,
/// which demuxed on the caller's thread, the deferred one, which demuxes on
/// this one, and every reseek after them -- so there is exactly one description
/// of what a decode worker does.
fn run_span(
    path: &Path,
    source: &mut Option<Source>,
    cmd: SpanCmd,
    abort: &Abort,
    backend: &BackendCell,
) {
    let SpanCmd {
        start,
        end,
        color,
        transform,
        canvas,
        tone,
        speed,
        tx,
        ..
    } = cmd;
    let opened = match source {
        Some(opened) => opened,
        None => {
            let Some(fresh) = Source::open(path) else {
                backend.set(Backend::Gap);
                return;
            };
            source.insert(fresh)
        }
    };
    // The clamp the sync opener does with the metadata it read on the caller's
    // thread; here the metadata arrives first.
    let end = end.min(opened.meta.frame_count);
    // An empty range decodes nothing and must send nothing: `run_hw` counts a
    // frame *out* before it compares, so without this a zero-length span would
    // emit one picture.
    if end <= start {
        return;
    }
    // The stream's own colour and peak brightness: properties of the stream, so
    // neither can change while one range decodes -- but the grade, the canvas
    // and the rendition are the *span's*, so this is built per span.
    let mut render = Render::new(color, transform, opened.meta.color, canvas, tone, opened.peak);
    if let Some(reused) = opened.position_hw(path, start) {
        // Cancelled during the init (or the seek): leave without decoding.
        if abort.hit() {
            return;
        }
        backend.set(Backend::Hardware);
        let mut decoded = run_hw(
            opened.hw.as_mut().expect("positioned above"),
            &tx,
            start,
            end,
            &mut render,
            abort,
            speed,
        );
        // A *reused* session that produced nothing may simply be one this
        // reseek left where the driver would not follow; a session opened fresh
        // at the same frame is the honest second question, and only its silence
        // is a software fallback. Without this a single unfollowed seek would
        // collapse the rest of the file to software.
        if !decoded && reused {
            eprintln!("hardware decode produced nothing after a reseek; reopening at frame {start}");
            opened.hw = open_hw(path, start);
            if let Some(hw) = opened.hw.as_mut() {
                decoded = run_hw(hw, &tx, start, end, &mut render, abort, speed);
            }
        }
        if decoded {
            return;
        }
        // A driver that opens but cannot decode a single frame is still a
        // fallback case, not a dead session.
        opened.hw = None;
        eprintln!("hardware decode failed before any frame, falling back to software");
    }
    // ...except where there is nothing to fall back to. Feeding HEVC or VP9
    // bytes to `rusty_h264` would be garbage, not a fallback.
    if opened.meta.codec != Codec::H264 {
        eprintln!("{}", opened.meta.codec.needs_plugin());
        return;
    }
    eprintln!("decode backend: software (rusty_h264)");
    backend.set(Backend::Software);
    run(&mut opened.demuxer, &tx, start, end, &mut render, abort, speed)
}

/// One clip's pictures on their way to the renderer: graded, placed on the
/// project canvas, converted. Both decode loops go through it, so hardware and
/// software cannot show two different pictures.
///
/// The order is grade, then place (see [`Composer`]): a grade is the clip's own,
/// applied at the resolution it was shot at, and the letterbox bars around it
/// are not the clip -- a brightness grade must not lift them off black.
///
/// A project at the media's own resolution never reaches any of that: it takes
/// the fused graded conversion, the one path this engine had before there was a
/// project resolution, byte for byte and allocation for allocation.
struct Render {
    color: ColorParams,
    /// This clip's placement, on top of whatever the canvas's own fit policy
    /// already does ([`crate::scale::Composer::place_transformed`]).
    transform: TransformParams,
    /// What the source's samples mean -- the stream's own matrix and range,
    /// which is what the conversion below is done in rather than the BT.601 it
    /// used to assume of every file.
    desc: ColorDescription,
    canvas: Composer,
    /// This stream's HDR-to-SDR map, built once here because building it is
    /// float work over 9 537 grid nodes and the transfer is a property of the
    /// stream. `None` for an SDR stream, which is what keeps that path exactly
    /// the one it was.
    tone: Option<ToneMapper>,
    /// The working copy of the source planes, refilled per frame and kept
    /// across the worker's whole range. Empty unless the clip needs one: it is
    /// tone-mapped in place, graded in place, or both.
    graded: (Vec<u8>, Vec<u8>, Vec<u8>),
    /// Where [`Composer::place_transformed`]'s owned picture lands, so a
    /// transformed frame's planes live as long as `self` -- matching what
    /// [`Composer::place`] already hands back by borrowing `self.canvas`.
    placed: (Vec<u8>, Vec<u8>, Vec<u8>),
}

/// What [`Render::frame`] converts a tone-mapped picture in: the tone map's
/// output contract ([`crate::tonemap`]), and no longer the HDR space the file
/// declared.
const TONE_MAPPED: ColorDescription = ColorDescription {
    matrix: Matrix::Bt709,
    transfer: Transfer::Sdr,
    full_range: false,
};

impl Render {
    /// `peak` is what the file declared about its own brightness
    /// ([`Demuxer::light`]), which only an HDR stream has and only the
    /// reference rendition reads (see [`tonemap::Preset`]).
    fn new(
        color: ColorParams,
        transform: TransformParams,
        desc: ColorDescription,
        canvas: Composer,
        preset: tonemap::Preset,
        peak: Option<f32>,
    ) -> Self {
        Self {
            color,
            transform,
            desc,
            canvas,
            // corner-cut: the ceiling is that the map reads limited-range codes
            // (`tonemap`'s input contract), so a full-range PQ file -- which
            // no encoder writes and no camera produces -- is mapped as if it
            // were limited and comes out slightly crushed. The upgrade is a
            // range argument to `ToneMapper::new`.
            tone: match desc.transfer {
                Transfer::Sdr => None,
                Transfer::Pq => Some(ToneMapper::new(tonemap::Transfer::Pq, preset, peak)),
                Transfer::Hlg => Some(ToneMapper::new(tonemap::Transfer::Hlg, preset, peak)),
            },
            graded: (Vec::new(), Vec::new(), Vec::new()),
            placed: (Vec::new(), Vec::new(), Vec::new()),
        }
    }

    fn frame(
        &mut self,
        index: u32,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        width: u32,
        height: u32,
    ) -> Frame {
        let passthrough = self.canvas.is_passthrough(width, height);
        // A transform still has a picture to place even where the canvas
        // itself would hand this size back untouched -- moving, scaling,
        // rotating or cropping a passthrough-sized clip is still a placement,
        // and must not be silently dropped by the fused conversion below.
        let transform_active = !self.transform.is_identity();
        let skip_placement = passthrough && !transform_active;
        // Everything that has to happen to the samples before the canvas sees
        // them, on one copy: the tone map (HDR streams only) and, unless the
        // conversion below can fuse it, the grade. An SDR stream that is either
        // ungraded or passing through takes neither and never allocates.
        //
        // The grade lands *after* the map on purpose: it is a look on the
        // picture a viewer is shown, so its brightness and saturation mean what
        // they say in the SDR the tone map just produced, not in the 10-stop
        // HDR the file was in.
        let (y, u, v, desc) = if self.tone.is_some()
            || (!skip_placement && !self.color.is_identity())
        {
            let (gy, gu, gv) = &mut self.graded;
            gy.clear();
            gy.extend_from_slice(y);
            gu.clear();
            gu.extend_from_slice(u);
            gv.clear();
            gv.extend_from_slice(v);
            if let Some(tone) = &self.tone {
                tone.map(gy, gu, gv, width as usize, height as usize);
            }
            if !passthrough && !self.color.is_identity() {
                crate::color::apply_yuv(&self.color, gy, gu, gv);
            }
            let desc = if self.tone.is_some() {
                TONE_MAPPED
            } else {
                self.desc
            };
            (&gy[..], &gu[..], &gv[..], desc)
        } else {
            (y, u, v, self.desc)
        };
        if skip_placement {
            return Frame {
                index,
                width,
                height,
                bgra: i420_to_bgra_with(
                    &desc,
                    &self.color,
                    y,
                    u,
                    v,
                    width as usize,
                    height as usize,
                ),
            };
        }
        let (y, u, v, width, height) = if transform_active {
            let (py, pu, pv, w, h) = self
                .canvas
                .place_transformed(y, u, v, width, height, &self.transform);
            self.placed = (py, pu, pv);
            (&self.placed.0[..], &self.placed.1[..], &self.placed.2[..], w, h)
        } else {
            self.canvas.place(y, u, v, width, height)
        };
        Frame {
            index,
            width,
            height,
            // Ungraded on purpose: the grade is already in the pixels above and
            // the canvas around them must stay the black it was filled with.
            bgra: i420_to_bgra(&desc, y, u, v, width as usize, height as usize),
        }
    }
}

/// One still image, decoded to tightly packed I420 -- the shape every other
/// source arrives in, so a placed image goes through the same grade, the same
/// canvas and the same conversion as a decoded picture and there is no second
/// pixel path to keep in step.
///
/// Held whole rather than streamed: an image *is* one picture, and the file is
/// read once per span that plays it.
pub(crate) struct Still {
    pub(crate) y: Vec<u8>,
    pub(crate) u: Vec<u8>,
    pub(crate) v: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Still {
    /// Decodes `path`. Refused by name for a picture that is not one
    /// ([`crate::is_resolution`]): an 8K limit that a keystroke and a project
    /// file are both held to cannot be walked around by a 30000-pixel-wide PNG.
    ///
    /// Alpha is *dropped*, not composited: `to_rgb8` discards the channel, so a
    /// transparent PNG arrives fully opaque over its own colours rather than
    /// over the clip beneath it. I420 has nowhere to carry it and this engine
    /// composes one picture per frame (topmost lane wins), so there is nothing
    /// for a blend to blend with.
    ///
    /// corner-cut: the ceiling is a logo with a transparent background, which
    /// lands as a rectangle. Upgrade path is keeping the alpha plane here and
    /// giving `scale::Composer` a blend over the lane below.
    pub(crate) fn open(path: &Path) -> crate::Result<Self> {
        let reader = image::ImageReader::open(path)?.with_guessed_format()?;
        let rgb = reader
            .decode()
            .map_err(|e| format!("{}: {e}", path.display()))?
            .to_rgb8();
        let (width, height) = rgb.dimensions();
        if !crate::is_resolution(width, height) {
            return Err(format!(
                "{} is {width}x{height}, which is not a picture this engine composes",
                path.display()
            )
            .into());
        }
        let (y, u, v) = rgb_to_i420(&rgb, width as usize, height as usize);
        Ok(Self {
            y,
            u,
            v,
            width,
            height,
        })
    }

    /// The planes as an export's decoder hands them over, so a still and a
    /// decoded picture are the same value to `export::run`.
    pub(crate) fn picture(&self) -> (&[u8], &[u8], &[u8], u32, u32) {
        (&self.y, &self.u, &self.v, self.width, self.height)
    }
}

/// How big the picture in `path` is, from its header alone -- what a library row
/// says about a file it has not placed anywhere. The whole file is not decoded.
pub fn image_size(path: &Path) -> crate::Result<(u32, u32)> {
    Ok(image::ImageReader::open(path)?
        .with_guessed_format()?
        .into_dimensions()
        .map_err(|e| format!("{}: {e}", path.display()))?)
}

/// Packed RGB8 -> tightly packed I420, BT.601 limited range: the exact inverse
/// of [`crate::convert::i420_to_bgra`] *in that matrix*, which is why
/// [`DecodeSession::open_still`] hands the renderer a BT.601 description and
/// not the timeline's. A still has no stream to be tagged by; it round-trips to
/// the colour it was authored in through this pair and nothing else sees these
/// planes (`images::still_pixels_match_the_source_image` measures it).
///
/// Chroma is the 2x2 box average, and an odd edge averages the samples it has --
/// the planes come out `(w + 1) / 2` by `(h + 1) / 2`, which is the shape
/// [`crate::scale::Composer::place`] panics on anything else for.
fn rgb_to_i420(rgb: &[u8], width: usize, height: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (cw, ch) = crate::scale::chroma_dims(width, height);
    let mut y = vec![0u8; width * height];
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for row in 0..height {
        for col in 0..width {
            let p = (row * width + col) * 3;
            let (r, g, b) = (rgb[p] as i32, rgb[p + 1] as i32, rgb[p + 2] as i32);
            y[row * width + col] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        }
    }
    for crow in 0..ch {
        for ccol in 0..cw {
            let (mut sr, mut sg, mut sb, mut n) = (0i32, 0i32, 0i32, 0i32);
            for row in crow * 2..(crow * 2 + 2).min(height) {
                for col in ccol * 2..(ccol * 2 + 2).min(width) {
                    let p = (row * width + col) * 3;
                    sr += rgb[p] as i32;
                    sg += rgb[p + 1] as i32;
                    sb += rgb[p + 2] as i32;
                    n += 1;
                }
            }
            let n = n.max(1);
            let (r, g, b) = (sr / n, sg / n, sb / n);
            u[crow * cw + ccol] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            v[crow * cw + ccol] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
        }
    }
    (y, u, v)
}

/// `None` when the software path must be used: forced by `VE_SW=1`, or the
/// plugin is absent/broken/unable to open this particular file.
fn open_hw(path: &Path, start_frame: u32) -> Option<HwSession> {
    if std::env::var_os("VE_SW").is_some_and(|v| v == "1") {
        return None;
    }
    HwSession::open_at(path, start_frame)
}

/// `Ok` when the plugin really decodes this file: it opens *and* hands back a
/// first picture. Opening only reads the container -- a stream the vendored
/// bitstream parser dies on (a 2160p wavefront HEVC did, for one) opens fine and
/// then produces nothing, which reaches the user as a black picture with an
/// instant `eof` unless the door asks for that first frame. Costs one decoded
/// frame per probe.
///
/// The `Err` says which half failed, because "there is no decoder for this
/// codec" and "the decoder here would not take this stream" are different
/// refusals and a user acts on them differently.
fn hw_decodes(path: &PathBuf, start_frame: u32, codec: Codec) -> Result<(), String> {
    let Some(mut hw) = open_hw(path, start_frame) else {
        return Err(codec.needs_plugin());
    };
    // End of stream counts as a refusal, not as a pass: a session that hands
    // back no picture at all is exactly the black-frame-and-instant-eof this
    // probe exists to catch, and `Ok(None)` is how the plugin says it.
    match hw.next_frame() {
        Ok(Some(_)) => Ok(()),
        _ => Err(codec.undecodable()),
    }
}

/// Returns whether the session was handled; `false` only when hardware decode
/// failed without emitting a single frame, so software can still take over.
///
/// The plugin already positioned itself at `start_frame`, so indices are
/// counted from there (a stream whose very first sample is not a sync sample
/// cannot be decoded from its start at all, and is not accounted for).
fn run_hw(
    hw: &mut HwSession,
    tx: &SyncSender<Frame>,
    start_frame: u32,
    end_frame: u32,
    render: &mut Render,
    abort: &Abort,
    speed: Speed,
) -> bool {
    let mut index = start_frame;
    // Pictures dropped for being late since the last one handed over; see
    // [`LATE_RUN`].
    let mut skipped = 0;
    loop {
        if abort.hit() {
            return true;
        }
        match hw.next_frame() {
            Ok(Some((y, u, v, width, height))) => {
                let due = index;
                index += 1;
                if abort.late(due) {
                    if skipped < LATE_RUN {
                        skipped += 1;
                    } else {
                        skipped = 0;
                        let frame = render.frame(due, y, u, v, width, height);
                        if tx.send(frame).is_err() {
                            return true; // consumer went away
                        }
                    }
                } else if skip_for_speed(speed, start_frame, due, end_frame) {
                    // Redundant by construction, not by lateness: no run limit
                    // needed (see `skip_for_speed`).
                } else {
                    skipped = 0;
                    let frame = render.frame(due, y, u, v, width, height);
                    if tx.send(frame).is_err() {
                        return true; // consumer went away
                    }
                }
                if index >= end_frame {
                    return true; // end of the requested range
                }
            }
            Ok(None) => return true,
            Err(e) => {
                if index == start_frame {
                    return false;
                }
                eprintln!("hardware decode error at frame {index}: {e}");
                return true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    /// Drops `path` from the page cache, so the open below really goes to disk
    /// -- the case the deferred opener exists for, and the one that costs
    /// seconds on a 25 GB film. Unprivileged (`posix_fadvise(DONTNEED)`), and a
    /// no-op wherever `python3` is not around: the assertions hold either way,
    /// the numbers printed are simply the warm ones.
    fn evict(path: &Path) {
        let _ = std::process::Command::new("python3")
            .arg("-c")
            .arg(
                "import os,sys\n\
                 fd=os.open(sys.argv[1],os.O_RDONLY)\n\
                 os.posix_fadvise(fd,0,0,os.POSIX_FADV_DONTNEED)\n\
                 os.close(fd)",
            )
            .arg(path)
            .status();
    }

    /// The whole point of [`DecodeSession::open_worker_deferred`]: the caller
    /// gets its stream back in the time a thread takes to spawn, and the file
    /// is opened behind it -- so the first frame necessarily arrives *after*
    /// the call returned, and the sync opener on the same cold file pays on the
    /// calling thread what this one does not.
    #[test]
    fn a_deferred_open_costs_the_caller_a_thread_and_not_a_demux() {
        let path = asset("test_baseline.mp4");
        evict(&path);
        let start = Instant::now();
        let stream = DecodeSession::open_worker_deferred(
            &path,
            0,
            u32::MAX,
            ColorParams::default(),
            TransformParams::default(),
            Composer::passthrough(),
            tonemap::Preset::default(),
            Speed::NORMAL,
        );
        let returned = start.elapsed();
        let frame = stream
            .frames
            .recv_timeout(Duration::from_secs(10))
            .expect("first frame");
        let first_frame = start.elapsed();

        // The same file, equally cold, through the door the import path keeps:
        // that one hands back metadata, and the demux it reads to do so is on
        // this thread.
        evict(&path);
        let sync_start = Instant::now();
        let sync = DecodeSession::open_worker(
            &path,
            0,
            u32::MAX,
            ColorParams::default(),
            TransformParams::default(),
            Composer::passthrough(),
            tonemap::Preset::default(),
        )
        .expect("sync open");
        let sync_returned = sync_start.elapsed();
        drop(sync);

        eprintln!(
            "deferred open returned in {returned:?}, first frame at {first_frame:?}; \
             sync open returned in {sync_returned:?}"
        );
        assert_eq!(frame.index, 0, "the deferred worker starts where it is told");
        assert!(
            returned < Duration::from_millis(10),
            "the caller waited {returned:?} -- something is still being read \
             from the file before this returns"
        );
        assert!(
            first_frame > returned,
            "the first frame arrived within the call ({first_frame:?} vs \
             {returned:?}), which cannot happen if the open is deferred"
        );
        // No `returned < sync_returned` here on purpose: on a loaded machine
        // a thread spawn (what the deferred open pays) can cost more wall
        // clock than a whole sync demux of a small fixture, and the race
        // flaked one run in three under a parallel build. The two asserts
        // above already pin the invariant -- the call returns in microseconds
        // territory and the demux happens after it, on the worker.
        let _ = sync_returned;
    }

    /// The late-picture policy, at the one place it is decided. A worker told
    /// the playhead is already at frame 20 must not spend a conversion and a
    /// queue slot on the twenty pictures behind it -- they can never be shown --
    /// and it must not go silent either, which is what [`LATE_RUN`] bounds: a
    /// machine that cannot decode in real time is late on every picture, and one
    /// in nine still reaches the screen.
    ///
    /// Deterministic on purpose: the floor is set by hand rather than by a clock
    /// that would have to outrun a debug-build decoder to prove anything.
    #[test]
    fn a_worker_skips_the_pictures_the_playhead_has_gone_past() {
        let take = |floor: u32| {
            let stream = DecodeSession::open_worker_deferred(
                asset("test_baseline.mp4"),
                0,
                u32::MAX,
                ColorParams::default(),
                TransformParams::default(),
                Composer::passthrough(),
                tonemap::Preset::default(),
                Speed::NORMAL,
            );
            stream.worker.playhead(floor);
            let mut indices = Vec::new();
            while indices.len() < 4 {
                let frame = stream
                    .frames
                    .recv_timeout(Duration::from_secs(10))
                    .expect("a picture");
                indices.push(frame.index);
            }
            indices
        };

        // Nobody has moved: every picture of the range is owed, in order.
        assert_eq!(take(0), vec![0, 1, 2, 3]);

        // The playhead is at 20, so the pictures before it are decoded (the ones
        // after reference them) and dropped unconverted -- all but the one in
        // nine [`LATE_RUN`] lets through, so the caller never goes blind.
        let caught_up = take(20);
        assert!(
            caught_up[0] > 0 && caught_up.windows(2).any(|w| w[1] > w[0] + 1),
            "nothing was skipped: {caught_up:?}"
        );
        assert!(
            caught_up.iter().any(|&i| i >= 20),
            "the playhead was never reached: {caught_up:?}"
        );
        assert!(
            caught_up.len() >= 4,
            "the worker went silent instead of dropping down to one in nine"
        );
    }

    /// [`skip_for_speed`] on its own, with no worker or channel involved: at
    /// 2x every other source frame maps onto the same timeline frame
    /// ([`Speed::timeline_at`]), so `skip`/`keep` must alternate exactly, and
    /// the last frame of the span (the one [`crate::project::Span::source_len`]
    /// is built to land on) must never be skipped regardless of where the
    /// alternation lands.
    #[test]
    fn skip_for_speed_alternates_at_2x_and_never_drops_the_last_frame() {
        let speed = Speed::from_permille(2000);
        let start = 100;
        let end = 110;

        let skips: Vec<bool> = (start..end)
            .map(|due| skip_for_speed(speed, start, due, end))
            .collect();
        // Alternation holds up to (but not including) the span's last frame,
        // which is special-cased below.
        for pair in (start..end - 2).zip(skips.windows(2)) {
            let (due, w) = pair;
            assert_eq!(
                w[0], !w[1],
                "frame {due} and its successor should alternate skip/keep at 2x: {skips:?}"
            );
        }
        assert!(
            !skip_for_speed(speed, start, end - 1, end),
            "the last frame of the span must never be skipped"
        );
    }

    /// Dropping a [`FrameStream`] whose frames nobody ever took must not wait.
    /// The channel is bounded, so such a worker is parked in `send` within a
    /// few frames, and the only thing that can wake it is its receiver going
    /// away -- which is why the receiver is the first field and the join inside
    /// `Worker::drop` is the second. Swap those two fields and this hangs.
    ///
    /// This is the whole basis of every teardown path in the engine, so it is
    /// asserted rather than commented: the drop runs on a second thread and a
    /// regression fails in ten seconds instead of hanging the suite forever.
    #[test]
    fn dropping_a_frame_stream_never_waits_for_an_undrained_channel() {
        let (_meta, stream) = DecodeSession::open_worker(
            &asset("test_baseline.mp4"),
            0,
            u32::MAX,
            ColorParams::default(),
            TransformParams::default(),
            Composer::passthrough(),
            tonemap::Preset::default(),
        )
        .expect("open");
        // Nothing is ever received: two frames fill the channel and the next
        // send parks the decode thread, exactly as a refused project load does.
        thread::sleep(Duration::from_millis(500));

        let (done_tx, done_rx) = sync_channel(1);
        thread::spawn(move || {
            drop(stream);
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)) != Err(RecvTimeoutError::Timeout),
            "dropping a parked stream deadlocked: it joined a decode thread \
             while still holding the receiver that thread is waiting on"
        );
    }
}

fn run(
    demuxer: &mut Demuxer,
    tx: &SyncSender<Frame>,
    start_frame: u32,
    end_frame: u32,
    render: &mut Render,
    abort: &Abort,
    speed: Speed,
) {
    // A decoder per span, and cheap: it is a parameter-set map and a picture
    // buffer, while the *demuxer* -- whose index cost seconds to build -- is the
    // one this worker keeps.
    let mut decoder = Decoder::new();
    // Decoding has to restart at a sync sample, so pictures between it and
    // `start_frame` are decoded (the target frame references them) but never
    // converted or sent. Signed, because the landing sync sample can sit inside
    // what the file's edit list trims, i.e. *before* frame 0.
    let mut index = demuxer.seek_to_sync_at_or_before(start_frame);
    let mut skipped = 0;

    // corner-cut: emits pictures in decode order. Fine for Baseline (no B-frames);
    // reordering streams need POC-sorted output before display.
    loop {
        if abort.hit() {
            break;
        }
        let au = match demuxer.next_access_unit() {
            Ok(Some(au)) => au,
            Ok(None) => break,
            Err(e) => {
                eprintln!("demux error: {e}");
                break;
            }
        };
        let yuv = match decoder.decode(&au) {
            Ok(Some(yuv)) => yuv,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("decode error at access unit {index}: {e}");
                break;
            }
        };
        if index < i64::from(start_frame) {
            index += 1;
            continue;
        }
        let due = index as u32;
        index += 1;
        if abort.late(due) {
            if skipped < LATE_RUN {
                skipped += 1;
            } else {
                skipped = 0;
                let frame = render.frame(
                    due,
                    &yuv.y,
                    &yuv.u,
                    &yuv.v,
                    yuv.width as u32,
                    yuv.height as u32,
                );
                if tx.send(frame).is_err() {
                    break; // consumer went away
                }
            }
        } else if skip_for_speed(speed, start_frame, due, end_frame) {
            // Redundant by construction, not by lateness: no run limit needed
            // (see `skip_for_speed`).
        } else {
            skipped = 0;
            let frame = render.frame(
                due,
                &yuv.y,
                &yuv.u,
                &yuv.v,
                yuv.width as u32,
                yuv.height as u32,
            );
            if tx.send(frame).is_err() {
                break; // consumer went away
            }
        }
        if index >= i64::from(end_frame) {
            break; // end of the requested range
        }
    }
}
