//! Inverse telecine ("remove pulldown"): 29.97i film that carries 23.976p pictures
//! in 3:2 pulldown comes back as the 23.976p pictures.
//!
//! **Hard pulldown** is baked into the pictures. Four film frames A B C D become
//! five interlaced frames `AA BB BC CD DD` (field pairs; the 2-3 pattern, and its
//! rotations such as `AA AB BC CC DD`): three of the five are clean, and two
//! consecutive ones are mixed, each holding one field of C and one of another
//! film frame. Within every block of five, the mixed pair starts at the input
//! frame whose index is `phase` modulo five, so
//!
//! * the clean frames are the film frames as they are,
//! * the mixed pair `(m, m + 1)` is one film frame: the second field of `m` and the
//!   first field of `m + 1` (a field match, ffmpeg's `fieldmatch`), woven together,
//! * the second frame of the pair is dropped (the decimation, ffmpeg's `decimate`):
//!   five frames in, four out.
//!
//! The mapping of output frame `k` to input frames is then pure arithmetic
//! ([`Cadence::input_of`]), so a seek lands on the same picture a sequential decode
//! gives, and the output timeline is exactly 4/5 of the input's frame rate.
//!
//! The phase and the field order are found once per file by [`cadence_for`], from the
//! first [`SAMPLE`] pictures: for each candidate phase the woven film frame must be
//! free of combing where its two source frames are combed, in every block that has
//! motion in it. Material that is truly interlaced (every frame combed) never
//! satisfies that, so [`Telecine::Auto`] leaves it to the deinterlacer. One cadence
//! is assumed for the whole file: a cadence broken by an edit is not followed.
//!
//! **Soft pulldown** (MPEG-2 `repeat_first_field`, H.264 `pic_struct`) leaves the
//! film frames coded as they are and only asks the player to repeat fields; the
//! decoder counts coded pictures, which are the 23.976p frames, so the only work is
//! to state their rate ([`Repeats`] finds the flags; the transport stream reader
//! states the rate).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ec_core::registry::{CodecId, CodecParameters, Decoder};
use ec_core::{Frame, Interlace, Packet, PixelFormat, Plane, TimeBase, VideoFrame};

use crate::demux::{Codec, Demuxer};

/// Pictures looked at to find the cadence: twelve blocks of five.
pub const SAMPLE: usize = 60;

/// Where the mixed pair of a block of five starts, and which field displays first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    /// Input frames `i` with `i % 5 == phase` start a mixed pair.
    pub phase: u8,
    /// `TopFieldFirst` or `BottomFieldFirst`.
    pub order: Interlace,
}

impl Cadence {
    fn skipped(self, i: u32) -> bool {
        (i % 5 + 5 - u32::from(self.phase)) % 5 == 1
    }

    fn starts_pair(self, i: u32) -> bool {
        i % 5 == u32::from(self.phase)
    }

    /// The four offsets in a block of five that produce an output.
    fn offsets(self) -> [u32; 4] {
        let mut out = [0; 4];
        let mut n = 0;
        for k in 0..5 {
            if !self.skipped(k) {
                out[n] = k;
                n += 1;
            }
        }
        out
    }

    /// The input frame that starts output frame `k` (for a woven one, the first of
    /// its two).
    pub fn input_of(self, k: u32) -> u32 {
        5 * (k / 4) + self.offsets()[(k % 4) as usize]
    }

    /// The output frame input frame `i` produces (starts), `None` for the dropped one.
    pub fn output_of(self, i: u32) -> Option<u32> {
        if self.skipped(i) {
            return None;
        }
        Some(4 * (i / 5) + self.offsets().iter().filter(|&&o| o < i % 5).count() as u32)
    }

    /// How many output frames `frames` input frames make.
    pub fn out_count(self, frames: u32) -> u32 {
        4 * (frames / 5) + self.offsets().iter().filter(|&&o| o < frames % 5).count() as u32
    }
}

/// The film frame in a mixed pair: `first` supplies the rows of the field that
/// displays first, `second` those of the one that displays second (the pair's first
/// frame, which holds the film frame as its second field).
pub fn weave(first: &VideoFrame, second: &VideoFrame, order: Interlace) -> VideoFrame {
    let mut out = first.clone();
    out.interlace = Interlace::Progressive;
    let parity = if order == Interlace::BottomFieldFirst { 0 } else { 1 };
    let alike = first.format == second.format
        && first.width == second.width
        && first.height == second.height
        && first.planes.len() == second.planes.len();
    if !alike {
        return out;
    }
    let geometry = first.format.plane_geometry(first.width, first.height);
    for (i, (row_bytes, rows)) in geometry.into_iter().enumerate() {
        let (stride, src) = (out.planes[i].stride, &second.planes[i]);
        if src.stride != stride || src.data.len() != out.planes[i].data.len() {
            continue;
        }
        let mut data = out.planes[i].data.to_vec();
        for y in (parity..rows).step_by(2) {
            let (a, b) = (y * stride, y * stride + row_bytes);
            if b <= data.len() {
                data[a..b].copy_from_slice(&src.data[a..b]);
            }
        }
        out.planes[i] = Plane::new(data, stride);
    }
    out
}

/// The decode layer's stage: input frames in display order with their display index,
/// the film frames out, tagged with their output index.
pub struct Stage {
    cadence: Cadence,
    /// Output frames below this are dropped (a seek decodes from earlier).
    first_wanted: i64,
    /// The first frame of a mixed pair, waiting for its second.
    held: Option<(i64, VideoFrame)>,
}

impl Stage {
    pub fn new(cadence: Cadence, first_wanted: i64) -> Self {
        Self { cadence, first_wanted, held: None }
    }

    /// The input frame a demuxer seek should land at or before for output `start`.
    pub fn landing(&self, start: u32) -> u32 {
        self.cadence.input_of(start)
    }

    fn solo(&self, pic: &VideoFrame) -> VideoFrame {
        let keep = crate::deinterlace::Keep::first_of(self.cadence.order);
        crate::deinterlace::filter_frame(pic, pic, pic, keep, 1)
    }

    fn tagged(&self, i: u32, pic: VideoFrame) -> Vec<(i64, VideoFrame)> {
        match self.cadence.output_of(i) {
            Some(k) if i64::from(k) >= self.first_wanted => vec![(i64::from(k), pic)],
            _ => Vec::new(),
        }
    }

    pub fn push_all(&mut self, tag: i64, pic: VideoFrame) -> Vec<(i64, VideoFrame)> {
        let Ok(i) = u32::try_from(tag) else { return Vec::new() };
        let mut out = Vec::new();
        if let Some((t, orphan)) = self.held.take() {
            if t + 1 == tag {
                // `pic` is the second of the pair.
                let film = weave(&pic, &orphan, self.cadence.order);
                out.extend(self.tagged(t as u32, film));
                return out;
            }
            let solo = self.solo(&orphan);
            out.extend(self.tagged(t as u32, solo));
        }
        if self.cadence.skipped(i) {
            // Its mate is not in hand (a seek landed on it): nothing to show.
        } else if self.cadence.starts_pair(i) {
            self.held = Some((tag, pic));
        } else {
            let mut clean = pic;
            clean.interlace = Interlace::Progressive;
            out.extend(self.tagged(i, clean));
        }
        out
    }

    pub fn finish_all(&mut self) -> Vec<(i64, VideoFrame)> {
        match self.held.take() {
            Some((t, orphan)) => {
                let solo = self.solo(&orphan);
                self.tagged(t as u32, solo)
            }
            None => Vec::new(),
        }
    }
}

// ----------------------------------------------------------------------------
// Detection

/// How a clip's pulldown removal is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Telecine {
    /// Only a cadence that holds clearly over the sample.
    Auto,
    /// The best cadence the sample has, however weak.
    On,
}

/// Luma at 8 bits, tightly packed, of a planar picture.
fn luma(f: &VideoFrame) -> Option<(Vec<u8>, usize, usize)> {
    let (w, h) = (f.width as usize, f.height as usize);
    let plane = f.planes.first()?;
    let wide = match f.format {
        PixelFormat::I420 | PixelFormat::I422 | PixelFormat::I444 => false,
        PixelFormat::I010 | PixelFormat::I210 | PixelFormat::I410 => true,
        _ => return None,
    };
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = plane.data.get(y * plane.stride..)?;
        if wide {
            out.extend((0..w).map(|x| (u16::from_le_bytes([row[2 * x], row[2 * x + 1]]) >> 2) as u8));
        } else {
            out.extend_from_slice(row.get(..w)?);
        }
    }
    Some((out, w, h))
}

/// The share of pixels that stick out of both rows next to them, the signature of
/// two fields from different moments woven together.
fn comb(l: &[u8], w: usize, h: usize) -> f64 {
    const T: i32 = 12;
    let mut n = 0usize;
    for y in 1..h.saturating_sub(1) {
        let (a, b, c) = (&l[(y - 1) * w..y * w], &l[y * w..(y + 1) * w], &l[(y + 1) * w..(y + 2) * w]);
        for x in 0..w {
            let (a, b, c) = (i32::from(a[x]), i32::from(b[x]), i32::from(c[x]));
            if (b - a > T && b - c > T) || (a - b > T && c - b > T) {
                n += 1;
            }
        }
    }
    n as f64 / (w * h).max(1) as f64
}

/// Luma of the film frame a mixed pair `(a, b)` makes under `order`.
fn woven_luma(a: &[u8], b: &[u8], w: usize, h: usize, order: Interlace) -> Vec<u8> {
    let parity = if order == Interlace::BottomFieldFirst { 0 } else { 1 };
    let mut out = b.to_vec();
    for y in (parity..h).step_by(2) {
        out[y * w..(y + 1) * w].copy_from_slice(&a[y * w..(y + 1) * w]);
    }
    out
}

/// The cadence of `frames` (display order), if there is one.
///
/// The best cadence the pictures show, and whether it holds clearly: in at least four
/// blocks with motion in them, and in nine of ten of those.
pub fn detect(frames: &[VideoFrame]) -> Option<(Cadence, bool)> {
    let planes: Vec<(Vec<u8>, usize, usize)> = frames.iter().map(luma).collect::<Option<_>>()?;
    let n = planes.len();
    if n < 10 {
        return None;
    }
    let (w, h) = (planes[0].1, planes[0].2);
    if planes.iter().any(|p| p.1 != w || p.2 != h) || h < 8 {
        return None;
    }
    let cf: Vec<f64> = planes.iter().map(|p| comb(&p.0, w, h)).collect();
    let mut best: Option<(Cadence, usize, usize, f64)> = None;
    for order in [Interlace::TopFieldFirst, Interlace::BottomFieldFirst] {
        let cw: Vec<f64> = (0..n - 1)
            .map(|i| comb(&woven_luma(&planes[i].0, &planes[i + 1].0, w, h, order), w, h))
            .collect();
        for phase in 0..5u8 {
            let (mut informative, mut passed, mut margin) = (0usize, 0usize, 0.0f64);
            let mut i = usize::from(phase);
            while i + 1 < n {
                let hi = cf[i].min(cf[i + 1]);
                let lo = (i + 2..(i + 5).min(n)).fold(cw[i], |m, j| m.max(cf[j]));
                if hi > 0.0004 {
                    informative += 1;
                    if hi >= 3.0 * lo {
                        passed += 1;
                    }
                }
                margin += hi - lo;
                i += 5;
            }
            let c = Cadence { phase, order };
            let better = match &best {
                None => true,
                Some((_, p, _, m)) => (passed, margin) > (*p, *m),
            };
            if better {
                best = Some((c, passed, informative, margin));
            }
        }
    }
    let (c, passed, informative, margin) = best?;
    Some((c, informative >= 4 && passed * 10 >= informative * 9 && margin > 0.0))
}

/// The cadence a file's first pictures show, remembered per file. Only software-decoded interlaced H.264 and MPEG-2 have one.
pub fn cadence_for(path: &Path, how: Telecine) -> Option<Cadence> {
    type Known = Mutex<HashMap<PathBuf, Option<(Cadence, bool)>>>;
    static KNOWN: OnceLock<Known> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    let cached = known.lock().ok().and_then(|k| k.get(path).copied());
    let found = match cached {
        Some(found) => found,
        None => {
            let found = sample(path).and_then(|frames| detect(&frames));
            if let Ok(mut k) = known.lock() {
                k.insert(path.to_path_buf(), found);
            }
            found
        }
    };
    found.filter(|(_, clear)| *clear || how == Telecine::On).map(|(c, _)| c)
}

/// The first [`SAMPLE`] pictures of an interlaced H.264 or MPEG-2 file, decoded.
fn sample(path: &Path) -> Option<Vec<VideoFrame>> {
    let (meta, mut demuxer) = Demuxer::open(path).ok()?;
    if !demuxer.h264_interlaced() {
        return None;
    }
    demuxer.seek_to_sync_at_or_before(0);
    match meta.codec {
        Codec::H264 => {
            let d = ec_h264::H264Decoder::new(CodecParameters::new(CodecId::H264)).ok()?;
            pull(&mut demuxer, d)
        }
        Codec::Mpeg2 => {
            let d = ec_mpeg2::Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).ok()?;
            pull(&mut demuxer, d)
        }
        _ => None,
    }
}

fn pull<D: Decoder>(demuxer: &mut Demuxer, mut decoder: D) -> Option<Vec<VideoFrame>> {
    let mut frames = Vec::new();
    let take = |decoder: &mut D, frames: &mut Vec<VideoFrame>| loop {
        match decoder.receive_frame() {
            Ok(Frame::Video(pic)) => frames.push(pic),
            Ok(_) => {}
            Err(_) => return,
        }
    };
    while frames.len() < SAMPLE {
        match demuxer.next_access_unit() {
            Ok(Some(au)) => {
                decoder.send_packet(&Packet::new(0, TimeBase::new(1, 1), au)).ok()?;
                take(&mut decoder, &mut frames);
            }
            _ => break,
        }
    }
    if frames.len() < SAMPLE {
        decoder.flush().ok()?;
        take(&mut decoder, &mut frames);
    }
    frames.truncate(SAMPLE);
    Some(frames)
}

// ----------------------------------------------------------------------------
// Soft pulldown

/// What a stream's first pictures say about repeating fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Repeats {
    /// Pictures that state it either way.
    pub pictures: u32,
    /// Pictures that ask for a third field: MPEG-2 `repeat_first_field`, H.264
    /// `pic_struct` 5 and 6.
    pub tripled: u32,
}

impl Repeats {
    /// 3:2 soft pulldown asks for a third field on every other picture; a stream
    /// that does it on about half of them, over enough of them, is film.
    pub fn soft(self) -> bool {
        self.pictures >= 8 && self.tripled * 10 >= self.pictures * 3 && self.tripled * 10 <= self.pictures * 7
    }

    /// Takes one MPEG-2 picture (an access unit) in: its picture coding extension's
    /// `repeat_first_field`, for a frame picture coded progressive.
    pub fn mpeg2(&mut self, au: &[u8]) {
        let mut i = 0;
        while let Some(at) = au[i..].windows(5).position(|w| w[..4] == [0, 0, 1, 0xB5] && w[4] >> 4 == 8) {
            let ext = &au[i + at + 4..];
            if ext.len() >= 5 {
                let (frame_picture, rff, progressive) = (ext[2] & 3 == 3, ext[3] >> 1 & 1 == 1, ext[4] >> 7 == 1);
                if frame_picture && progressive {
                    self.pictures += 1;
                    self.tripled += u32::from(rff);
                }
                return;
            }
            i += at + 4;
        }
    }

    /// Takes one H.264 access unit in: the `pic_struct` of its picture timing SEI.
    /// `sets` is the stream's SPS and PPS (Annex B), which say whether it is there.
    pub fn h264(&mut self, sets: &[u8], au: &[u8]) {
        use ec_h264_syntax::nal::{AnnexBIter, unescape_rbsp};
        let Some(vui) = AnnexBIter::new(sets)
            .find(|n| n.first().is_some_and(|b| b & 0x1f == 7))
            .and_then(|n| {
                let mut rbsp = Vec::new();
                unescape_rbsp(&n[1..], &mut rbsp);
                ec_h264_syntax::Sps::parse(&rbsp).ok()
            })
            .and_then(|sps| sps.vui)
        else {
            return;
        };
        if !vui.pic_struct_present {
            return;
        }
        let delays = vui.nal_hrd.as_ref().or(vui.vcl_hrd.as_ref()).map(|h| {
            (u32::from(h.cpb_removal_delay_length_minus1) + 1, u32::from(h.dpb_output_delay_length_minus1) + 1)
        });
        for nal in AnnexBIter::new(au).filter(|n| n.first().is_some_and(|b| b & 0x1f == 6)) {
            let mut rbsp = Vec::new();
            unescape_rbsp(&nal[1..], &mut rbsp);
            let mut at = 0;
            while at + 2 <= rbsp.len() {
                let mut kind = 0usize;
                while rbsp.get(at) == Some(&0xFF) {
                    kind += 255;
                    at += 1;
                }
                kind += usize::from(*rbsp.get(at).unwrap_or(&0));
                at += 1;
                let mut size = 0usize;
                while rbsp.get(at) == Some(&0xFF) {
                    size += 255;
                    at += 1;
                }
                size += usize::from(*rbsp.get(at).unwrap_or(&0));
                at += 1;
                if kind == 1 {
                    let skip = delays.map_or(0, |(a, b)| a + b) as usize;
                    // pic_struct is the four bits after the delays.
                    let (byte, bit) = (at + skip / 8, skip % 8);
                    if let Some(&b) = rbsp.get(byte) {
                        let next = rbsp.get(byte + 1).copied().unwrap_or(0);
                        let pic_struct = (u16::from_be_bytes([b, next]) >> (12 - bit)) & 0xF;
                        self.pictures += 1;
                        self.tripled += u32::from(pic_struct == 5 || pic_struct == 6);
                    }
                    return;
                }
                at += size;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpeg2_repeat_flags_are_counted() {
        // A picture coding extension: frame picture, tff, repeat_first_field, progressive_frame.
        let au = |rff: bool| [0, 0, 1, 0xB5, 0x8F, 0xFF, 0xF3, 0x80 | (u8::from(rff) << 1), 0x80];
        let mut r = Repeats::default();
        for n in 0..12 {
            r.mpeg2(&au(n % 2 == 0));
        }
        assert_eq!(r, Repeats { pictures: 12, tripled: 6 });
        assert!(r.soft());
        let mut still = Repeats::default();
        for _ in 0..12 {
            still.mpeg2(&au(false));
        }
        assert!(!still.soft(), "no repeats is not pulldown");
    }

    #[test]
    fn offsets_cover_four_of_five() {
        for phase in 0..5u8 {
            let c = Cadence { phase, order: Interlace::TopFieldFirst };
            let mut k = 0;
            for i in 0..50u32 {
                match c.output_of(i) {
                    Some(o) => {
                        assert_eq!(o, k, "phase {phase} input {i}");
                        assert_eq!(c.input_of(o), i);
                        k += 1;
                    }
                    None => assert!(c.skipped(i)),
                }
            }
            assert_eq!(c.out_count(50), k);
            assert_eq!(c.out_count(50), 40);
            assert_eq!(c.out_count(7), (0..7).filter(|&i| !c.skipped(i)).count() as u32);
        }
    }
}
