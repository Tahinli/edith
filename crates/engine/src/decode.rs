//! Background decode worker. Frames leave over a bounded channel so the caller's
//! thread (the UI thread) never blocks on decode and memory stays capped.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;

use rusty_h264::Decoder;

use crate::color::ColorParams;
use crate::convert::{i420_to_bgra, i420_to_bgra_with};
use crate::demux::{Codec, Demuxer, VideoMeta};
use crate::hw::HwSession;
use crate::scale::Composer;

/// One decoded picture, ready to hand to a renderer.
pub struct Frame {
    /// Position in decode order, starting at 0.
    pub index: u32,
    pub width: u32,
    pub height: u32,
    /// BGRA8, straight alpha, tightly packed.
    pub bgra: Vec<u8>,
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
}

impl Worker {
    /// Stops the worker at its next check without waiting. Callers that replace
    /// a worker should do this first and drop the receiver second: a worker
    /// parked in `send` only wakes on the disconnect, so the join below would
    /// otherwise wait for a consumer that is never coming.
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
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
    fn detach(mut self) -> Arc<AtomicBool> {
        self.handle = None;
        Arc::clone(&self.cancel)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
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
            // No project, no canvas: the file-level API hands back the
            // pictures the file holds, at the size it holds them.
            Composer::passthrough(),
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
                },
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
            worker: Worker { cancel, handle },
        }
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
    pub(crate) fn open_worker(
        path: impl AsRef<Path>,
        start_frame: u32,
        end_frame: u32,
        color: ColorParams,
        canvas: Composer,
    ) -> crate::Result<(VideoMeta, FrameStream)> {
        let path = path.as_ref().to_path_buf();
        let (meta, demuxer) = Demuxer::open(&path)?;
        // No software HEVC or VP9 decoder exists, so such a file the plugin will
        // not take is refused *here*, where the caller still has somewhere to
        // show it -- a worker that opened and then produced nothing is a black
        // screen with no explanation. The probe session is opened and dropped:
        // it costs one extra VA-API init (~90 ms) and only off the H.264 path.
        if meta.codec != Codec::H264 && open_hw(&path, start_frame).is_none() {
            return Err(meta.codec.needs_plugin().into());
        }
        let end_frame = end_frame.min(meta.frame_count);
        // Small bound: a 720p BGRA frame is ~3.5 MB, so we must not let the
        // decoder run ahead of the display without limit.
        let (tx, rx) = sync_channel(2);
        let cancel = Arc::new(AtomicBool::new(false));
        if end_frame <= start_frame {
            // Nothing to decode: dropping `tx` here closes the channel cleanly,
            // so the caller sees an immediate end of stream rather than an error.
            return Ok((
                meta,
                FrameStream {
                    frames: rx,
                    worker: Worker {
                        cancel,
                        handle: None,
                    },
                },
            ));
        }
        let worker_cancel = Arc::clone(&cancel);
        let handle = thread::Builder::new()
            .name("decode".into())
            .spawn(move || {
                // Cancelled before this thread was even scheduled -- do not
                // enter VA-API initialisation at all, because that is the one
                // stretch a cancel cannot interrupt (~65-90 ms of driver setup
                // that would then have to be torn down again).
                if worker_cancel.load(Ordering::Relaxed) {
                    return;
                }
                let mut render = Render::new(color, canvas);
                // The plugin has to be opened on the thread that uses it: its
                // VA-API state is not `Send`-safe across a later hand-off.
                if let Some(hw) = open_hw(&path, start_frame) {
                    // Cancelled *during* init: close the session (dropping `hw`
                    // does it) and leave without decoding anything.
                    if worker_cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    eprintln!("decode backend: hardware (VA-API plugin)");
                    if run_hw(hw, &tx, start_frame, end_frame, &mut render, &worker_cancel) {
                        return;
                    }
                    // A driver that opens but cannot decode a single frame is
                    // still a fallback case, not a dead session.
                    eprintln!("hardware decode failed before any frame, falling back to software");
                }
                // ...except where there is nothing to fall back to. Feeding HEVC
                // or VP9 bytes to `rusty_h264` would be garbage, not a fallback.
                if meta.codec != Codec::H264 {
                    eprintln!("{}", meta.codec.needs_plugin());
                    return;
                }
                eprintln!("decode backend: software (rusty_h264)");
                run(
                    demuxer,
                    tx,
                    start_frame,
                    end_frame,
                    &mut render,
                    &worker_cancel,
                )
            })?;
        Ok((
            meta,
            FrameStream {
                frames: rx,
                worker: Worker {
                    cancel,
                    handle: Some(handle),
                },
            },
        ))
    }
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
    canvas: Composer,
    /// The graded copy of the source planes, refilled per frame and kept across
    /// the worker's whole range. Empty unless the clip is both graded *and*
    /// placed, which is the only case that needs it.
    graded: (Vec<u8>, Vec<u8>, Vec<u8>),
}

impl Render {
    fn new(color: ColorParams, canvas: Composer) -> Self {
        Self {
            color,
            canvas,
            graded: (Vec::new(), Vec::new(), Vec::new()),
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
        if self.canvas.is_passthrough(width, height) {
            return Frame {
                index,
                width,
                height,
                bgra: i420_to_bgra_with(&self.color, y, u, v, width as usize, height as usize),
            };
        }
        let (gy, gu, gv) = &mut self.graded;
        let (y, u, v) = if self.color.is_identity() {
            (y, u, v)
        } else {
            gy.clear();
            gy.extend_from_slice(y);
            gu.clear();
            gu.extend_from_slice(u);
            gv.clear();
            gv.extend_from_slice(v);
            crate::color::apply_yuv(&self.color, gy, gu, gv);
            (&gy[..], &gu[..], &gv[..])
        };
        let (y, u, v, width, height) = self.canvas.place(y, u, v, width, height);
        Frame {
            index,
            width,
            height,
            // Ungraded on purpose: the grade is already in the pixels above and
            // the canvas around them must stay the black it was filled with.
            bgra: i420_to_bgra(y, u, v, width as usize, height as usize),
        }
    }
}

/// `None` when the software path must be used: forced by `VE_SW=1`, or the
/// plugin is absent/broken/unable to open this particular file.
fn open_hw(path: &PathBuf, start_frame: u32) -> Option<HwSession> {
    if std::env::var_os("VE_SW").is_some_and(|v| v == "1") {
        return None;
    }
    HwSession::open_at(path, start_frame)
}

/// Returns whether the session was handled; `false` only when hardware decode
/// failed without emitting a single frame, so software can still take over.
///
/// The plugin already positioned itself at `start_frame`, so indices are
/// counted from there (a stream whose very first sample is not a sync sample
/// cannot be decoded from its start at all, and is not accounted for).
fn run_hw(
    mut hw: HwSession,
    tx: &SyncSender<Frame>,
    start_frame: u32,
    end_frame: u32,
    render: &mut Render,
    cancel: &AtomicBool,
) -> bool {
    let mut index = start_frame;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        match hw.next_frame() {
            Ok(Some((y, u, v, width, height))) => {
                let frame = render.frame(index, y, u, v, width, height);
                index += 1;
                if tx.send(frame).is_err() {
                    return true; // consumer went away
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
    use std::time::Duration;

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
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
            Composer::passthrough(),
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
    mut demuxer: Demuxer,
    tx: SyncSender<Frame>,
    start_frame: u32,
    end_frame: u32,
    render: &mut Render,
    cancel: &AtomicBool,
) {
    let mut decoder = Decoder::new();
    // Decoding has to restart at a sync sample, so pictures between it and
    // `start_frame` are decoded (the target frame references them) but never
    // converted or sent. Signed, because the landing sync sample can sit inside
    // what the file's edit list trims, i.e. *before* frame 0.
    let mut index = demuxer.seek_to_sync_at_or_before(start_frame);

    // ponytail: emits pictures in decode order. Fine for Baseline (no B-frames);
    // reordering streams need POC-sorted output before display.
    loop {
        if cancel.load(Ordering::Relaxed) {
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
        let frame = render.frame(
            index as u32,
            &yuv.y,
            &yuv.u,
            &yuv.v,
            yuv.width as u32,
            yuv.height as u32,
        );
        index += 1;
        if tx.send(frame).is_err() {
            break; // consumer went away
        }
        if index >= i64::from(end_frame) {
            break; // end of the requested range
        }
    }
}
