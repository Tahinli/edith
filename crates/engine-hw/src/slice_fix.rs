//! Repairs the two counters the H.264 driver writes wrong.
//!
//! cros-codecs hands `frame_num` and `pic_order_cnt_lsb` to VA-API in the
//! picture and slice parameter buffers, and mesa radeonsi (26.1.7, measured on
//! an RX 9060 XT) writes the slice header itself with **both at zero on every
//! picture** -- the same driver habit already noted for HEVC in `lib.rs`. The
//! stream then breaks 7.4.3: consecutive reference pictures with the same
//! `frame_num` are a gap a decoder must conceal, and every picture claiming
//! output order 0 leaves a decoder that bumps by POC -- which is what a phone's
//! stateful hardware decoder does -- to emit pictures in bursts. Desktop
//! decoders (ffmpeg, openh264, the VA-API one on this very card) ignore both
//! fields for a one-reference stream and play it clean, which is why the
//! defect was only ever seen on a phone: three real exports judder every few
//! seconds in every player there, before and after a WhatsApp transcode.
//!
//! Both fields are fixed-width (`u(v)`, widths from the SPS), so the repair is
//! an in-place bit rewrite: unescape the NAL, set the bits, re-escape. The
//! values are what cros-codecs meant: the picture's index since the last IDR,
//! modulo `MaxFrameNum`, and twice that modulo `MaxPicOrderCntLsb` -- POC type
//! 0 with no B-frames, so output order is decode order.
//!
//! Level is the other header field that was wrong ([`h264_level`],
//! [`hevc_level`]): `L4` was hard-coded, which a 3840x1608 picture overflows
//! (Table A-1 caps 4.0 at 8192 macroblocks a frame).

use cros_codecs::codec::h264::parser::Level;
use cros_codecs::codec::h265::parser::Level as H265Level;

const NAL_SLICE: u8 = 1;
const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;

/// What the slice header rewrite needs from the SPS, plus the running count.
#[derive(Debug, Default, Clone, Copy)]
pub struct SliceCounters {
    /// `log2_max_frame_num_minus4 + 4`; 0 until an SPS has been seen.
    log2_max_frame_num: u8,
    /// `log2_max_pic_order_cnt_lsb_minus4 + 4` when `pic_order_cnt_type == 0`,
    /// else 0 (no `pic_order_cnt_lsb` field in the slice header).
    log2_max_poc_lsb: u8,
    /// `frame_mbs_only_flag`; a field-coded stream carries more fields before
    /// `pic_order_cnt_lsb` than this rewrites, so it is left alone.
    frame_mbs_only: bool,
    /// Pictures since the last IDR, the IDR itself being 0: the POC runs on
    /// this (7.4.3, POC type 0: `pic_order_cnt_lsb` of frame n is 2n).
    count: u32,
    /// *Reference* pictures since the last IDR: `frame_num` runs on this one,
    /// because 7.4.3 has it step only after a picture with `nal_ref_idc != 0`.
    /// The vendored encoder marks every picture a reference, so the two agree
    /// today; kept apart so a non-reference picture is numbered right the day
    /// the encoder writes one.
    refs: u32,
}

impl SliceCounters {
    /// Rewrites every slice in one access unit. An SPS in the unit is parsed
    /// first (cros-codecs writes one at every IDR and only there). A unit whose
    /// SPS cannot be read, or that arrives before any SPS, is left untouched.
    pub fn fix_au(&mut self, au: &mut Vec<u8>) {
        let nals = nal_ranges(au);
        for &(start, end) in &nals {
            if au[start] & 0x1f == NAL_SPS {
                if let Some(parsed) = parse_sps(&unescape(&au[start + 1..end])) {
                    *self = SliceCounters { count: self.count, refs: self.refs, ..parsed };
                }
            }
        }
        if self.log2_max_frame_num == 0 || !self.frame_mbs_only {
            return;
        }
        // An IDR restarts the count; every slice of one picture takes the same
        // numbers, so the count steps once per access unit, after the rewrite.
        if nals.iter().any(|&(s, _)| au[s] & 0x1f == NAL_IDR) {
            self.count = 0;
            self.refs = 0;
        }
        let frame_num = self.refs & ((1u32 << self.log2_max_frame_num) - 1);
        let poc_lsb = (self.count.wrapping_mul(2)) & ((1u32 << self.log2_max_poc_lsb) - 1);
        let mut out = Vec::with_capacity(au.len() + 8);
        let mut cursor = 0;
        for &(start, end) in &nals {
            out.extend_from_slice(&au[cursor..start]);
            let kind = au[start] & 0x1f;
            if kind == NAL_SLICE || kind == NAL_IDR {
                out.push(au[start]);
                let mut rbsp = unescape(&au[start + 1..end]);
                if rewrite_slice(&mut rbsp, kind == NAL_IDR, self, frame_num, poc_lsb) {
                    escape(&rbsp, &mut out);
                } else {
                    out.extend_from_slice(&au[start + 1..end]);
                }
            } else {
                out.extend_from_slice(&au[start..end]);
            }
            cursor = end;
        }
        out.extend_from_slice(&au[cursor..]);
        *au = out;
        self.count = self.count.wrapping_add(1);
        let is_ref = |&(s, _): &(usize, usize)| {
            matches!(au[s] & 0x1f, NAL_SLICE | NAL_IDR) && au[s] & 0x60 != 0
        };
        if nals.iter().any(is_ref) {
            self.refs = self.refs.wrapping_add(1);
        }
    }
}

/// `(first byte after the start code, end)` of every NAL in an Annex B unit.
fn nal_ranges(au: &[u8]) -> Vec<(usize, usize)> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= au.len() {
        if au[i..i + 3] == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut ranges = Vec::with_capacity(starts.len());
    for (n, &s) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(au.len(), |&next| next - 3);
        // A four-byte start code's leading zero belongs to the separator.
        while end > s && au[end - 1] == 0 && n + 1 < starts.len() {
            end -= 1;
        }
        ranges.push((s, end));
    }
    ranges.retain(|&(s, e)| e > s);
    ranges
}

fn unescape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut zeros = 0usize;
    for &b in bytes {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

fn escape(rbsp: &[u8], out: &mut Vec<u8>) {
    let mut zeros = 0usize;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(u32::from(bit))
    }
    fn bits(&mut self, n: u8) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        Some(if k % 2 == 1 { (k as i32 + 1) / 2 } else { -(k as i32 / 2) })
    }
}

fn put_bits(data: &mut [u8], pos: usize, width: u8, value: u32) {
    for i in 0..usize::from(width) {
        let bit = (value >> (usize::from(width) - 1 - i)) & 1;
        let at = pos + i;
        let mask = 0x80 >> (at % 8);
        if bit == 1 {
            data[at / 8] |= mask;
        } else {
            data[at / 8] &= !mask;
        }
    }
}

/// Reads the SPS fields the rewrite depends on (§7.3.2.1.1) from an unescaped
/// payload (NAL header byte already stripped).
fn parse_sps(rbsp: &[u8]) -> Option<SliceCounters> {
    let mut b = Bits::new(rbsp);
    let profile_idc = b.bits(8)?;
    b.bits(8)?; // constraint flags + reserved
    b.bits(8)?; // level_idc
    b.ue()?; // seq_parameter_set_id
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        let chroma_format_idc = b.ue()?;
        if chroma_format_idc == 3 {
            b.bit()?; // separate_colour_plane_flag
        }
        b.ue()?; // bit_depth_luma_minus8
        b.ue()?; // bit_depth_chroma_minus8
        b.bit()?; // qpprime_y_zero_transform_bypass_flag
        if b.bit()? == 1 {
            // seq_scaling_matrix_present_flag: never written by cros-codecs, and
            // walking the lists is not worth carrying for a stream that has them.
            return None;
        }
    }
    let log2_max_frame_num = u8::try_from(b.ue()? + 4).ok()?;
    let poc_type = b.ue()?;
    let log2_max_poc_lsb = match poc_type {
        0 => u8::try_from(b.ue()? + 4).ok()?,
        1 => {
            b.bit()?;
            b.se()?;
            b.se()?;
            let cycle = b.ue()?;
            for _ in 0..cycle {
                b.se()?;
            }
            0
        }
        _ => 0,
    };
    if log2_max_frame_num > 16 || log2_max_poc_lsb > 16 {
        return None;
    }
    b.ue()?; // max_num_ref_frames
    b.bit()?; // gaps_in_frame_num_value_allowed_flag
    b.ue()?; // pic_width_in_mbs_minus1
    b.ue()?; // pic_height_in_map_units_minus1
    let frame_mbs_only = b.bit()? == 1;
    Some(SliceCounters {
        log2_max_frame_num,
        log2_max_poc_lsb,
        frame_mbs_only,
        count: 0,
        refs: 0,
    })
}

/// Sets `frame_num` and (POC type 0) `pic_order_cnt_lsb` in one unescaped
/// slice payload (§7.3.3). `false` where the header could not be walked, in
/// which case the payload is unchanged.
fn rewrite_slice(
    rbsp: &mut [u8],
    idr: bool,
    sps: &SliceCounters,
    frame_num: u32,
    poc_lsb: u32,
) -> bool {
    let (frame_num_at, poc_at) = {
        let mut b = Bits::new(rbsp);
        let mut walk = || -> Option<(usize, Option<usize>)> {
            b.ue()?; // first_mb_in_slice
            b.ue()?; // slice_type
            b.ue()?; // pic_parameter_set_id
            let frame_num_at = b.pos;
            b.bits(sps.log2_max_frame_num)?;
            if idr {
                b.ue()?; // idr_pic_id
            }
            let poc_at = (sps.log2_max_poc_lsb > 0).then_some(b.pos);
            if let Some(at) = poc_at {
                if at + usize::from(sps.log2_max_poc_lsb) > rbsp.len() * 8 {
                    return None;
                }
            }
            Some((frame_num_at, poc_at))
        };
        match walk() {
            Some(v) => v,
            None => return false,
        }
    };
    put_bits(rbsp, frame_num_at, sps.log2_max_frame_num, frame_num);
    if let Some(at) = poc_at {
        put_bits(rbsp, at, sps.log2_max_poc_lsb, poc_lsb);
    }
    true
}

/// The lowest H.264 level (Table A-1) whose frame size and macroblock rate
/// hold `width`x`height` at `fps`; 6.2 past the table.
pub fn h264_level(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Level {
    const LEVELS: [(u8, u64, u64); 17] = [
        (10, 1485, 99),
        (11, 3000, 396),
        (12, 6000, 396),
        (13, 11880, 396),
        (20, 11880, 396),
        (21, 19800, 792),
        (22, 20250, 1620),
        (30, 40500, 1620),
        (31, 108000, 3600),
        (32, 216000, 5120),
        (40, 245760, 8192),
        (41, 245760, 8192),
        (42, 522240, 8704),
        (50, 589824, 22080),
        (51, 983040, 36864),
        (52, 2073600, 36864),
        (60, 4177920, 139264),
    ];
    let frame_mbs = u64::from(width.div_ceil(16)) * u64::from(height.div_ceil(16));
    let fps = f64::from(fps_num.max(1)) / f64::from(fps_den.max(1));
    let mbps = (frame_mbs as f64 * fps).ceil() as u64;
    let idc = LEVELS
        .iter()
        .find(|&&(_, max_mbps, max_fs)| mbps <= max_mbps && frame_mbs <= max_fs)
        .map_or(62, |&(idc, ..)| idc);
    Level::try_from(idc).unwrap_or(Level::L6_2)
}

/// The lowest HEVC level (Table A.8, Main tier) whose luma picture size and
/// sample rate hold the picture; 6.2 past the table.
pub fn hevc_level(width: u32, height: u32, fps_num: u32, fps_den: u32) -> H265Level {
    const LEVELS: [(H265Level, u64, u64); 13] = [
        (H265Level::L1, 36_864, 552_960),
        (H265Level::L2, 122_880, 3_686_400),
        (H265Level::L2_1, 245_760, 7_372_800),
        (H265Level::L3, 552_960, 16_588_800),
        (H265Level::L3_1, 983_040, 33_177_600),
        (H265Level::L4, 2_228_224, 66_846_720),
        (H265Level::L4_1, 2_228_224, 133_693_440),
        (H265Level::L5, 8_912_896, 267_386_880),
        (H265Level::L5_1, 8_912_896, 534_773_760),
        (H265Level::L5_2, 8_912_896, 1_069_547_520),
        (H265Level::L6, 35_651_584, 1_069_547_520),
        (H265Level::L6_1, 35_651_584, 2_139_095_040),
        (H265Level::L6_2, 35_651_584, 4_278_190_080),
    ];
    let luma_ps = u64::from(width) * u64::from(height);
    let fps = f64::from(fps_num.max(1)) / f64::from(fps_den.max(1));
    let luma_sr = (luma_ps as f64 * fps).ceil() as u64;
    LEVELS
        .iter()
        .find(|&&(_, max_ps, max_sr)| luma_ps <= max_ps && luma_sr <= max_sr)
        .map_or(H265Level::L6_2, |&(level, ..)| level)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Writer {
        bytes: Vec<u8>,
        bits: usize,
    }
    impl Writer {
        fn new() -> Self {
            Self { bytes: Vec::new(), bits: 0 }
        }
        fn u(&mut self, width: u8, v: u32) {
            for i in 0..usize::from(width) {
                if self.bits % 8 == 0 {
                    self.bytes.push(0);
                }
                if (v >> (usize::from(width) - 1 - i)) & 1 == 1 {
                    let at = self.bits;
                    self.bytes[at / 8] |= 0x80 >> (at % 8);
                }
                self.bits += 1;
            }
        }
        fn ue(&mut self, v: u32) {
            let k = v + 1;
            let len = 32 - k.leading_zeros() as u8;
            self.u(len - 1, 0);
            self.u(len, k);
        }
        fn done(mut self) -> Vec<u8> {
            self.u(1, 1); // rbsp trailing bit
            self.bytes
        }
    }

    /// Main profile SPS the way cros-codecs writes one at a 2 s GOP and 30 fps:
    /// MaxFrameNum 32, MaxPocLsb 64, frame_mbs_only.
    fn sps() -> Vec<u8> {
        let mut w = Writer::new();
        w.u(8, 77);
        w.u(8, 0);
        w.u(8, 40);
        w.ue(0); // sps id
        w.ue(1); // log2_max_frame_num_minus4
        w.ue(0); // poc type
        w.ue(2); // log2_max_pic_order_cnt_lsb_minus4
        w.ue(1); // max_num_ref_frames
        w.u(1, 0);
        w.ue(119);
        w.ue(67);
        w.u(1, 1); // frame_mbs_only
        w.u(1, 1);
        w.u(1, 0);
        w.u(1, 0);
        let mut nal = vec![0, 0, 0, 1, 0x67];
        escape(&w.done(), &mut nal);
        nal
    }

    /// A slice header as the driver writes it (both counters zero), with
    /// `tail` bytes of "macroblock data" after it.
    fn slice(idr: bool, tail: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.ue(0); // first_mb_in_slice
        w.ue(if idr { 7 } else { 5 }); // slice_type I / P
        w.ue(0); // pps id
        w.u(5, 0); // frame_num
        if idr {
            w.ue(0); // idr_pic_id
        }
        w.u(6, 0); // pic_order_cnt_lsb
        let mut rbsp = w.done();
        rbsp.extend_from_slice(tail);
        let mut nal = vec![0, 0, 0, 1, if idr { 0x65 } else { 0x41 }];
        escape(&rbsp, &mut nal);
        nal
    }

    /// Reads the two counters back out of an escaped slice NAL.
    fn read(nal: &[u8]) -> (u32, u32) {
        let (start, end) = nal_ranges(nal)[0];
        let idr = nal[start] & 0x1f == NAL_IDR;
        let rbsp = unescape(&nal[start + 1..end]);
        let mut b = Bits::new(&rbsp);
        b.ue().unwrap();
        b.ue().unwrap();
        b.ue().unwrap();
        let frame_num = b.bits(5).unwrap();
        if idr {
            b.ue().unwrap();
        }
        (frame_num, b.bits(6).unwrap())
    }

    #[test]
    fn frame_num_holds_across_a_non_reference_picture() {
        // 7.4.3: a non-reference picture (nal_ref_idc 0) takes PrevRefFrameNum
        // + 1 like any other, but it is not a reference, so the picture after
        // it takes the same frame_num. The POC still steps every picture.
        let mut fix = SliceCounters::default();
        let mut idr = sps();
        idr.extend(slice(true, &[]));
        fix.fix_au(&mut idr);
        let mut p = slice(false, &[]);
        fix.fix_au(&mut p);
        assert_eq!(read(&p), (1, 2));
        let mut b = slice(false, &[]);
        b[4] = 0x01; // nal_ref_idc 0, type 1
        fix.fix_au(&mut b);
        assert_eq!(read(&b), (2, 4));
        let mut p = slice(false, &[]);
        fix.fix_au(&mut p);
        assert_eq!(read(&p), (2, 6), "the picture after a non-reference keeps its frame_num");
    }

    #[test]
    fn counters_step_per_picture_and_restart_at_idr() {
        let mut fix = SliceCounters::default();
        let mut idr = sps();
        idr.extend(slice(true, &[0xAB; 40]));
        fix.fix_au(&mut idr);
        let slice_nal = nal_ranges(&idr)[1];
        assert_eq!(read(&idr[slice_nal.0 - 4..]), (0, 0));
        let mut seen = Vec::new();
        for _ in 0..40 {
            let mut p = slice(false, &[0xCD; 40]);
            fix.fix_au(&mut p);
            seen.push(read(&p));
        }
        // frame_num wraps at MaxFrameNum 32, POC lsb at 64 -- both as 8.2.1 derives.
        assert_eq!(seen[0], (1, 2));
        assert_eq!(seen[30], (31, 62));
        assert_eq!(seen[31], (0, 0));
        assert_eq!(seen[32], (1, 2));
        let mut again = sps();
        again.extend(slice(true, &[]));
        fix.fix_au(&mut again);
        let at = nal_ranges(&again)[1].0 - 4;
        assert_eq!(read(&again[at..]), (0, 0), "an IDR restarts the count");
        let mut p = slice(false, &[]);
        fix.fix_au(&mut p);
        assert_eq!(read(&p), (1, 2));
    }

    #[test]
    fn macroblock_data_survives_byte_for_byte() {
        let mut fix = SliceCounters::default();
        let mut idr = sps();
        idr.extend(slice(true, &[]));
        fix.fix_au(&mut idr);
        // A tail that the escaper has to touch both before and after: the
        // rewritten header must not disturb it.
        let tail = [0, 0, 1, 0, 0, 3, 0, 0, 0, 7, 0xFF, 0, 0];
        let mut p = slice(false, &tail);
        let before = unescape(&p[5..]);
        fix.fix_au(&mut p);
        let after = unescape(&p[5..]);
        assert_eq!(before.len(), after.len());
        assert_eq!(&before[3..], &after[3..]);
        assert_eq!(&after[after.len() - tail.len()..], &tail);
    }

    #[test]
    fn nothing_is_touched_before_an_sps() {
        let mut fix = SliceCounters::default();
        let mut p = slice(false, &[1, 2, 3]);
        let before = p.clone();
        fix.fix_au(&mut p);
        assert_eq!(p, before);
    }

    #[test]
    fn levels_follow_table_a1() {
        assert_eq!(h264_level(1920, 1080, 30, 1), Level::L4);
        assert_eq!(h264_level(1920, 1080, 60, 1), Level::L4_2);
        assert_eq!(h264_level(3840, 1608, 24000, 1001), Level::L5_1);
        assert_eq!(h264_level(3840, 2160, 60, 1), Level::L5_2);
        assert_eq!(h264_level(640, 360, 30, 1), Level::L3);
        assert_eq!(hevc_level(1920, 1080, 30, 1), H265Level::L4);
        assert_eq!(hevc_level(3840, 2160, 30, 1), H265Level::L5);
        assert_eq!(hevc_level(3840, 2160, 60, 1), H265Level::L5_1);
    }
}
