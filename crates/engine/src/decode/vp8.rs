//! The VP8 software seat: one span of `ec-vp8` through the same loop
//! [`super::run_vp9`] drives `ec-vp9` through, and the same door -- the
//! [`Render`] every other seat's pictures go through, the same speed and
//! lateness rules, the same way a refusal is named on stderr. Export reads the
//! same [`Stream`] to decode its sources.
//!
//! `ec-vp8` takes one demuxed block (one complete frame) at a time, which is
//! exactly what the demuxer hands out, so every access unit goes straight into
//! the decoder. A shown frame comes back as the picture it displays. A hidden
//! frame (an alt-ref or golden update that is not shown) comes back as `None`:
//! unlike VP9's, a hidden VP8 frame is a block of its own in WebM and mp4, so it
//! takes a container slot and sends nothing into it. VP8 has no reorder delay and
//! no `show_existing_frame`, so nothing is held back at the end of the stream.
//!
//! Seeking has only one kind of landing point: a key frame. The demuxer's sync
//! samples are exactly those, so a span starts at the key frame at or before
//! the target, decodes through it, and sends nothing before the target.

use std::sync::mpsc::SyncSender;

use ec_vp8::decode::{Decoder, Picture};

use super::{Abort, DecodeFailure, Frame, I420, LATE_RUN, Render, skip_for_speed};
use crate::demux::Demuxer;
use crate::project::Speed;

/// One VP8 decode, pulled a picture at a time: the decoder and the display index
/// of the next slot. A hidden frame still takes a slot, so the index moves past
/// it without a picture.
pub(crate) struct Stream {
    decoder: Decoder,
    /// Signed, for the same reason the other spans' is: the landing sync sample
    /// can sit inside what an mp4's edit list trims, i.e. before frame 0.
    index: i64,
}

impl Stream {
    /// A decoder positioned at the sync sample at or before `start_frame`.
    pub(crate) fn open(demuxer: &mut Demuxer, start_frame: u32) -> Self {
        Self {
            decoder: Decoder::new(),
            index: demuxer.seek_to_sync_at_or_before(start_frame),
        }
    }

    /// The next slot that shows a picture, copied into `out`, with its display
    /// index. `None` at end of stream.
    pub(crate) fn next(
        &mut self,
        demuxer: &mut Demuxer,
        out: &mut I420,
    ) -> Result<Option<i64>, String> {
        loop {
            let frame = match demuxer.next_access_unit() {
                Ok(Some(frame)) => frame,
                // The demuxer is dry: every shown frame has already been returned.
                Ok(None) => return Ok(None),
                Err(e) => return Err(format!("demux error: {e}")),
            };
            let picture = match self.decoder.decode(&frame) {
                Ok(Some(picture)) => picture,
                // A hidden frame: the sample still owns its slot on the container's
                // timeline (the count `frame_count` and every seek index are made
                // of), it just has no picture to show, so the slot is passed over.
                Ok(None) => {
                    self.index += 1;
                    continue;
                }
                Err(e) => {
                    return Err(format!(
                        "software VP8 decode failed at frame {}: {e}",
                        self.index
                    ));
                }
            };
            copy(&picture, out);
            let due = self.index;
            self.index += 1;
            return Ok(Some(due));
        }
    }
}

/// The VP8 software seat over `[start_frame, end_frame)`. A span decodes from
/// the key frame at or before `start_frame`; pictures before the target are
/// decoded (the target references them) but never sent.
///
/// A refusal out of `ec-vp8` fails the span in words: named on stderr, the
/// pictures decoded so far stand, and no garbage is sent.
// The eight inputs every software span takes (`run_vp9` has the same shape); a
// one-off struct would only ever be built and taken apart here.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_vp8(
    demuxer: &mut Demuxer,
    tx: &SyncSender<Frame>,
    start_frame: u32,
    end_frame: u32,
    render: &mut Render,
    abort: &Abort,
    speed: Speed,
    failure: &DecodeFailure,
) {
    let mut stream = Stream::open(demuxer, start_frame);
    let mut out = I420::default();
    let mut skipped = 0;
    while !abort.hit() {
        let due = match stream.next(demuxer, &mut out) {
            Ok(Some(due)) => due,
            Ok(None) => break,
            Err(e) => {
                failure.set(e);
                break;
            }
        };
        if due < i64::from(start_frame) {
            continue;
        }
        let due = due as u32;
        // The three ends [`super::run`] reaches per picture: late for the
        // playhead (one in [`LATE_RUN`] still goes through), redundant by
        // construction at speed, or a send.
        let sent = if abort.late(due) {
            if skipped < LATE_RUN {
                skipped += 1;
                false
            } else {
                skipped = 0;
                send(&out, due, render, tx)
            }
        } else if skip_for_speed(speed, start_frame, due, end_frame) {
            // Redundant by construction, not by lateness (see `skip_for_speed`).
            false
        } else {
            skipped = 0;
            send(&out, due, render, tx)
        };
        // The consumer went away, or the range is out: nothing left to decode for.
        if sent || i64::from(due) + 1 >= i64::from(end_frame) {
            break;
        }
    }
}

/// `ec-vp8` hands back 8-bit 4:2:0 planes already cropped to the frame, which
/// is the tightly packed I420 every converter here takes -- no narrowing, only
/// the copy that lets the planes outlive the decoder's buffer.
fn copy(picture: &Picture, out: &mut I420) {
    out.width = u32::from(picture.width);
    out.height = u32::from(picture.height);
    out.y.clear();
    out.y.extend_from_slice(&picture.y);
    out.u.clear();
    out.u.extend_from_slice(&picture.u);
    out.v.clear();
    out.v.extend_from_slice(&picture.v);
}

/// Queues one picture through the shared [`Render`]. `true` when the consumer
/// went away, so the span stops rather than keeps decoding into a void.
fn send(out: &I420, due: u32, render: &mut Render, tx: &SyncSender<Frame>) -> bool {
    let frame = render.frame(due, &out.y, &out.u, &out.v, out.width, out.height);
    tx.send(frame).is_err()
}
