//! Background decode worker. Frames leave over a bounded channel so the caller's
//! thread (the UI thread) never blocks on decode and memory stays capped.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;

use rusty_h264::Decoder;

use crate::convert::i420_to_bgra;
use crate::demux::{Demuxer, VideoMeta};
use crate::hw::HwSession;

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
        let (meta, rx, worker) = Self::open_worker(path, start_frame, end_frame)?;
        Ok((meta, rx, worker.detach()))
    }

    /// As [`DecodeSession::open_range`], but the caller gets the whole
    /// [`Worker`] and can therefore *wait* for it. In-process only: a caller
    /// that outlives its workers has to be able to join them at exit.
    pub(crate) fn open_worker(
        path: impl AsRef<Path>,
        start_frame: u32,
        end_frame: u32,
    ) -> crate::Result<(VideoMeta, Receiver<Frame>, Worker)> {
        let path = path.as_ref().to_path_buf();
        let (meta, demuxer) = Demuxer::open(&path)?;
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
                rx,
                Worker {
                    cancel,
                    handle: None,
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
                // The plugin has to be opened on the thread that uses it: its
                // VA-API state is not `Send`-safe across a later hand-off.
                if let Some(hw) = open_hw(&path, start_frame) {
                    // Cancelled *during* init: close the session (dropping `hw`
                    // does it) and leave without decoding anything.
                    if worker_cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    eprintln!("decode backend: hardware (VA-API plugin)");
                    if run_hw(hw, &tx, start_frame, end_frame, &worker_cancel) {
                        return;
                    }
                    // A driver that opens but cannot decode a single frame is
                    // still a fallback case, not a dead session.
                    eprintln!("hardware decode failed before any frame, falling back to software");
                }
                eprintln!("decode backend: software (rusty_h264)");
                run(demuxer, tx, start_frame, end_frame, &worker_cancel)
            })?;
        Ok((
            meta,
            rx,
            Worker {
                cancel,
                handle: Some(handle),
            },
        ))
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
    cancel: &AtomicBool,
) -> bool {
    let mut index = start_frame;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        match hw.next_frame() {
            Ok(Some((y, u, v, width, height))) => {
                let frame = Frame {
                    index,
                    width,
                    height,
                    bgra: i420_to_bgra(y, u, v, width as usize, height as usize),
                };
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

fn run(
    mut demuxer: Demuxer,
    tx: SyncSender<Frame>,
    start_frame: u32,
    end_frame: u32,
    cancel: &AtomicBool,
) {
    let mut decoder = Decoder::new();
    // Sample ids are 1-based, frame indices 0-based. Decoding has to restart at
    // a sync sample, so pictures between it and `start_frame` are decoded (the
    // target frame references them) but never converted or sent.
    let sync = demuxer.seek_to_sync_at_or_before(start_frame.saturating_add(1));
    let mut index = sync - 1;

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
        if index < start_frame {
            index += 1;
            continue;
        }
        let frame = Frame {
            index,
            width: yuv.width as u32,
            height: yuv.height as u32,
            bgra: i420_to_bgra(&yuv.y, &yuv.u, &yuv.v, yuv.width, yuv.height),
        };
        index += 1;
        if tx.send(frame).is_err() {
            break; // consumer went away
        }
        if index >= end_frame {
            break; // end of the requested range
        }
    }
}
