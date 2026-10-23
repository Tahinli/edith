//! What the three intra-only software seats (`ec-prores`, `ec-dnxhd`, `ec-mjpeg`) share:
//! the pulled [`Stream`], the span loop [`run`], and the first-sample probe the demuxer
//! reads a file's facts with ([`probe`]).
//!
//! **Seeking.** Every picture of these codecs is a sync sample and references nothing, so
//! a span starts exactly on its first frame and a seek is one sample read: no
//! decode-and-drop, and a mid-file start decodes exactly what a sequential decode
//! reaches. The one exception is an interlaced stream, whose deinterlacer
//! ([`crate::deinterlace`], the stage H.264 and MPEG-2 use) wants the picture before: the
//! span then starts one frame earlier and sends nothing before the target.
//!
//! **Pictures.** The seats hand over `I420`, `I422`, `I444` (8 bit), `I210`, `I410`, `I412`
//! (10 and 12 bit), or `Gray8` (MJPEG). [`canon`] folds those into the six shapes the
//! deinterlacer and [`Deep`] carry: 12-bit is narrowed to 10 (`>> 2`), gray gets neutral
//! chroma. 4:2:2 and 4:4:4 are averaged down to 4:2:0 by [`Deep`] exactly as H.264 and
//! HEVC 4:2:2 / 4:4:4 are. ProRes alpha is dropped: the engine has no alpha path for a
//! video clip (a clip is composited as opaque YUV), so the seat decodes the picture alone.
//!
//! **Range.** MJPEG is full range (`yuvj`): [`probe`] states it as the stream's own colour
//! tags, so the same conversion every full-range source takes applies to it.

use std::sync::mpsc::SyncSender;

use ec_core::frame::{Interlace, PixelFormat, Plane, VideoFrame};
use ec_core::registry::{CodecId, CodecParameters, MediaParameters};

use super::{Abort, Deep, DecodeFailure, Frame, I420, LATE_RUN, Render, Wide, skip_for_speed};
use crate::colorspace::{Chroma, Tags};
use crate::deinterlace::Deinterlacer;
use crate::demux::{Codec, Demuxer, VideoMeta};
use crate::project::Speed;

/// The container-independent facts of an intra-only stream that its first sample states.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IntraFacts {
    /// Whether the pictures are two woven fields.
    pub interlaced: bool,
    /// The field that displays first: the stream's own word where it has one (ProRes,
    /// DNxHD), else the container's `fiel` / `FieldOrder`, else top first.
    pub order: Interlace,
    /// The container's coded height: what steers MJPEG's two-field packet rule.
    pub coded_height: u32,
}

impl IntraFacts {
    /// `order`, never `Progressive`.
    pub(crate) fn field_order(self) -> Interlace {
        if self.order.is_interlaced() { self.order } else { Interlace::TopFieldFirst }
    }
}

/// What reading a stream's first sample tells the demuxer about the track.
pub(crate) struct Probed {
    pub facts: IntraFacts,
    pub bit_depth: u8,
    pub chroma: Chroma,
    /// Only where the sample states it (DNxHD's compression id); ProRes' comes from the
    /// sample entry's fourcc.
    pub profile: Option<&'static str>,
    /// The colour the stream itself states, merged under the container's.
    pub tags: Tags,
}

/// Decodes `sample` (a stream's first) with the seat for `codec` and reads the facts off
/// the header and the picture. `Ok(None)` when the sample does not decode (a damaged
/// first frame is the decode's to report, not the open's); `Err` only for a refusal by
/// name (`Unsupported`), which fails the open the way a refused H.264 profile does.
pub(crate) fn probe(
    codec: Codec,
    sample: &[u8],
    meta: &VideoMeta,
    container_order: Option<Interlace>,
    header: &[u8],
) -> Result<Option<Probed>, String> {
    let named = |e: ec_core::Error| match e {
        ec_core::Error::Unsupported { .. } => Err(format!("{} is not supported: {e}", codec.name())),
        _ => Ok(None),
    };
    match codec {
        Codec::ProRes => {
            let mut d = ec_prores::ProResDecoder::new(CodecParameters::new(CodecId::ProRes)).map_err(|e| e.to_string())?;
            d.set_threads(1);
            let full = match d.decode_full(sample) {
                Ok(f) => f,
                Err(e) => return named(e),
            };
            let i = full.info;
            Ok(Some(Probed {
                facts: IntraFacts { interlaced: i.interlace.is_interlaced(), order: i.interlace, coded_height: meta.coded_height },
                bit_depth: if i.is_444 { 12 } else { 10 },
                chroma: if i.is_444 { Chroma::C444 } else { Chroma::C422 },
                profile: None,
                tags: Tags::from_codes(u64::from(i.matrix), u64::from(i.transfer), 0).with_primaries(u64::from(i.primaries)),
            }))
        }
        Codec::Dnxhd => {
            let mut d = ec_dnxhd::DnxhdDecoder::new(CodecParameters::new(CodecId::Dnxhd)).map_err(|e| e.to_string())?;
            d.set_threads(1);
            let pic = match d.decode_native(sample) {
                Ok(p) => p,
                Err(e) => return named(e),
            };
            let i = pic.info;
            let order = match (i.interlaced, i.top_field_first) {
                (false, _) => Interlace::Progressive,
                (true, true) => Interlace::TopFieldFirst,
                (true, false) => Interlace::BottomFieldFirst,
            };
            Ok(Some(Probed {
                facts: IntraFacts { interlaced: i.interlaced, order, coded_height: meta.coded_height },
                bit_depth: i.bit_depth as u8,
                chroma: match i.chroma {
                    ec_dnxhd::Chroma::C444 => Chroma::C444,
                    _ => Chroma::C422,
                },
                profile: crate::video_profile::dnxhd(i.cid, i.bit_depth, sample.len(), meta.frame_rate),
                tags: Tags::from_codes(u64::from(i.matrix), 0, 0),
            }))
        }
        Codec::Mjpeg => {
            let mut d = mjpeg_decoder(meta.coded_height, container_order.unwrap_or(Interlace::TopFieldFirst))?;
            let pic = match d.decode(sample, None) {
                Ok(p) => p,
                Err(e) => return named(e),
            };
            let interlaced = pic.interlace.is_interlaced();
            Ok(Some(Probed {
                facts: IntraFacts {
                    interlaced,
                    order: if interlaced { pic.interlace } else { Interlace::Progressive },
                    coded_height: meta.coded_height,
                },
                bit_depth: 8,
                chroma: match pic.format {
                    PixelFormat::Gray8 => Chroma::Gray,
                    PixelFormat::I422 => Chroma::C422,
                    PixelFormat::I444 => Chroma::C444,
                    _ => Chroma::C420,
                },
                profile: None,
                // JFIF: BT.601, every code used. Stated as the stream's own word so the
                // conversion reads a `yuvj` file as the full-range source it is; a container
                // that states its own (an mp4 `colr`) still wins over it.
                tags: Tags {
                    full_range: Some(true),
                    ..Tags::from_codes(5, 0, 0)
                },
            }))
        }
        Codec::Dv => {
            let mut d = ec_dv::DvDecoder::new(CodecParameters::new(CodecId::DvVideo)).map_err(|e| e.to_string())?;
            let pic = match d.decode_native(sample) {
                Ok(p) => p,
                Err(e) => return named(e),
            };
            let interlaced = pic.interlace.is_interlaced();
            Ok(Some(Probed {
                facts: IntraFacts {
                    interlaced,
                    // The VAUX pack states the order (bottom first for 525 and 625 DV25 and
                    // DV50, top first for DVCPRO HD).
                    order: if interlaced { pic.interlace } else { Interlace::Progressive },
                    coded_height: meta.coded_height,
                },
                bit_depth: 8,
                // 4:1:1 comes out widened to 4:2:2 (exact chroma duplication).
                chroma: match pic.chroma {
                    ec_dv::Chroma::Yuv420 => Chroma::C420,
                    ec_dv::Chroma::Yuv411 | ec_dv::Chroma::Yuv422 => Chroma::C422,
                },
                profile: Some(crate::video_profile::dv(pic.profile)),
                // Untagged: the size rule reads 525 and 625 lines as BT.601 and DVCPRO HD as BT.709.
                tags: Tags::default(),
            }))
        }
        Codec::Raw => {
            let seat = super::raw::RawSeat::open(IntraFacts::default(), header)?;
            let pic = match seat.0.decode(sample) {
                Ok(p) => p,
                Err(e) => return named(e),
            };
            let full = Tags { full_range: Some(true), ..Tags::default() };
            let (chroma, tags, bit_depth) = match pic.format {
                // RGB is converted to full-range BT.709 4:4:4 by the seat, and says so.
                PixelFormat::Bgr8 | PixelFormat::Rgb8 | PixelFormat::Bgra8 => {
                    (Chroma::C444, Tags { full_range: Some(true), ..Tags::from_codes(1, 0, 0) }, 8)
                }
                PixelFormat::I422 => (Chroma::C422, Tags::default(), 8),
                PixelFormat::I210 => (Chroma::C422, Tags::default(), 10),
                PixelFormat::I410 => (Chroma::C444, Tags::default(), 10),
                // Gray is full range to ffmpeg and to JPEG alike.
                PixelFormat::Gray8 => (Chroma::Gray, full, 8),
                PixelFormat::Gray16 => (Chroma::Gray, full, 10),
                _ => (Chroma::C420, Tags::default(), 8),
            };
            Ok(Some(Probed {
                facts: IntraFacts { coded_height: meta.coded_height, ..IntraFacts::default() },
                bit_depth,
                chroma,
                profile: Some(crate::video_profile::raw(header)),
                tags,
            }))
        }
        _ => Ok(None),
    }
}

pub(super) fn mjpeg_decoder(container_height: u32, order: Interlace) -> Result<ec_mjpeg::MjpegDecoder, String> {
    let mut params = CodecParameters::new(CodecId::Mjpeg);
    if let MediaParameters::Video(v) = &mut params.media {
        v.height = container_height;
    }
    let mut d = ec_mjpeg::MjpegDecoder::new(params).map_err(|e| e.to_string())?;
    d.set_field_order(order);
    Ok(d)
}

/// An intra-only software decoder: one sample in, one picture out.
pub(crate) trait Seat: Sized {
    const NAME: &'static str;
    /// `header` is the container's picture header where the codec has none of its own (an AVI's
    /// raw video `BITMAPINFOHEADER`); empty otherwise.
    fn open(facts: IntraFacts, header: &[u8]) -> Result<Self, String>;
    fn decode(&mut self, sample: &[u8]) -> Result<VideoFrame, String>;
}

/// One intra-only decode, pulled a picture at a time.
pub(crate) struct Stream<S: Seat> {
    seat: Result<S, String>,
    /// Display index of the next picture. Signed like the other seats'.
    index: i64,
    flushed: bool,
    deint: Deinterlacer,
    deep: Deep,
    scratch: I420,
}

impl<S: Seat> Stream<S> {
    /// A decoder positioned on `start_frame` (one earlier for an interlaced stream, whose
    /// deinterlacer wants the picture before).
    pub(crate) fn open(demuxer: &mut Demuxer, start_frame: u32) -> Self {
        let facts = demuxer.intra();
        let index = demuxer.seek_to_sync_at_or_before(if facts.interlaced {
            start_frame.saturating_sub(1)
        } else {
            start_frame
        });
        Self {
            seat: S::open(facts, demuxer.video_params().map_or(&[][..], |p| p.3)),
            index,
            flushed: false,
            deint: Deinterlacer::new(facts.interlaced, i64::from(start_frame), Interlace::Progressive).flagged_only(),
            deep: Deep::default(),
            scratch: I420::default(),
        }
    }

    /// The next picture in display order, narrowed to 8-bit 4:2:0 into `out`.
    pub(crate) fn next(&mut self, demuxer: &mut Demuxer, out: &mut I420) -> Result<Option<i64>, String> {
        self.pull(demuxer, Some(out), None)
    }

    /// As [`Self::next`], at 10-bit scale into `out` (an 8-bit picture shifted up by two).
    pub(crate) fn next_wide(&mut self, demuxer: &mut Demuxer, out: &mut Wide) -> Result<Option<i64>, String> {
        self.pull(demuxer, None, Some(out))
    }

    fn pull(
        &mut self,
        demuxer: &mut Demuxer,
        mut narrow: Option<&mut I420>,
        mut wide: Option<&mut Wide>,
    ) -> Result<Option<i64>, String> {
        loop {
            if self.flushed {
                return match self.deint.finish() {
                    Some((due, pic)) => {
                        self.fill(&pic, narrow.as_deref_mut(), wide.as_deref_mut());
                        Ok(Some(due))
                    }
                    None => Ok(None),
                };
            }
            match demuxer.next_access_unit() {
                Ok(Some(sample)) => {
                    let seat = self.seat.as_mut().map_err(|e| e.clone())?;
                    let pic = seat
                        .decode(&sample)
                        .map_err(|e| format!("software {} decode failed at frame {}: {e}", S::NAME, self.index))?;
                    let tag = self.index;
                    self.index += 1;
                    let Some(pic) = canon(pic) else {
                        return Err(format!("software {} frame {tag} has a pixel layout edith does not carry", S::NAME));
                    };
                    if let Some((due, pic)) = self.deint.push(tag, pic) {
                        self.fill(&pic, narrow.as_deref_mut(), wide.as_deref_mut());
                        return Ok(Some(due));
                    }
                }
                Ok(None) => self.flushed = true,
                Err(e) => return Err(format!("demux error: {e}")),
            }
        }
    }

    /// `pic` (one of the six canonical shapes) into whichever output is asked for.
    fn fill(&mut self, pic: &VideoFrame, narrow: Option<&mut I420>, wide: Option<&mut Wide>) {
        let sixteen = matches!(pic.format, PixelFormat::I010 | PixelFormat::I210 | PixelFormat::I410);
        match (narrow, wide) {
            (Some(out), _) => self.fill_narrow(pic, out),
            (None, Some(out)) if sixteen => {
                self.deep.load(pic, true);
                std::mem::swap(out, &mut self.deep.wide);
            }
            (None, Some(out)) => {
                let mut tmp = std::mem::take(&mut self.scratch);
                self.fill_narrow(pic, &mut tmp);
                out.width = tmp.width;
                out.height = tmp.height;
                for (src, dst) in [(&tmp.y, &mut out.y), (&tmp.u, &mut out.u), (&tmp.v, &mut out.v)] {
                    dst.clear();
                    dst.extend(src.iter().map(|&s| u16::from(s) << 2));
                }
                self.scratch = tmp;
            }
            (None, None) => {}
        }
    }

    fn fill_narrow(&mut self, pic: &VideoFrame, out: &mut I420) {
        if pic.format == PixelFormat::I420 {
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
        } else {
            self.deep.load(pic, false);
            std::mem::swap(out, &mut self.deep.narrow);
        }
    }
}

/// Folds a seat's picture into I420, I422, I444, I010, I210 or I410: 12-bit goes to 10
/// (`>> 2`) and gray gets neutral chroma. `None` for a layout no seat produces.
fn canon(pic: VideoFrame) -> Option<VideoFrame> {
    match pic.format {
        PixelFormat::I420
        | PixelFormat::I422
        | PixelFormat::I444
        | PixelFormat::I010
        | PixelFormat::I210
        | PixelFormat::I410 => Some(pic),
        PixelFormat::I412 => {
            let mut out = VideoFrame::alloc(PixelFormat::I410, pic.width, pic.height);
            out.color = pic.color;
            out.pts = pic.pts;
            out.interlace = pic.interlace;
            for (dst, src) in out.planes.iter_mut().zip(&pic.planes) {
                let stride = dst.stride;
                let mut data = vec![0u8; dst.data.len()];
                for (row, line) in data.chunks_mut(stride).enumerate() {
                    let from = &src.data[row * src.stride..row * src.stride + stride];
                    for (d, s) in line.as_chunks_mut::<2>().0.iter_mut().zip(from.as_chunks::<2>().0) {
                        *d = (u16::from_le_bytes(*s) >> 2).to_le_bytes();
                    }
                }
                *dst = Plane::new(data, stride);
            }
            Some(out)
        }
        PixelFormat::Gray8 => {
            let mut out = VideoFrame::alloc(PixelFormat::I420, pic.width, pic.height);
            out.color = pic.color;
            out.pts = pic.pts;
            out.interlace = pic.interlace;
            let w = pic.width as usize;
            let mut luma = vec![0u8; out.planes[0].data.len()];
            for (row, line) in luma.chunks_mut(out.planes[0].stride).enumerate() {
                line[..w].copy_from_slice(&pic.planes[0].data[row * pic.planes[0].stride..][..w]);
            }
            out.planes[0] = Plane::new(luma, out.planes[0].stride);
            for p in 1..3 {
                let stride = out.planes[p].stride;
                out.planes[p] = Plane::new(vec![128u8; out.planes[p].data.len()], stride);
            }
            Some(out)
        }
        _ => None,
    }
}

/// The span loop for any intra-only seat: `[start_frame, end_frame)` through the shared
/// [`Render`], with the same lateness and speed rules every software seat applies.
// The eight inputs every software span takes (`run_hevc` has the same shape).
#[allow(clippy::too_many_arguments)]
pub(super) fn run<S: Seat>(
    demuxer: &mut Demuxer,
    tx: &SyncSender<Frame>,
    start_frame: u32,
    end_frame: u32,
    render: &mut Render,
    abort: &Abort,
    speed: Speed,
    failure: &DecodeFailure,
) {
    let mut stream = Stream::<S>::open(demuxer, start_frame);
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

/// Queues one picture through the shared [`Render`]. `true` when the consumer went away.
fn send(out: &I420, due: u32, render: &mut Render, tx: &SyncSender<Frame>) -> bool {
    let frame = render.frame(due, &out.y, &out.u, &out.v, out.width, out.height);
    tx.send(frame).is_err()
}
