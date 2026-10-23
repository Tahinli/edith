//! The MPEG-1/2 video software seat: one span of `ec-mpeg2` through the same
//! [`Render`] door every other seat's pictures go through. Export reads the same
//! [`Stream`] to decode its sources. Always software: no GPU path exists for it here.
//!
//! `ec-mpeg2` takes one coded picture per packet (what the transport-stream demuxer
//! hands out, with the sequence header in front of a keyframe that lacks one) and
//! releases frames in display order, I and P one reference late and B at once; the
//! last ones wait in it until the stream is flushed.
//!
//! **Seeking.** A span starts on the keyframe at or before the target: a sequence
//! header or an I picture. A B picture of an open GOP that follows it and needs the
//! reference before it has none, so `ec-mpeg2` drops it, as ffmpeg does; those
//! pictures sit *before* the landing I picture in display order, so the display index
//! counts from the I picture, where the demuxer put it.
//!
//! **Interlace.** Field pictures come out woven, with the field order in the frame. An
//! interlaced sequence (`progressive_sequence` 0) goes through the same deinterlacer
//! H.264 does ([`crate::deinterlace`]), picture by picture: a picture coded
//! progressive (`progressive_frame` 1, a film cadence inside an interlaced sequence)
//! is left exactly as it was. As for H.264 the stage runs one picture behind, so a
//! seek lands one picture earlier and the target is filtered from the same neighbours
//! a sequential decode gave it.
//!
//! **Chroma.** 4:2:2 (the 4:2:2 profile) has its chroma averaged down a pair of rows
//! to 4:2:0 as H.264 and HEVC 4:2:2 do ([`Deep`]).
//!
//! `repeat_first_field` (3:2 pulldown) is not applied: the timeline is frame-locked
//! and the demuxer counts one frame per coded picture.

use std::sync::mpsc::SyncSender;

use ec_core::frame::{Frame as Decoded, Interlace, PixelFormat, VideoFrame};
use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
use ec_core::{Packet, TimeBase};
use ec_mpeg2::Mpeg2Decoder;

use super::{Abort, Deep, DecodeFailure, Frame, I420, LATE_RUN, Render, skip_for_speed};
use crate::deinterlace::Deinterlacer;
use crate::demux::Demuxer;
use crate::project::{Deinterlace, Speed};

/// One MPEG-1/2 decode, pulled a picture at a time.
pub(crate) struct Stream {
    decoder: Mpeg2Decoder,
    /// Display index of the next picture the decoder releases. Signed: the landing
    /// keyframe can sit before frame 0.
    index: i64,
    /// Set once the demuxer runs dry and the decoder has been flushed.
    flushed: bool,
    deint: Deinterlacer,
    deep: Deep,
    /// Pictures the deinterlacing stage has released and `next` has not yet handed
    /// out (pulldown removal can release two at once).
    ready: std::collections::VecDeque<(i64, VideoFrame)>,
}

impl Stream {
    /// A decoder positioned at the keyframe at or before `start_frame` (one earlier
    /// for an interlaced sequence, whose deinterlacer wants the picture before).
    pub(crate) fn open(demuxer: &mut Demuxer, start_frame: u32, mode: Deinterlace) -> Self {
        let interlaced = demuxer.h264_interlaced();
        let deint = Deinterlacer::new(interlaced, i64::from(start_frame), Interlace::Progressive)
            .flagged_only()
            .cadenced(mode, demuxer);
        let index = demuxer.seek_to_sync_at_or_before(deint.landing(start_frame));
        Self {
            decoder: Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video))
                .expect("ec-mpeg2 takes its own codec id"),
            index,
            flushed: false,
            deint,
            deep: Deep::default(),
            ready: Default::default(),
        }
    }

    /// The next picture in display order, narrowed into `out`, with its display
    /// index. `None` at end of stream; `Err` names a refusal, and the pictures before
    /// it stand.
    pub(crate) fn next(
        &mut self,
        demuxer: &mut Demuxer,
        out: &mut I420,
    ) -> Result<Option<i64>, String> {
        loop {
            if let Some((due, pic)) = self.ready.pop_front() {
                self.narrow(&pic, out)?;
                return Ok(Some(due));
            }
            match self.decoder.receive_frame() {
                Ok(Decoded::Video(pic)) => {
                    let tag = self.index;
                    self.index += 1;
                    self.ready.extend(self.deint.push_all(tag, pic));
                    continue;
                }
                Ok(_) => continue,
                Err(e) if e.is_need_more() || e.is_eof() => {}
                Err(e) => {
                    return Err(format!("software MPEG-2 decode failed at frame {}: {e}", self.index));
                }
            }
            if self.flushed {
                // The end of the stream releases the picture the stage was holding
                // for a successor.
                self.ready.extend(self.deint.finish_all());
                return match self.ready.pop_front() {
                    Some((due, pic)) => {
                        self.narrow(&pic, out)?;
                        Ok(Some(due))
                    }
                    None => Ok(None),
                };
            }
            match demuxer.next_access_unit() {
                Ok(Some(au)) => {
                    let packet = Packet::new(0, TimeBase::new(1, 1), au);
                    self.decoder.send_packet(&packet).map_err(|e| {
                        format!("software MPEG-2 decode failed at frame {}: {e}", self.index)
                    })?;
                }
                Ok(None) => {
                    self.flushed = true;
                    self.decoder
                        .flush()
                        .map_err(|e| format!("software MPEG-2 decode failed at end of stream: {e}"))?;
                }
                Err(e) => return Err(format!("demux error: {e}")),
            }
        }
    }

    /// One picture as the tightly packed 8-bit 4:2:0 every converter takes.
    fn narrow(&mut self, pic: &VideoFrame, out: &mut I420) -> Result<(), String> {
        match pic.format {
            PixelFormat::I420 => {
                out.width = pic.width;
                out.height = pic.height;
                let (cw, ch) = (pic.width.div_ceil(2) as usize, pic.height.div_ceil(2) as usize);
                let dims = [(pic.width as usize, pic.height as usize), (cw, ch), (cw, ch)];
                for ((plane, (w, h)), dst) in pic
                    .planes
                    .iter()
                    .zip(dims)
                    .zip([&mut out.y, &mut out.u, &mut out.v])
                {
                    dst.clear();
                    for row in plane.data.chunks(plane.stride).take(h) {
                        dst.extend_from_slice(&row[..w]);
                    }
                }
                Ok(())
            }
            PixelFormat::I422 => {
                self.deep.load(pic, false);
                std::mem::swap(out, &mut self.deep.narrow);
                Ok(())
            }
            other => Err(format!(
                "software MPEG-2 frame {} is {other:?}, and only 4:2:0 and 4:2:2 are carried",
                self.index
            )),
        }
    }
}

/// The MPEG-1/2 software seat over `[start_frame, end_frame)`. A span decodes from
/// the keyframe at or before `start_frame`; pictures before the target are decoded
/// (the target references them) but never sent.
///
/// A refusal out of `ec-mpeg2` fails the span in words: named on stderr, the pictures
/// decoded so far stand, and no garbage is sent.
// The eight inputs every software span takes (`run_hevc` has the same shape); a
// one-off struct would only ever be built and taken apart here.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_mpeg2(
    demuxer: &mut Demuxer,
    tx: &SyncSender<Frame>,
    start_frame: u32,
    end_frame: u32,
    render: &mut Render,
    abort: &Abort,
    speed: Speed,
    failure: &DecodeFailure,
    deint: Deinterlace,
) {
    let mut stream = Stream::open(demuxer, start_frame, deint);
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

/// Queues one picture through the shared [`Render`]. `true` when the consumer went
/// away, so the span stops rather than keeps decoding into a void.
fn send(out: &I420, due: u32, render: &mut Render, tx: &SyncSender<Frame>) -> bool {
    let frame = render.frame(due, &out.y, &out.u, &out.v, out.width, out.height);
    tx.send(frame).is_err()
}
