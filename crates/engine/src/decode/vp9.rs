//! The VP9 software seat: one span of `ec-vp9` through the same [`Render`] door
//! every other seat's pictures go through, and the same way a refusal is named
//! on stderr. Export reads the same [`Stream`] to decode its sources.
//!
//! `ec-vp9` takes one demuxed block (a frame or a superframe) at a time, which
//! is exactly what the demuxer hands out, so unlike AV1 there is no chunking:
//! every access unit goes straight into the decoder. A hidden frame comes back
//! as `None` and takes no display slot, and a shown one (including a
//! `show_existing_frame`) comes back as the picture it displays. VP9 has no
//! reorder delay, so nothing is held back at the end of the stream.

use std::sync::mpsc::SyncSender;

use ec_vp9::decode::{Decoder, Picture};

use super::{Abort, DecodeFailure, Frame, I420, LATE_RUN, Render, skip_for_speed};
use crate::demux::Demuxer;
use crate::project::Speed;

/// One VP9 decode, pulled a picture at a time: the decoder and the display
/// index of the next shown picture.
pub(crate) struct Stream {
    decoder: Decoder,
    /// Signed, for the same reason the AV1 span's is: the landing sync sample
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

    /// The next shown picture, narrowed into `out`, with its display index.
    /// `None` at end of stream.
    pub(crate) fn next(
        &mut self,
        demuxer: &mut Demuxer,
        out: &mut I420,
    ) -> Result<Option<i64>, String> {
        loop {
            let au = match demuxer.next_access_unit() {
                Ok(Some(au)) => au,
                // The demuxer is dry: every shown frame has already been returned.
                Ok(None) => return Ok(None),
                Err(e) => return Err(format!("demux error: {e}")),
            };
            let picture = match self.decoder.decode(&au) {
                Ok(Some(picture)) => picture,
                // A hidden frame: a reference, with no display slot of its own.
                Ok(None) => continue,
                Err(e) => {
                    return Err(format!(
                        "software VP9 decode failed at frame {}: {e}",
                        self.index
                    ));
                }
            };
            if !carried(&picture) {
                return Err(format!(
                    "software VP9 frame {} is {}-bit, and only 8 or 10 bits is carried",
                    self.index, picture.bit_depth,
                ));
            }
            narrow(&picture, out);
            let due = self.index;
            self.index += 1;
            return Ok(Some(due));
        }
    }
}

/// The VP9 software seat over `[start_frame, end_frame)`. A span decodes from
/// the sync sample at or before `start_frame`; pictures before the target are
/// decoded (the target references them) but never sent.
///
/// A refusal out of `ec-vp9`, or a picture outside what edith carries (anything
/// but 8 or 10 bits), fails the span in words: named on stderr, the
/// pictures decoded so far stand, and no garbage is sent.
// The eight inputs every software span takes (`run_vp8` has the same shape); a
// one-off struct would only ever be built and taken apart here.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_vp9(
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
        // construction at speed, or a conversion and a send.
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

/// Whether edith carries this picture's bit depth: 8 or 10 bits. Profile 1 and 3
/// (4:4:4, 4:2:2, 4:4:0) pictures are carried too, their chroma reduced to 4:2:0 by
/// [`narrow`]; 12-bit is refused at open by the demuxer (`vp9_bit_depth`), and this
/// is the same line drawn on the decoded picture.
fn carried(picture: &Picture) -> bool {
    matches!(picture.bit_depth, 8 | 10)
}

/// One [`Picture`] down to the tightly packed 8-bit I420 every converter here
/// takes, by the same rule [`super::narrow_av1`] applies: a 10-bit sample
/// downshifts by two, the truncation the hardware seat's P010 read-back does.
/// Chroma that is not 4:2:0 is reduced to it by averaging the source samples each
/// output sample covers (a 2x2 box where the source is subsampled less, so a 4:4:4
/// chroma plane averages 2x2 and a 4:2:2 one averages 1x2 vertically).
fn narrow(picture: &Picture, out: &mut I420) {
    let shift = picture.bit_depth.saturating_sub(8);
    let plane = |src: &[u16], dst: &mut Vec<u8>| {
        dst.clear();
        dst.extend(src.iter().map(|&sample| (sample >> shift) as u8));
    };
    let (w, h) = (usize::from(picture.width), usize::from(picture.height));
    plane(&picture.y[..w * h], &mut out.y);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (ssx, ssy) = (usize::from(picture.subsampling_x), usize::from(picture.subsampling_y));
    if (ssx, ssy) == (1, 1) {
        plane(&picture.u, &mut out.u);
        plane(&picture.v, &mut out.v);
    } else {
        let stride = picture.uv_stride;
        let (sw, sh) = ((w + ssx) >> ssx, (h + ssy) >> ssy);
        let reduce = |src: &[u16], dst: &mut Vec<u8>| {
            dst.clear();
            for oy in 0..ch {
                let (y0, y1) = if ssy == 0 { (2 * oy, (2 * oy + 2).min(sh)) } else { (oy, oy + 1) };
                for ox in 0..cw {
                    let (x0, x1) = if ssx == 0 { (2 * ox, (2 * ox + 2).min(sw)) } else { (ox, ox + 1) };
                    let (mut sum, mut n) = (0u32, 0u32);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            sum += u32::from(src[y * stride + x]);
                            n += 1;
                        }
                    }
                    dst.push(((sum / n) >> shift) as u8);
                }
            }
        };
        reduce(&picture.u, &mut out.u);
        reduce(&picture.v, &mut out.v);
    }
    out.width = u32::from(picture.width);
    out.height = u32::from(picture.height);
}

/// Queues one narrowed picture through the shared [`Render`]. `true` when the
/// consumer went away, so the span stops rather than keeps decoding into a void.
fn send(out: &I420, due: u32, render: &mut Render, tx: &SyncSender<Frame>) -> bool {
    let frame = render.frame(due, &out.y, &out.u, &out.v, out.width, out.height);
    tx.send(frame).is_err()
}
