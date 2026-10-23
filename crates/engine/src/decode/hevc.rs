//! The HEVC software seat: one span of `ec-h265` through the same [`Render`]
//! door every other seat's pictures go through, and the same way a refusal is
//! named on stderr. Export reads the same [`Stream`] to decode its sources.
//!
//! `ec-h265` takes one access unit at a time in Annex B framing, which is what
//! the demuxer hands out: the config record's VPS, SPS and PPS ride in front of
//! every sync sample, so a span can start on any of them. Pictures leave the
//! decoder in display (POC) order through its bumping process, so `index` counts
//! them as they come out -- the count the demuxer's landing index is made of. The
//! last pictures of a stream wait in the reorder buffer until the decoder is
//! flushed at end of stream.
//!
//! A span starts on an IRAP. A fresh decoder treats the picture it starts on as
//! a random-access point with NoRaslOutputFlag set, so the RASL pictures of a CRA
//! are skipped (8.1.3) and never come out. The RADL pictures of that IRAP do come
//! out, but ahead of it in display order: they sit before the landing picture on
//! the timeline, and so before any span target. They are dropped, and the index
//! counts from the landing picture on, where the demuxer put it.
//!
//! Each picture is cropped to its SPS conformance window and handed on as 8-bit
//! I420. A 10-bit picture downshifts by two (a 12-bit one by four), the same
//! truncation the AV1 seat and the hardware seat's P010 read-back take, so the
//! seats hand the renderer the same bytes off the same file. A 4:2:2 or 4:4:4
//! picture has its chroma averaged down to 4:2:0 by the export's `chroma_420`,
//! and a monochrome one gets neutral chroma.

use std::sync::mpsc::SyncSender;

use ec_h265::decode::DecodedPicture;
use ec_h265::{Decoder, Frame as Decoded};
use ec_h265_syntax::ConformanceWindow;

use super::{Abort, DecodeFailure, Frame, I420, LATE_RUN, Render, Wide, skip_for_speed};
use crate::demux::Demuxer;
use crate::project::Speed;

/// One HEVC decode, pulled a picture at a time: the decoder, the display index
/// of the next picture out, and the reusable crop buffers of the whole stream.
pub(crate) struct Stream {
    decoder: Decoder,
    /// Display index of the next picture out. Signed: the landing sync sample can
    /// sit inside what an mp4's edit list trims, i.e. before frame 0.
    index: i64,
    /// Set by the first picture out whose decode-order index is 0: the landing
    /// picture. Everything out before it is a leading picture of the landing IRAP.
    landed: bool,
    /// Set once the demuxer runs dry and the decoder has been flushed.
    flushed: bool,
    /// The cropped samples at their own depth, before they narrow to 8 bits.
    wide: [Vec<u16>; 3],
}

impl Stream {
    /// A decoder positioned at the sync sample at or before `start_frame`.
    pub(crate) fn open(demuxer: &mut Demuxer, start_frame: u32) -> Self {
        Self {
            decoder: Decoder::new(),
            index: demuxer.seek_to_sync_at_or_before(start_frame),
            landed: false,
            flushed: false,
            wide: Default::default(),
        }
    }

    /// The next picture in display order, narrowed into `out`, with its display
    /// index. `None` at end of stream. A refusal is named in words, and the
    /// pictures before it stand.
    pub(crate) fn next(
        &mut self,
        demuxer: &mut Demuxer,
        out: &mut I420,
    ) -> Result<Option<i64>, String> {
        self.next_with(demuxer, |decoded, due, wide| fill(decoded, due, wide, out))
    }

    /// The next picture in display order at its own depth, scaled to 10 bits
    /// (an 8-bit picture is shifted up by two), into `out`. The same stream and
    /// the same refusals as [`Stream::next`]; the export's 10-bit seat takes these
    /// planes, so nothing is lost to a narrowing that the file could carry.
    pub(crate) fn next_wide(
        &mut self,
        demuxer: &mut Demuxer,
        out: &mut Wide,
    ) -> Result<Option<i64>, String> {
        self.next_with(demuxer, |decoded, due, _| fill_wide(decoded, due, out))
    }

    /// The decode loop both outputs share: `sink` takes each picture with the
    /// stream's crop scratch, and the display index is returned once it has.
    fn next_with(
        &mut self,
        demuxer: &mut Demuxer,
        mut sink: impl FnMut(&Decoded, i64, &mut [Vec<u16>; 3]) -> Result<(), String>,
    ) -> Result<Option<i64>, String> {
        loop {
            if let Some(decoded) = self.decoder.next_frame() {
                if !self.landed {
                    if decoded.au_index != 0 {
                        continue;
                    }
                    self.landed = true;
                }
                if !carried(&decoded.picture) {
                    return Err(format!(
                        "software HEVC frame {} is {}/{}-bit luma/chroma, and only 8, 10 and 12 bits are carried",
                        self.index, decoded.picture.bit_depth, decoded.picture.bit_depth_c,
                    ));
                }
                let due = self.index;
                self.index += 1;
                sink(&decoded, due, &mut self.wide)?;
                return Ok(Some(due));
            }
            if self.flushed {
                return Ok(None);
            }
            match demuxer.next_access_unit() {
                Ok(Some(au)) => {
                    if let Err(e) = self.decoder.decode_annex_b(&au) {
                        return Err(format!(
                            "software HEVC decode failed at frame {}: {e}",
                            self.index
                        ));
                    }
                }
                // The demuxer is dry: flush releases every picture the reorder
                // delay still holds, and the loop above hands them out.
                Ok(None) => {
                    self.flushed = true;
                    if let Err(e) = self.decoder.flush() {
                        return Err(format!("software HEVC decode failed at end of stream: {e}"));
                    }
                }
                Err(e) => return Err(format!("demux error: {e}")),
            }
        }
    }
}

/// The HEVC software seat over `[start_frame, end_frame)`. A span decodes from
/// the sync sample at or before `start_frame`; pictures before the target are
/// decoded (the target references them) but never sent.
///
/// A refusal out of `ec-h265`, or a picture outside what edith carries (anything
/// but 4:2:0 at 8 or 10 bits), fails the span in words: named on stderr, the
/// pictures decoded so far stand, and no garbage is sent.
// The eight inputs every software span takes (`run_vp9` has the same shape); a
// one-off struct would only ever be built and taken apart here.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_hevc(
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

/// Queues one narrowed picture through the shared [`Render`]. `true` when the
/// consumer went away, so the span stops rather than keeps decoding into a void.
fn send(out: &I420, due: u32, render: &mut Render, tx: &SyncSender<Frame>) -> bool {
    let frame = render.frame(due, &out.y, &out.u, &out.v, out.width, out.height);
    tx.send(frame).is_err()
}

/// Whether edith carries this picture's shape: any chroma format `ec-h265`
/// decodes (4:0:0, 4:2:0, 4:2:2, 4:4:4) at 8, 10 or 12 bits for luma and chroma.
/// What `ec-h265` refuses (extended precision, above 12 bits, separate colour
/// planes) never reaches here.
fn carried(picture: &DecodedPicture) -> bool {
    picture.chroma_format_idc <= 3
        && matches!(picture.bit_depth, 8 | 10 | 12)
        && matches!(chroma_depth(picture), 8 | 10 | 12)
}

/// The depth of the chroma samples: a monochrome picture has none of its own, and
/// its neutral chroma is made at the luma depth.
fn chroma_depth(picture: &DecodedPicture) -> u32 {
    if picture.chroma_format_idc == 0 {
        picture.bit_depth
    } else {
        picture.bit_depth_c
    }
}

/// Crops `picture` to its conformance window, plane by plane, into 16-bit
/// samples at their own depth, with the chroma at 4:2:0 whatever the stream
/// codes: a 4:2:2 or 4:4:4 plane is averaged down by the export's `chroma_420`,
/// and a monochrome picture gets neutral chroma. The window's offsets are in
/// chroma units: `SubWidthC` by `SubHeightC` luma samples (2x2 in 4:2:0, 2x1 in
/// 4:2:2, 1x1 in 4:4:4 and 4:0:0). `None` when the window does not fit the
/// picture, which a corrupt SPS can say.
fn crop(
    picture: &DecodedPicture,
    window: &ConformanceWindow,
    y: &mut Vec<u16>,
    u: &mut Vec<u16>,
    v: &mut Vec<u16>,
) -> Option<(u32, u32)> {
    let (left, right) = (window.left as usize, window.right as usize);
    let (top, bottom) = (window.top as usize, window.bottom as usize);
    let (width, height) = (picture.width, picture.height);
    let idc = picture.chroma_format_idc;
    let sub_w = if matches!(idc, 1 | 2) { 2 } else { 1 };
    let sub_h = if idc == 1 { 2 } else { 1 };
    let out_w = width.checked_sub(sub_w * (left + right))?;
    let out_h = height.checked_sub(sub_h * (top + bottom))?;
    if out_w == 0 || out_h == 0 {
        return None;
    }
    region(
        &picture.y,
        width,
        (sub_w * left, sub_h * top),
        (out_w, out_h),
        y,
    )?;
    let (ow, oh) = (out_w.div_ceil(2), out_h.div_ceil(2));
    if idc == 0 {
        let neutral = 1u16 << (picture.bit_depth - 1);
        for plane in [&mut *u, &mut *v] {
            plane.clear();
            plane.resize(ow * oh, neutral);
        }
        return Some((out_w as u32, out_h as u32));
    }
    let chroma_stride = width.div_ceil(sub_w);
    let (cw, ch) = (out_w.div_ceil(sub_w), out_h.div_ceil(sub_h));
    for (src, dst) in [(&picture.cb, &mut *u), (&picture.cr, &mut *v)] {
        region(src, chroma_stride, (left, top), (cw, ch), dst)?;
        if (cw, ch) != (ow, oh) {
            let narrowed = crate::export::chroma_420(dst, cw, ch, ow, oh);
            dst.clear();
            dst.extend(narrowed.into_iter().map(|s| s as u16));
        }
    }
    Some((out_w as u32, out_h as u32))
}

/// One rectangle of a plane, copied row by row into `dst`. `None` when it runs
/// past the plane's samples.
fn region(
    src: &[u16],
    stride: usize,
    (x0, y0): (usize, usize),
    (w, h): (usize, usize),
    dst: &mut Vec<u16>,
) -> Option<()> {
    dst.clear();
    for row in y0..y0.checked_add(h)? {
        let start = row.checked_mul(stride)?.checked_add(x0)?;
        dst.extend_from_slice(src.get(start..start.checked_add(w)?)?);
    }
    Some(())
}

/// Narrows one cropped plane to 8 bits by the depth's downshift.
fn to_8bit(src: &[u16], shift: u32, dst: &mut Vec<u8>) {
    dst.clear();
    dst.extend(src.iter().map(|&sample| (sample >> shift) as u8));
}

/// Crops and narrows one picture into `out`. `Err` when the window does not fit
/// the picture.
/// One cropped picture at 10-bit scale, into the caller's planes. The crop is
/// the same as [`fill`]'s; an 8-bit picture is shifted up to the 10-bit scale.
fn fill_wide(decoded: &Decoded, due: i64, out: &mut Wide) -> Result<(), String> {
    let picture = &decoded.picture;
    let Some((w, h)) = crop(picture, &decoded.window, &mut out.y, &mut out.u, &mut out.v) else {
        return Err(format!(
            "software HEVC frame {due}: its conformance window does not fit the {}x{} picture",
            picture.width, picture.height,
        ));
    };
    // To the 10-bit scale: 8-bit shifts up by two, 12-bit down by two.
    let to_10 = |s: &mut u16, depth: u32| {
        *s = if depth <= 10 { *s << (10 - depth) } else { *s >> (depth - 10) };
    };
    for s in &mut out.y {
        to_10(s, picture.bit_depth);
    }
    for s in out.u.iter_mut().chain(out.v.iter_mut()) {
        to_10(s, chroma_depth(picture));
    }
    out.width = w;
    out.height = h;
    Ok(())
}

fn fill(
    decoded: &Decoded,
    due: i64,
    wide: &mut [Vec<u16>; 3],
    out: &mut I420,
) -> Result<(), String> {
    let picture = &decoded.picture;
    let [wy, wu, wv] = wide;
    let Some((w, h)) = crop(picture, &decoded.window, wy, wu, wv) else {
        return Err(format!(
            "software HEVC frame {due}: its conformance window does not fit the {}x{} picture",
            picture.width, picture.height,
        ));
    };
    let shift_luma = picture.bit_depth.saturating_sub(8);
    let shift_chroma = chroma_depth(picture).saturating_sub(8);
    to_8bit(wy, shift_luma, &mut out.y);
    to_8bit(wu, shift_chroma, &mut out.u);
    to_8bit(wv, shift_chroma, &mut out.v);
    out.width = w;
    out.height = h;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    /// The window's crop is taken off each plane in its own units: luma by two
    /// samples a chroma unit, chroma by one, and the samples are the ones that
    /// sat inside the window.
    #[test]
    fn crop_takes_the_conformance_window_out_of_the_coded_picture() {
        // A 16x8 coded picture whose luma sample at (x, y) is x + 16 * y.
        let luma: Vec<u16> = (0..8u16)
            .flat_map(|y| (0..16u16).map(move |x| x + 16 * y))
            .collect();
        let picture = DecodedPicture {
            width: 16,
            height: 8,
            bit_depth: 8,
            bit_depth_c: 8,
            chroma_format_idc: 1,
            y: luma,
            cb: vec![7; 8 * 4],
            cr: vec![9; 8 * 4],
        };
        // One chroma unit off the left and right, one off the bottom: luma loses
        // two samples each side and two off the bottom.
        let window = ConformanceWindow {
            left: 1,
            right: 1,
            top: 0,
            bottom: 1,
        };
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        assert_eq!(
            crop(&picture, &window, &mut y, &mut u, &mut v),
            Some((12, 6))
        );
        assert_eq!(y.len(), 12 * 6);
        assert_eq!(&y[..12], &[2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
        assert_eq!(y[12], 18, "the second row starts two samples in, at y = 1");
        assert_eq!(
            (u.len(), v.len()),
            (6 * 3, 6 * 3),
            "chroma crops by the same units"
        );
        assert!(u.iter().all(|&s| s == 7) && v.iter().all(|&s| s == 9));

        // A window that eats the whole picture is refused, not cropped to nothing.
        let all = ConformanceWindow {
            left: 4,
            right: 4,
            top: 0,
            bottom: 0,
        };
        assert_eq!(crop(&picture, &all, &mut y, &mut u, &mut v), None);
    }

    /// Every picture a linear decode releases, cropped to its window at its own
    /// depth, equals ffmpeg's decode plane for plane. The fixtures run from the
    /// 8-bit and Main 10 4:2:0 cases through an odd size and a CRA-led GOP.
    #[test]
    fn decoded_planes_match_ffmpeg_sample_for_sample() {
        let cases = [
            ("test_hevcsw_8.mp4", 352, 288, "yuv420p", 1),
            ("test_hevcsw_8.mkv", 352, 288, "yuv420p", 1),
            ("test_hevcsw_10.mp4", 352, 288, "yuv420p10le", 2),
            ("test_hevcsw_10.mkv", 352, 288, "yuv420p10le", 2),
            ("test_hevcsw_odd.mp4", 350, 286, "yuv420p", 1),
            ("test_hevcsw_cra.mp4", 352, 288, "yuv420p", 1),
        ];
        for (name, w, h, pix_fmt, bytes) in cases {
            let path = asset(name);
            let want = ffmpeg_planes(&path, w, h, pix_fmt, bytes);
            let got = ec_planes(&path);
            assert_eq!(got.len(), want.len(), "{name}: picture count");
            for (i, (g, e)) in got.iter().zip(&want).enumerate() {
                assert!(g == e, "{name}: picture {i} differs from ffmpeg");
            }
        }
    }

    /// A 4:2:2 picture crops with full-height chroma and comes out 4:2:0: the
    /// window's vertical offsets are luma rows, and each pair of chroma rows
    /// averages to one, an odd last row with itself.
    #[test]
    fn crop_takes_422_to_420_by_averaging_chroma_row_pairs() {
        let (w, h) = (16usize, 8usize);
        // Chroma is 8 wide and 8 tall here: row r holds 10 * r.
        let chroma: Vec<u16> = (0..8u16)
            .flat_map(|r| std::iter::repeat_n(10 * r, 8))
            .collect();
        let picture = DecodedPicture {
            width: w,
            height: h,
            bit_depth: 8,
            bit_depth_c: 8,
            chroma_format_idc: 2,
            y: vec![3; w * h],
            cb: chroma.clone(),
            cr: chroma,
        };
        assert!(carried(&picture));
        // One chroma unit off each side: luma loses two columns each side, and one
        // luma row off the bottom, as 4:2:2 has no vertical chroma unit.
        let window = ConformanceWindow {
            left: 1,
            right: 1,
            top: 0,
            bottom: 1,
        };
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        assert_eq!(
            crop(&picture, &window, &mut y, &mut u, &mut v),
            Some((12, 7))
        );
        assert_eq!(y.len(), 12 * 7);
        // Cropped chroma rows 0..7 hold 0, 10, .., 60; pairs (0,10), (20,30),
        // (40,50) and the odd row 60 with itself.
        let want: Vec<u16> = [5u16, 25, 45, 60]
            .iter()
            .flat_map(|&r| std::iter::repeat_n(r, 6))
            .collect();
        assert_eq!(u, want);
        assert_eq!(v, want);
    }

    /// The seat's own pictures for a whole file, each as 4:2:0 planes widened to
    /// 16 bits: 8-bit through the same [`Stream::next`] the span uses, 10-bit
    /// through [`Stream::next_wide`].
    fn seat_planes(path: &std::path::Path, bits: u32) -> Vec<[Vec<u16>; 3]> {
        let (_, mut demuxer) = crate::demux::Demuxer::open(path).expect("open the clip");
        let mut stream = Stream::open(&mut demuxer, 0);
        let mut out = Vec::new();
        if bits == 8 {
            let mut picture = I420::default();
            while stream
                .next(&mut demuxer, &mut picture)
                .expect("decode")
                .is_some()
            {
                out.push([
                    picture.y.iter().map(|&s| u16::from(s)).collect(),
                    picture.u.iter().map(|&s| u16::from(s)).collect(),
                    picture.v.iter().map(|&s| u16::from(s)).collect(),
                ]);
            }
        } else {
            let mut picture = Wide::default();
            while stream
                .next_wide(&mut demuxer, &mut picture)
                .expect("decode")
                .is_some()
            {
                out.push([picture.y.clone(), picture.u.clone(), picture.v.clone()]);
            }
        }
        out
    }

    /// Peak signal-to-noise ratio over every sample of every plane of every frame.
    fn psnr(got: &[[Vec<u16>; 3]], want: &[[Vec<u16>; 3]], peak: f64) -> f64 {
        let (mut squared, mut n) = (0f64, 0usize);
        for (g, w) in got.iter().zip(want) {
            for (gp, wp) in g.iter().zip(w) {
                for (&a, &b) in gp.iter().zip(wp) {
                    let d = f64::from(a) - f64::from(b);
                    squared += d * d;
                    n += 1;
                }
            }
        }
        10.0 * (peak * peak * n as f64 / squared).log10()
    }

    /// A 4:2:2 clip at `pix_fmt`, encoded by ffmpeg's libx265 into the temp dir:
    /// 30 frames of 352x288. `assets/` is gitignored, so the clip is made here.
    fn gen_422(name: &str, pix_fmt: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{name}-{}.mp4", std::process::id()));
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=352x288:rate=30:duration=1")
            .args(["-frames:v", "30", "-c:v", "libx265", "-pix_fmt", pix_fmt])
            .args(["-x265-params", "log-level=error"])
            .arg(&path)
            .output()
            .expect("run ffmpeg: the clips are encoded with it");
        assert!(
            out.status.success(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        path
    }

    /// Every range-extensions shape decodes on the seat to 4:2:0 planes within a
    /// PSNR of ffmpeg's own decode of the same file at the same narrowing, frame
    /// for frame, every frame there is. 8-bit pictures compare at 8 bits; 10 and
    /// 12-bit ones at the 10-bit scale `next_wide` hands on. Monochrome chroma is
    /// the neutral value exactly.
    #[test]
    fn rext_streams_decode_to_420_within_psnr_of_ffmpeg() {
        for (src, dst, bits, bytes, floor) in [
            ("yuv422p", "yuv420p", 8, 1, 45.0),
            ("yuv422p10le", "yuv420p10le", 10, 2, 45.0),
            ("yuv422p12le", "yuv420p10le", 10, 2, 45.0),
            ("yuv444p", "yuv420p", 8, 1, 38.0),
            ("yuv444p10le", "yuv420p10le", 10, 2, 38.0),
            ("yuv444p12le", "yuv420p10le", 10, 2, 38.0),
            ("gray", "gray", 8, 1, 45.0),
            ("gray10le", "gray10le", 10, 2, 45.0),
            ("gray12le", "gray10le", 10, 2, 45.0),
        ] {
            let clip = gen_422(&format!("hevcrext-unit-{src}"), src);
            let got = seat_planes(&clip, bits);
            let want = ffmpeg_planes(&clip, 352, 288, dst, bytes);
            let _ = std::fs::remove_file(&clip);
            assert_eq!(got.len(), 30, "{src}: every frame comes out");
            assert_eq!(got.len(), want.len(), "{src}: picture count");
            let db = psnr(&got, &want, f64::from((1u32 << bits) - 1));
            eprintln!("{src} -> {dst} against ffmpeg: {db:.2} dB");
            assert!(db > floor, "{src} is {db:.2} dB from ffmpeg, want above {floor}");
            if src.starts_with("gray") {
                let neutral = 1u16 << (bits - 1);
                assert!(
                    got.iter().all(|f| f[1].iter().chain(&f[2]).all(|&s| s == neutral)),
                    "{src}: chroma is neutral"
                );
            }
        }
    }

    /// Every picture a linear decode releases, cropped as the seat crops it.
    fn ec_planes(path: &std::path::Path) -> Vec<[Vec<u16>; 3]> {
        let (_, mut demuxer) = crate::demux::Demuxer::open(path).expect("open the fixture");
        let mut decoder = Decoder::new();
        let mut out = Vec::new();
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        let mut take = |decoder: &mut Decoder, out: &mut Vec<[Vec<u16>; 3]>| {
            while let Some(decoded) = decoder.next_frame() {
                assert!(carried(&decoded.picture), "fixture outside 8/10-bit 4:2:0");
                crop(&decoded.picture, &decoded.window, &mut y, &mut u, &mut v)
                    .expect("the window fits the picture");
                out.push([y.clone(), u.clone(), v.clone()]);
            }
        };
        while let Some(au) = demuxer.next_access_unit().expect("read") {
            decoder.decode_annex_b(&au).expect("decode");
            take(&mut decoder, &mut out);
        }
        decoder.flush().expect("flush");
        take(&mut decoder, &mut out);
        out
    }

    /// ffmpeg's own decode of the same file, split into planes, each sample
    /// widened to 16 bits so it compares with [`ec_planes`] directly.
    fn ffmpeg_planes(
        path: &std::path::Path,
        w: usize,
        h: usize,
        pix_fmt: &str,
        bytes: usize,
    ) -> Vec<[Vec<u16>; 3]> {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args([
                "-f",
                "rawvideo",
                "-pix_fmt",
                pix_fmt,
                "-fps_mode",
                "passthrough",
                "-",
            ])
            .output()
            .expect("run ffmpeg: the fixtures are made with it, see scripts/gen_fixtures.sh");
        assert!(out.status.success(), "ffmpeg failed on {}", path.display());
        // A gray target is luma alone (ffmpeg's yuv420p of gray would also move
        // the range), so the chroma planes come back empty and are not compared.
        let gray = pix_fmt.starts_with("gray");
        let (cw, ch) = if gray { (0, 0) } else { (w.div_ceil(2), h.div_ceil(2)) };
        let frame = (w * h + 2 * cw * ch) * bytes;
        assert_eq!(
            out.stdout.len() % frame,
            0,
            "ffmpeg handed back a partial frame"
        );
        out.stdout
            .chunks_exact(frame)
            .map(|f| {
                let wide: Vec<u16> = if bytes == 1 {
                    f.iter().map(|&s| u16::from(s)).collect()
                } else {
                    f.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|p| u16::from_le_bytes(*p))
                        .collect()
                };
                let (y, uv) = wide.split_at(w * h);
                let (u, v) = uv.split_at(cw * ch);
                [y.to_vec(), u.to_vec(), v.to_vec()]
            })
            .collect()
    }
}
