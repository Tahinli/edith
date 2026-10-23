//! The MPEG-4 Part 2 / H.263 software seat (and MS-MPEG4 version 2 and 3, which `ec-mpeg4` decodes
//! the same way, one picture per packet, no reordering): one span of `ec-mpeg4` (Simple and Advanced Simple
//! profile, H.263 baseline) through the same [`Render`] door every other seat's pictures go
//! through. Export reads the same [`Stream`] to decode its sources. Always software: no GPU path
//! exists for it here.
//!
//! `ec-mpeg4` takes one access unit per packet (what mp4/3GP, Matroska and AVI store, with the
//! Visual Object Layer header put in front of a sync sample that lacks one by the demuxer) and
//! releases pictures in display order: I and P one reference late, B at once, and the last one
//! when the stream is flushed. A DivX / XviD "packed bitstream" packet is unpacked by the
//! decoder.
//!
//! **Seeking.** A span starts on the sync sample at or before the target, an I picture. A B
//! picture that follows it in decode order and is displayed before it needs the reference
//! before the I picture; the display index counts from the I picture, where the demuxer put it.
//!
//! **Chroma and fields.** Always 8-bit 4:2:0. An interlaced (field-coded) stream is shown as it
//! is coded: there is no flag in the output to steer the shared deinterlacer with.

use std::sync::mpsc::SyncSender;

use ec_core::registry::{CodecId, CodecParameters, MediaParameters};
use ec_mpeg4::Mpeg4Decoder;

use super::{Abort, DecodeFailure, Frame, I420, LATE_RUN, Render, skip_for_speed};
use crate::demux::Demuxer;
use crate::project::Speed;

/// One MPEG-4 Part 2 decode, pulled a picture at a time.
pub(crate) struct Stream {
    decoder: Mpeg4Decoder,
    /// Display index of the next picture the decoder releases. Signed: the landing sync
    /// sample can sit before frame 0.
    index: i64,
    /// Set once the demuxer runs dry and the decoder has been flushed.
    flushed: bool,
}

impl Stream {
    /// A decoder positioned at the sync sample at or before `start_frame`.
    pub(crate) fn open(demuxer: &mut Demuxer, start_frame: u32) -> Self {
        let index = demuxer.seek_to_sync_at_or_before(start_frame);
        // MS-MPEG4 (an AVI's `MP42`, `DIV3`...) has no header: the codec id and the size come
        // from the container.
        let mut params = CodecParameters::new(CodecId::Mpeg4Part2);
        if let Some((id, width, height, _)) = demuxer.video_params().filter(|p| p.0 != CodecId::Mpeg4Part2) {
            params.codec = id;
            if let MediaParameters::Video(v) = &mut params.media {
                v.width = width;
                v.height = height;
            }
        }
        let mut decoder = Mpeg4Decoder::new(params).expect("ec-mpeg4 takes its own codec ids");
        if let Some(tag) = demuxer.fourcc() {
            decoder.set_fourcc(tag);
        }
        Self { decoder, index, flushed: false }
    }

    /// The next picture in display order, narrowed into `out`, with its display index. `None`
    /// at end of stream; `Err` names a refusal, and the pictures before it stand.
    pub(crate) fn next(&mut self, demuxer: &mut Demuxer, out: &mut I420) -> Result<Option<i64>, String> {
        loop {
            if let Some((pic, _)) = self.decoder.pop_picture() {
                let tag = self.index;
                self.index += 1;
                out.width = pic.width;
                out.height = pic.height;
                let (cw, ch) = (pic.width.div_ceil(2) as usize, pic.height.div_ceil(2) as usize);
                let dims = [(pic.width as usize, pic.height as usize), (cw, ch), (cw, ch)];
                for ((plane, (w, h)), dst) in pic.planes.iter().zip(dims).zip([&mut out.y, &mut out.u, &mut out.v]) {
                    dst.clear();
                    for row in plane.data.chunks(plane.stride).take(h) {
                        dst.extend_from_slice(&row[..w]);
                    }
                }
                return Ok(Some(tag));
            }
            if self.flushed {
                return Ok(None);
            }
            match demuxer.next_access_unit() {
                Ok(Some(au)) => {
                    self.decoder
                        .send_bytes(&au, None)
                        .map_err(|e| format!("software MPEG-4 decode failed at frame {}: {e}", self.index))?;
                }
                Ok(None) => {
                    self.flushed = true;
                    self.decoder
                        .flush_stream()
                        .map_err(|e| format!("software MPEG-4 decode failed at end of stream: {e}"))?;
                }
                Err(e) => return Err(format!("demux error: {e}")),
            }
        }
    }
}

/// The MPEG-4 Part 2 software seat over `[start_frame, end_frame)`. A span decodes from the
/// sync sample at or before `start_frame`; pictures before the target are decoded (the target
/// references them) but never sent.
///
/// A refusal out of `ec-mpeg4` fails the span in words: named on stderr, the pictures decoded
/// so far stand, and no garbage is sent.
// The eight inputs every software span takes (`run_mpeg2` has the same shape).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_mpeg4(
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
        let sent = if abort.late(due) {
            if skipped < LATE_RUN {
                skipped += 1;
                false
            } else {
                skipped = 0;
                send(&out, due, render, tx)
            }
        } else if skip_for_speed(speed, start_frame, due, end_frame) {
            false
        } else {
            skipped = 0;
            send(&out, due, render, tx)
        };
        if sent || i64::from(due) + 1 >= i64::from(end_frame) {
            break;
        }
    }
}

/// Queues one picture through the shared [`Render`]. `true` when the consumer went away, so the
/// span stops rather than keeps decoding into a void.
fn send(out: &I420, due: u32, render: &mut Render, tx: &SyncSender<Frame>) -> bool {
    let frame = render.frame(due, &out.y, &out.u, &out.v, out.width, out.height);
    tx.send(frame).is_err()
}
