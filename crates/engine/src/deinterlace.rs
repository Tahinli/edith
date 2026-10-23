//! Deinterlacing of woven H.264 pictures: a spatial-temporal, motion-adaptive
//! filter after `bwdif` (Bob Weaver Deinterlacing Filter, itself a yadif
//! derivative), in Rust and with no dependency.
//!
//! `ec-h264` hands over **woven** frames (both fields interleaved on the even
//! and odd lines) with the field order in [`VideoFrame::interlace`]. This turns
//! each into one progressive frame of the same rate ("send_frame" in ffmpeg's
//! words): the field that displays first is kept line for line, and the lines
//! of the other field are rebuilt from
//!
//! * the kept lines above and below (a 4-tap vertical filter, with a
//!   high-frequency term from the temporal neighbours when the picture moves),
//! * the same lines in the previous and the current frame (what the second
//!   field looked like a field before and a field after the kept one), and
//! * how much the kept lines changed from the previous to the next frame, which
//!   bounds how far the rebuilt line may stray from the temporal estimate.
//!
//! A static picture therefore keeps its full vertical detail, and a moving edge
//! is interpolated instead of combed. The filter needs the **previous** and the
//! **next** frame, so [`Deinterlacer`] runs one frame behind its input; the
//! first frame has itself as its past and the last as its future.
//!
//! The double-rate form (one frame per field, 50i -> 50p, a clip's
//! [`Deinterlace::Field`]) is the same filter run twice per input frame:
//! [`filter_field`] with the first field kept, then the second with its own
//! temporal neighbours (the current and the next frame), `bwdif=mode=send_field`.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use ec_core::registry::{CodecId, CodecParameters};
use ec_core::{Frame, Interlace, Packet, PixelFormat, Plane, TimeBase, VideoFrame};
use ec_h264::H264Decoder;

use crate::demux::Demuxer;
use crate::project::Deinterlace;

/// Which field of a woven frame survives untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// The top field (even lines) stays; the odd lines are rebuilt. Top-field-first
    /// content shown at its frame rate, and the second field of bottom-first
    /// content in a double-rate output.
    Top,
    /// The bottom field (odd lines) stays; the even lines are rebuilt.
    Bottom,
}

impl Keep {
    /// The field a frame-rate output keeps: the one that displays first.
    pub fn first_of(order: Interlace) -> Keep {
        match order {
            Interlace::BottomFieldFirst => Keep::Bottom,
            _ => Keep::Top,
        }
    }

    /// Parity (0 even, 1 odd) of the lines that are rebuilt.
    fn missing(self) -> usize {
        match self {
            Keep::Top => 1,
            Keep::Bottom => 0,
        }
    }
}

/// The field order of an interlaced H.264 stream, as `ffprobe` spells it
/// (`"tff"` or `"bff"`), `None` for a stream that is not interlaced.
///
/// The stream's own word comes first: the first picture `ec-h264` releases
/// states its order (the field with the lower picture order count displays
/// first). A stream that states none falls to the container (an mp4 `fiel`
/// box, a Matroska `FieldOrder`), and then to top-field-first, which is what
/// ffmpeg's deinterlacers assume. Leaves `demuxer` at the start of the stream.
pub fn field_order(demuxer: &mut Demuxer, path: &Path) -> Option<&'static str> {
    // ProRes, DNxHD and MJPEG state theirs in the first sample, read at open.
    let intra = demuxer.intra();
    if intra.interlaced {
        return Some(if intra.field_order() == Interlace::BottomFieldFirst { "bff" } else { "tff" });
    }
    if !demuxer.h264_interlaced() {
        return None;
    }
    let order = stream_order(demuxer);
    demuxer.seek_to_sync_at_or_before(0);
    let order = order
        .filter(|o| o.is_interlaced())
        .or_else(|| container_order(path))
        .unwrap_or(Interlace::TopFieldFirst);
    Some(if order == Interlace::BottomFieldFirst { "bff" } else { "tff" })
}

/// The order the first decoded picture states.
fn stream_order(demuxer: &mut Demuxer) -> Option<Interlace> {
    demuxer.seek_to_sync_at_or_before(0);
    if demuxer.is_mpeg2() {
        let decoder = ec_mpeg2::Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).ok()?;
        return first_order(demuxer, decoder, |pic| pic.interlace.is_interlaced());
    }
    let decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).ok()?;
    first_order(demuxer, decoder, |_| true)
}

/// Feeds `decoder` until a picture `counts` comes out, and returns its order. MPEG-2
/// skips the pictures coded progressive: they state no order.
fn first_order<D: ec_core::registry::Decoder>(
    demuxer: &mut Demuxer,
    mut decoder: D,
    counts: impl Fn(&VideoFrame) -> bool,
) -> Option<Interlace> {
    let take = |decoder: &mut D| loop {
        match decoder.receive_frame() {
            Ok(Frame::Video(pic)) if counts(&pic) => return Some(pic.interlace),
            Ok(_) => {}
            Err(_) => return None,
        }
    };
    for _ in 0..64 {
        let Ok(Some(au)) = demuxer.next_access_unit() else { break };
        decoder.send_packet(&Packet::new(0, TimeBase::new(1, 1), au)).ok()?;
        if let Some(order) = take(&mut decoder) {
            return Some(order);
        }
    }
    decoder.flush().ok()?;
    take(&mut decoder)
}

/// What the container says, where it says anything: the mp4 `fiel` box
/// (QuickTime field-handling, detail 1 and 14 display the top field first, 6
/// and 9 the bottom) and the Matroska `FieldOrder` element (1 tff, 6 bff).
/// Looked for in the head and the tail of the file, where an mp4's `moov` and
/// an mkv's `Tracks` live.
pub(crate) fn container_order(path: &Path) -> Option<Interlace> {
    const WINDOW: u64 = 8 << 20;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut windows = vec![(0, len.min(WINDOW))];
    if len > WINDOW {
        windows.push((len - WINDOW, WINDOW));
    }
    for (at, size) in windows {
        let mut buf = vec![0u8; size as usize];
        file.seek(SeekFrom::Start(at)).ok()?;
        file.read_exact(&mut buf).ok()?;
        if let Some(i) = buf.windows(4).position(|w| w == b"fiel")
            && let (Some(&count), Some(&detail)) = (buf.get(i + 4), buf.get(i + 5))
            && count == 2
        {
            match detail {
                1 | 14 => return Some(Interlace::TopFieldFirst),
                6 | 9 => return Some(Interlace::BottomFieldFirst),
                _ => {}
            }
        }
        if let Some(head) = buf.get(..65536.min(buf.len()))
            && let Some(i) = head.windows(2).position(|w| w == [0x9D, 0x81])
        {
            match head.get(i + 2) {
                Some(1) => return Some(Interlace::TopFieldFirst),
                Some(6) => return Some(Interlace::BottomFieldFirst),
                _ => {}
            }
        }
    }
    None
}

const COEF_LF: [i32; 2] = [4309, 213];
const COEF_HF: [i32; 3] = [5570, 3801, 1016];
const COEF_SP: [i32; 2] = [5077, 981];

/// A sample width: 8-bit bytes or 10-bit little-endian words.
trait Px {
    const BYTES: usize;
    const MAX: i32;
    fn get(row: &[u8], x: usize) -> i32;
    fn put(row: &mut [u8], x: usize, v: i32);
}

struct P8;
struct P16;

impl Px for P8 {
    const BYTES: usize = 1;
    const MAX: i32 = 255;
    #[inline(always)]
    fn get(row: &[u8], x: usize) -> i32 {
        i32::from(row[x])
    }
    #[inline(always)]
    fn put(row: &mut [u8], x: usize, v: i32) {
        row[x] = v as u8;
    }
}

impl Px for P16 {
    const BYTES: usize = 2;
    const MAX: i32 = 1023;
    #[inline(always)]
    fn get(row: &[u8], x: usize) -> i32 {
        i32::from(u16::from_le_bytes([row[2 * x], row[2 * x + 1]]))
    }
    #[inline(always)]
    fn put(row: &mut [u8], x: usize, v: i32) {
        row[2 * x..2 * x + 2].copy_from_slice(&(v as u16).to_le_bytes());
    }
}

/// One plane of the three frames, read-only.
#[derive(Clone, Copy)]
struct Src<'a> {
    prev: &'a [u8],
    cur: &'a [u8],
    next: &'a [u8],
    /// The frames the rebuilt line's temporal estimate reads (bwdif's `prev2` and
    /// `next2`): previous and current for the first field of a frame, current and
    /// next for the second.
    p2: &'a [u8],
    n2: &'a [u8],
    stride: usize,
    rows: usize,
}

impl<'a> Src<'a> {
    #[inline(always)]
    fn row(&self, data: &'a [u8], y: usize, row_bytes: usize) -> &'a [u8] {
        &data[y * self.stride..y * self.stride + row_bytes]
    }
}

/// Rebuilds row `y` (a missing-parity row) into `dst`.
#[inline(always)]
fn line<S: Px>(dst: &mut [u8], src: &Src<'_>, y: usize, w: usize) {
    let rb = w * S::BYTES;
    let h = src.rows;
    // The kept rows around `y`, and the missing rows of the previous and
    // current frame at `y` (the temporal neighbours `prev2` and `next2`).
    let above = if y > 0 { y - 1 } else { y + 1 };
    let below = if y + 1 < h { y + 1 } else { y - 1 };
    let c = src.row(src.cur, above, rb);
    let e = src.row(src.cur, below, rb);
    let pc = src.row(src.prev, above, rb);
    let pe = src.row(src.prev, below, rb);
    let nc = src.row(src.next, above, rb);
    let ne = src.row(src.next, below, rb);
    let p0 = src.row(src.p2, y, rb);
    let n0 = src.row(src.n2, y, rb);
    if y >= 4 && y + 4 < h {
        let c3 = src.row(src.cur, y - 3, rb);
        let e3 = src.row(src.cur, y + 3, rb);
        let pm2 = src.row(src.p2, y - 2, rb);
        let pp2 = src.row(src.p2, y + 2, rb);
        let pm4 = src.row(src.p2, y - 4, rb);
        let pp4 = src.row(src.p2, y + 4, rb);
        let nm2 = src.row(src.n2, y - 2, rb);
        let np2 = src.row(src.n2, y + 2, rb);
        let nm4 = src.row(src.n2, y - 4, rb);
        let np4 = src.row(src.n2, y + 4, rb);
        for x in 0..w {
            let (cv, ev) = (S::get(c, x), S::get(e, x));
            let (pa, na) = (S::get(p0, x), S::get(n0, x));
            let d = (pa + na) >> 1;
            let td0 = (pa - na).abs();
            let td1 = ((S::get(pc, x) - cv).abs() + (S::get(pe, x) - ev).abs()) >> 1;
            let td2 = ((S::get(nc, x) - cv).abs() + (S::get(ne, x) - ev).abs()) >> 1;
            let mut diff = (td0 >> 1).max(td1).max(td2);
            if diff == 0 {
                S::put(dst, x, d);
                continue;
            }
            let b = ((S::get(pm2, x) + S::get(nm2, x)) >> 1) - cv;
            let f = ((S::get(pp2, x) + S::get(np2, x)) >> 1) - ev;
            let (dc, de) = (d - cv, d - ev);
            let max = de.max(dc).max(b.min(f));
            let min = de.min(dc).min(b.max(f));
            diff = diff.max(min).max(-max);
            let sum3 = S::get(c3, x) + S::get(e3, x);
            let interpol = if (cv - ev).abs() > td0 {
                (((COEF_HF[0] * (pa + na)
                    - COEF_HF[1] * (S::get(pm2, x) + S::get(nm2, x) + S::get(pp2, x) + S::get(np2, x))
                    + COEF_HF[2]
                        * (S::get(pm4, x) + S::get(nm4, x) + S::get(pp4, x) + S::get(np4, x)))
                    >> 2)
                    + COEF_LF[0] * (cv + ev)
                    - COEF_LF[1] * sum3)
                    >> 13
            } else {
                (COEF_SP[0] * (cv + ev) - COEF_SP[1] * sum3) >> 13
            };
            let v = interpol.clamp(d - diff, d + diff).clamp(0, S::MAX);
            S::put(dst, x, v);
        }
    } else {
        // The first and last rows have too few neighbours for the long
        // filter: the plain average, held to the temporal estimate.
        for x in 0..w {
            let (cv, ev) = (S::get(c, x), S::get(e, x));
            let (pa, na) = (S::get(p0, x), S::get(n0, x));
            let d = (pa + na) >> 1;
            let td0 = (pa - na).abs();
            let td1 = ((S::get(pc, x) - cv).abs() + (S::get(pe, x) - ev).abs()) >> 1;
            let td2 = ((S::get(nc, x) - cv).abs() + (S::get(ne, x) - ev).abs()) >> 1;
            let diff = (td0 >> 1).max(td1).max(td2);
            let v = ((cv + ev) >> 1).clamp(d - diff, d + diff).clamp(0, S::MAX);
            S::put(dst, x, v);
        }
    }
}

/// Rebuilds every missing row of one plane into `out` (a copy of the current
/// plane), `bands` threads wide.
fn plane<S: Px>(src: Src<'_>, w: usize, missing: usize, bands: usize) -> Vec<u8> {
    let mut out = src.cur.to_vec();
    let stride = src.stride;
    let rows = src.rows;
    let work = |first: usize, data: &mut [u8]| {
        for (i, chunk) in data.chunks_mut(stride).enumerate() {
            let y = first + i;
            if y >= rows {
                break;
            }
            if y % 2 == missing {
                line::<S>(chunk, &src, y, w);
            }
        }
    };
    if bands <= 1 || rows < 64 {
        work(0, &mut out);
        return out;
    }
    // Bands of an even number of rows, so every band starts on the same parity.
    let per = (rows.div_ceil(bands) + 1) & !1;
    std::thread::scope(|scope| {
        for (n, data) in out.chunks_mut(per * stride).enumerate() {
            let work = &work;
            scope.spawn(move || work(n * per, data));
        }
    });
    out
}

fn threads_for(width: u32, height: u32) -> usize {
    if u64::from(width) * u64::from(height) < 400_000 {
        return 1;
    }
    std::thread::available_parallelism().map_or(1, |n| n.get()).min(4)
}

/// One progressive frame from `cur` and its neighbours. `prev` and `next` are
/// `cur` itself at the ends of a stream. A frame of a layout other than the planar
/// 8 and 10-bit 4:2:0, 4:2:2 and 4:4:4 ones (I420, I422, I444, I010, I210, I410), or of different geometry from `cur`, comes back as it is.
pub fn filter_frame(
    prev: &VideoFrame,
    cur: &VideoFrame,
    next: &VideoFrame,
    keep: Keep,
    threads: usize,
) -> VideoFrame {
    filter_field(prev, cur, next, keep, false, threads)
}

/// One progressive picture of a field: `keep` is the field that survives. With
/// `second` false this is [`filter_frame`] (the first field of a frame, whose
/// missing lines are estimated from the previous and the current frame); with
/// `second` true it is bwdif's second field of the same frame, whose missing lines
/// are estimated from the current and the next frame. A double-rate output is the
/// first field with `Keep::first_of(order)`, then the other with `second` set.
pub fn filter_field(
    prev: &VideoFrame,
    cur: &VideoFrame,
    next: &VideoFrame,
    keep: Keep,
    second: bool,
    threads: usize,
) -> VideoFrame {
    let mut out = cur.clone();
    out.interlace = Interlace::Progressive;
    let wide = match cur.format {
        PixelFormat::I420 | PixelFormat::I422 | PixelFormat::I444 => false,
        PixelFormat::I010 | PixelFormat::I210 | PixelFormat::I410 => true,
        _ => return out,
    };
    let same = |f: &VideoFrame| {
        f.format == cur.format
            && f.width == cur.width
            && f.height == cur.height
            && f.planes.len() == cur.planes.len()
            && f.planes.iter().zip(&cur.planes).all(|(a, b)| a.stride == b.stride && a.data.len() == b.data.len())
    };
    if !same(prev) || !same(next) {
        return out;
    }
    let geometry = cur.format.plane_geometry(cur.width, cur.height);
    for (i, (row_bytes, rows)) in geometry.into_iter().enumerate() {
        let src = Src {
            prev: &prev.planes[i].data,
            cur: &cur.planes[i].data,
            next: &next.planes[i].data,
            p2: if second { &cur.planes[i].data } else { &prev.planes[i].data },
            n2: if second { &next.planes[i].data } else { &cur.planes[i].data },
            stride: cur.planes[i].stride,
            rows,
        };
        if rows < 2 {
            continue;
        }
        let data = if wide {
            plane::<P16>(src, row_bytes / 2, keep.missing(), threads)
        } else {
            plane::<P8>(src, row_bytes, keep.missing(), threads)
        };
        out.planes[i] = Plane::new(data, src.stride);
    }
    out
}

/// The decode layer's deinterlacing stage: pictures go in in display order with
/// a tag (their display index), progressive ones come out one input behind.
///
/// Disabled it is a pass-through that holds nothing and touches no byte, which
/// is what keeps every progressive source exactly what it was.
///
/// In field mode ([`Deinterlace::Field`](crate::project::Deinterlace)) every
/// frame comes out as two pictures, the first field's and then the second's,
/// tagged `2 * tag` and `2 * tag + 1`: the stage's tags are field indices from
/// then on, and so is `first_wanted`.
pub struct Deinterlacer {
    on: bool,
    /// Whether the stream is interlaced at all, filtered or not (`Off` leaves the
    /// fields woven, but the stream still has no resumable display index).
    interlaced: bool,
    fields: bool,
    /// Pictures tagged below this are consumed for their neighbours but never
    /// filtered or returned: the ones a seek decodes before its target.
    first_wanted: i64,
    prev: Option<VideoFrame>,
    cur: Option<(i64, VideoFrame)>,
    /// The order of the last picture that stated one; a picture that states
    /// none (a frame picture of an interlaced sequence) takes it.
    order: Interlace,
    threads: usize,
    /// MPEG-2: a picture coded progressive passes untouched; only the pictures that
    /// state a field order are filtered.
    flagged_only: bool,
    /// Pulldown removal ([`crate::ivtc`]): input pictures in, film frames out, tagged
    /// by output index. When set, nothing else here acts.
    ivtc: Option<crate::ivtc::Stage>,
}

impl Deinterlacer {
    /// `on` for a stream whose SPS says frame_mbs_only_flag = 0. `default_order`
    /// is what a picture with no order of its own takes until one states it.
    pub fn new(on: bool, first_wanted: i64, default_order: Interlace) -> Self {
        Self {
            on,
            interlaced: on,
            fields: false,
            first_wanted,
            prev: None,
            cur: None,
            order: if default_order.is_interlaced() { default_order } else { Interlace::TopFieldFirst },
            threads: 0,
            flagged_only: false,
            ivtc: None,
        }
    }

    /// For [`Deinterlace::Ivtc`] over `demuxer`'s file: the stage that removes its
    /// pulldown, tags and `first_wanted` being film-frame indices from then on. Any
    /// other mode, and a file with no cadence, leave the stage as it was.
    pub fn cadenced(mut self, mode: Deinterlace, demuxer: &Demuxer) -> Self {
        if mode == Deinterlace::Ivtc
            && let Some(cadence) =
                demuxer.path().and_then(|p| crate::ivtc::cadence_for(p, crate::ivtc::Telecine::On))
        {
            self.ivtc = Some(crate::ivtc::Stage::new(cadence, self.first_wanted));
            self.on = true;
            self.interlaced = true;
        }
        self
    }

    /// The stage for a clip's [`Deinterlace`] setting over a stream that is (or
    /// is not) `interlaced`. A progressive stream is a pass-through whatever the
    /// setting; `Off` is a pass-through (the woven pictures as decoded); `Frame`
    /// is [`Deinterlacer::new`]; `Field` doubles the rate, and `first_wanted` is
    /// then a field index (see [`Deinterlacer::landing`]).
    pub fn with_mode(mode: Deinterlace, interlaced: bool, first_wanted: i64, default_order: Interlace) -> Self {
        let on = interlaced && mode != Deinterlace::Off;
        Self {
            fields: on && mode == Deinterlace::Field,
            interlaced,
            ..Self::new(on, first_wanted, default_order)
        }
    }

    /// For MPEG-2 video, whose pictures each say whether they are interlaced.
    pub fn flagged_only(mut self) -> Self {
        self.flagged_only = true;
        self
    }

    /// Whether this stage filters at all.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Whether the stream is interlaced, filtered or not.
    pub fn is_interlaced(&self) -> bool {
        self.interlaced
    }

    /// Whether this stage doubles the rate: its tags and `first_wanted` count
    /// fields, two to each frame the demuxer counts.
    pub fn is_fields(&self) -> bool {
        self.fields
    }

    /// The display index of the frame a demuxer seek should land at or before for
    /// the first wanted picture `start` (a field index in field mode): the frame
    /// that holds it, and one more before it when the stage filters, so the target
    /// is filtered with its true predecessor.
    pub fn landing(&self, start: u32) -> u32 {
        if let Some(stage) = &self.ivtc {
            return stage.landing(start);
        }
        let frame = if self.fields { start / 2 } else { start };
        if self.on { frame.saturating_sub(1) } else { frame }
    }

    /// Takes the next picture in display order. Returns the picture that is now
    /// complete (its successor has arrived): the same one back when disabled,
    /// the previous one filtered when enabled, `None` while priming and for a
    /// picture below `first_wanted`. Frame mode only; a field-mode stage hands
    /// two pictures per frame, which [`push_all`](Self::push_all) returns.
    pub fn push(&mut self, tag: i64, pic: VideoFrame) -> Option<(i64, VideoFrame)> {
        self.push_all(tag, pic).into_iter().next()
    }

    /// As [`push`](Self::push), for any mode: zero, one or (field mode) two
    /// pictures, in display order.
    pub fn push_all(&mut self, tag: i64, pic: VideoFrame) -> Vec<(i64, VideoFrame)> {
        if let Some(stage) = &mut self.ivtc {
            return stage.push_all(tag, pic);
        }
        if !self.on {
            return vec![(tag, pic)];
        }
        if pic.interlace.is_interlaced() {
            self.order = pic.interlace;
        }
        let done = match self.cur.take() {
            Some((t, cur)) => {
                let out = self.emit(t, &cur, &pic);
                self.prev = Some(cur);
                out
            }
            None => Vec::new(),
        };
        self.cur = Some((tag, pic));
        done
    }

    /// Ends the stream: the last picture, with itself as its future.
    pub fn finish(&mut self) -> Option<(i64, VideoFrame)> {
        self.finish_all().into_iter().next()
    }

    /// As [`finish`](Self::finish), for any mode.
    pub fn finish_all(&mut self) -> Vec<(i64, VideoFrame)> {
        if let Some(stage) = &mut self.ivtc {
            return stage.finish_all();
        }
        if !self.on {
            return Vec::new();
        }
        let Some((t, cur)) = self.cur.take() else { return Vec::new() };
        let next = cur.clone();
        self.emit(t, &cur, &next)
    }

    fn emit(&mut self, tag: i64, cur: &VideoFrame, next: &VideoFrame) -> Vec<(i64, VideoFrame)> {
        let count = if self.fields { 2 } else { 1 };
        let base = if self.fields { 2 * tag } else { tag };
        if base + count <= self.first_wanted {
            return Vec::new();
        }
        if self.flagged_only && !cur.interlace.is_interlaced() {
            return vec![(tag, cur.clone())];
        }
        if self.threads == 0 {
            self.threads = threads_for(cur.width, cur.height);
        }
        let order = if cur.interlace.is_interlaced() { cur.interlace } else { self.order };
        let prev = self.prev.as_ref().unwrap_or(cur);
        let first = Keep::first_of(order);
        let second = match first {
            Keep::Top => Keep::Bottom,
            Keep::Bottom => Keep::Top,
        };
        let mut out = Vec::with_capacity(2);
        for k in 0..count {
            if base + k < self.first_wanted {
                continue;
            }
            let pic = match k {
                0 => filter_field(prev, cur, next, first, false, self.threads),
                _ => filter_field(prev, cur, next, second, true, self.threads),
            };
            out.push((base + k, pic));
        }
        out
    }
}

/// Whether `path` is interlaced H.264, which is the only thing a clip's
/// deinterlace setting acts on. Remembered per path.
pub fn is_interlaced(path: &Path) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static KNOWN: OnceLock<Mutex<HashMap<std::path::PathBuf, bool>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    if let Some(&v) = known.lock().ok().and_then(|k| k.get(path).copied()).as_ref() {
        return v;
    }
    let v = Demuxer::open(path)
        .is_ok_and(|(meta, d)| meta.codec == crate::demux::Codec::H264 && d.h264_interlaced());
    if let Ok(mut k) = known.lock() {
        k.insert(path.to_path_buf(), v);
    }
    v
}

/// The setting that actually applies to `path`: the clip's own for interlaced
/// H.264, [`Deinterlace::Frame`] for every other source (which ignores it). The two
/// pulldown settings resolve here: to [`Deinterlace::Ivtc`] when the file has a
/// cadence to remove (`IvtcAuto`: a clear one), else to `Frame`.
pub fn effective(mode: Deinterlace, path: &Path) -> Deinterlace {
    use crate::ivtc::{Telecine, cadence_for};
    match mode {
        Deinterlace::Ivtc if cadence_for(path, Telecine::On).is_some() => Deinterlace::Ivtc,
        Deinterlace::IvtcAuto if cadence_for(path, Telecine::Auto).is_some() => Deinterlace::Ivtc,
        Deinterlace::Ivtc | Deinterlace::IvtcAuto => Deinterlace::Frame,
        _ if mode != Deinterlace::Frame && !is_interlaced(path) => Deinterlace::Frame,
        _ => mode,
    }
}

/// Frames of a file with `frames` pictures, as the decode layer numbers them under
/// `mode` (already [`effective`]): fields in field mode, film frames under
/// [`Deinterlace::Ivtc`], pictures otherwise.
pub fn counted_frames(mode: Deinterlace, path: &Path, frames: u32) -> u32 {
    match mode {
        Deinterlace::Field => frames * 2,
        Deinterlace::Ivtc => crate::ivtc::cadence_for(path, crate::ivtc::Telecine::On)
            .map_or(frames, |c| c.out_count(frames)),
        _ => frames,
    }
}

/// `rate` (a file's frames per timeline frame) for the way `mode` (already
/// [`effective`]) numbers a file's pictures: doubled in field mode, four fifths of
/// it once pulldown is removed.
pub fn counted_rate(mode: Deinterlace, rate: crate::project::Rate) -> crate::project::Rate {
    match mode {
        Deinterlace::Field => fields_rate(rate),
        Deinterlace::Ivtc => film_rate(rate),
        _ => rate,
    }
}

/// The file's picture that decode-layer frame `n` (numbered as `mode`, already
/// [`effective`], numbers them) comes from.
pub fn file_frame(mode: Deinterlace, path: &Path, n: u32) -> u32 {
    match mode {
        Deinterlace::Field => n / 2,
        Deinterlace::Ivtc => crate::ivtc::cadence_for(path, crate::ivtc::Telecine::On).map_or(n, |c| c.input_of(n)),
        _ => n,
    }
}

/// `rate` for a file whose pulldown is removed: 4 film frames for every 5 pictures.
pub fn film_rate(rate: crate::project::Rate) -> crate::project::Rate {
    rate.then(crate::project::Rate::from_fps(24.0, 30.0).unwrap_or(crate::project::Rate::REAL_TIME))
}

/// `rate` (a file's frames per timeline frame) for a source whose pictures are
/// fields: twice as many of them per timeline frame, counted from the file's first
/// field. The one conversion a field-mode clip's frame mapping needs; the decode
/// layer's frame numbers are then field numbers (`2 * frame + 0 or 1`).
pub fn fields_rate(rate: crate::project::Rate) -> crate::project::Rate {
    rate.then(crate::project::Rate::from_fps(2.0, 1.0).unwrap_or(crate::project::Rate::REAL_TIME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, f: impl Fn(usize, usize) -> u8) -> VideoFrame {
        let mut v = VideoFrame::alloc(PixelFormat::I420, w, h);
        for (i, plane) in v.planes.iter_mut().enumerate() {
            let stride = plane.stride;
            let data: Vec<u8> = (0..plane.data.len())
                .map(|n| if i == 0 { f(n % stride, n / stride) } else { 128 })
                .collect();
            *plane = Plane::new(data, stride);
        }
        v.interlace = Interlace::TopFieldFirst;
        v
    }

    #[test]
    fn a_static_picture_keeps_its_kept_field_and_a_flat_one_is_unchanged() {
        let flat = frame(64, 64, |_, _| 90);
        let out = filter_frame(&flat, &flat, &flat, Keep::Top, 1);
        assert!(out.planes[0].data.iter().all(|&b| b == 90));
        let stripes = frame(64, 64, |_, y| (y * 3) as u8);
        let out = filter_frame(&stripes, &stripes, &stripes, Keep::Top, 1);
        // Kept (even) lines are untouched; a static picture's rebuilt lines are
        // its own (the temporal estimate), so nothing moves at all.
        assert!(out.planes[0].data == stripes.planes[0].data);
        assert_eq!(out.interlace, Interlace::Progressive);
    }

    #[test]
    fn a_disabled_stage_is_the_identity_with_no_delay() {
        let a = frame(32, 32, |x, y| (x + y) as u8);
        let mut d = Deinterlacer::new(false, 0, Interlace::Progressive);
        let (t, out) = d.push(7, a.clone()).expect("immediately");
        assert_eq!(t, 7);
        assert!(out == a);
        assert!(d.finish().is_none());
    }

    #[test]
    fn the_stage_runs_one_behind_and_drains_at_the_end() {
        let a = frame(32, 32, |x, y| (x + y) as u8);
        let mut d = Deinterlacer::new(true, 0, Interlace::Progressive);
        assert!(d.push(0, a.clone()).is_none());
        assert_eq!(d.push(1, a.clone()).map(|o| o.0), Some(0));
        assert_eq!(d.finish().map(|o| o.0), Some(1));
        assert!(d.finish().is_none());
    }

    #[test]
    fn threaded_and_single_thread_agree() {
        let f = |x: usize, y: usize| ((x * 7 + y * 13) ^ (y * y)) as u8;
        let (a, b, c) = (frame(200, 300, f), frame(200, 300, |x, y| f(x + 3, y)), frame(200, 300, |x, y| f(x + 6, y)));
        assert!(filter_frame(&a, &b, &c, Keep::Top, 1) == filter_frame(&a, &b, &c, Keep::Top, 4));
        assert!(filter_frame(&a, &b, &c, Keep::Bottom, 1) == filter_frame(&a, &b, &c, Keep::Bottom, 3));
    }
}

#[cfg(test)]
mod session_tests;
