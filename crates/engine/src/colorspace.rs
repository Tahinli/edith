//! What the numbers in a decoded picture *mean*: which YUV->RGB matrix they
//! were coded against, which transfer curve, and whether they span the full
//! byte or the limited 16..235 range. Everything here is read off the file;
//! nothing here converts anything yet.
//!
//! Three tiers, in this order, per field:
//!
//! 1. the **container**'s own tags -- Matroska `Colour`, mp4 `colr` -- which is
//!    what a remuxer rewrites and therefore the most recently asserted truth,
//! 2. the **bitstream**'s: an H.264 or HEVC SPS VUI `colour_description`, an
//!    AV1 sequence header `color_config`, which is what the *encoder* wrote and
//!    what survives a container that dropped its tags,
//! 3. the **resolution heuristic** ffmpeg and mpv both fall back to: a picture
//!    720 lines or taller is BT.709, anything smaller is BT.601, limited range
//!    either way.
//!
//! Per field, not per file: a Matroska that tags `Range` and nothing else
//! leaves the matrix to the SPS below it. A code this does not know -- reserved,
//! unspecified, or a matrix with no implementation here -- is the same as an
//! absent one and falls to the next tier, which is the whole reason the tiers
//! carry [`Option`]s rather than resolved values.
//!
//! Code values are ISO/IEC 23091-2 (= ITU-T H.273), the one table Matroska, mp4
//! `colr`, H.264 VUI, HEVC VUI and AV1 `color_config` all point at.

use crate::demux::Codec;

/// The YUV->RGB matrix a stream's chroma was coded against. Only the three that
/// exist in practice: SD (BT.601), HD (BT.709) and UHD/HDR (BT.2020
/// non-constant-luminance). Constant-luminance BT.2020 is not here because no
/// encoder writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
    Bt2020Ncl,
}

/// The transfer curve the samples are on. `Sdr` covers every curve that is
/// displayed as-is -- BT.709's, sRGB's, SMPTE 170M's, which differ by less than
/// a display's own calibration -- against the two HDR ones, which do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    Sdr,
    /// SMPTE ST 2084, "PQ": what an HDR10 file is on.
    Pq,
    /// ARIB STD-B67, "HLG": broadcast HDR.
    Hlg,
}

/// What one video stream's samples mean, resolved through all three tiers.
/// [`Default`] is what this engine assumed of every file before it read any of
/// this: BT.601, SDR, limited range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorDescription {
    pub matrix: Matrix,
    pub transfer: Transfer,
    /// `true` when the samples use all 256 codes (JPEG/"pc" range) rather than
    /// the broadcast 16..235 luma / 16..240 chroma.
    pub full_range: bool,
}

impl Default for ColorDescription {
    fn default() -> Self {
        Self {
            matrix: Matrix::Bt601,
            transfer: Transfer::Sdr,
            full_range: false,
        }
    }
}

impl ColorDescription {
    /// The three tiers collapsed into one answer. `height` is the coded picture
    /// height the heuristic reads when nothing else said anything.
    pub fn resolve(container: Tags, bitstream: Tags, height: u32) -> Self {
        let tags = container.over(bitstream);
        Self {
            // The ffmpeg/mpv rule, and the reason it is a rule: an untagged
            // 1080p file is HD material and an untagged 480p one came off SD
            // material, and guessing the other way is a visible green/magenta
            // shift on faces.
            matrix: tags.matrix.unwrap_or(if height >= 720 {
                Matrix::Bt709
            } else {
                Matrix::Bt601
            }),
            transfer: tags.transfer.unwrap_or(Transfer::Sdr),
            full_range: tags.full_range.unwrap_or(false),
        }
    }
}

/// One tier's worth of colour tags: what that tier said, field by field, with
/// [`None`] for "said nothing" and for "said something this does not implement".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tags {
    pub matrix: Option<Matrix>,
    pub transfer: Option<Transfer>,
    pub full_range: Option<bool>,
}

impl Tags {
    /// H.273 code points, as every container and every bitstream writes them.
    /// `range` is the Matroska `Range` code -- 0 unspecified, 1 limited, 2 full,
    /// 3 "whatever the matrix implies", which is a non-answer here. A source
    /// that carries a plain `video_full_range_flag` instead passes `1 + flag`.
    pub fn from_codes(matrix: u64, transfer: u64, range: u64) -> Self {
        Self {
            matrix: match matrix {
                1 => Some(Matrix::Bt709),
                // 5 is BT.470BG (PAL), 6 SMPTE 170M (NTSC): one matrix, two
                // names for the two halves of the world's SD television.
                5 | 6 => Some(Matrix::Bt601),
                9 => Some(Matrix::Bt2020Ncl),
                // 0 unspecified, 2 reserved, 4 FCC, 7 SMPTE 240M, 10 BT.2020
                // constant-luminance, 14 ICtCp: nothing this renders with.
                _ => None,
            },
            transfer: match transfer {
                16 => Some(Transfer::Pq),
                18 => Some(Transfer::Hlg),
                // Unspecified and reserved say nothing; every other assigned
                // curve is displayed the way an SDR one is here.
                0 | 2 => None,
                _ => Some(Transfer::Sdr),
            },
            full_range: match range {
                1 => Some(false),
                2 => Some(true),
                _ => None,
            },
        }
    }

    /// This tier's answers, with `next`'s used for whatever this one left open.
    fn over(self, next: Self) -> Self {
        Self {
            matrix: self.matrix.or(next.matrix),
            transfer: self.transfer.or(next.transfer),
            full_range: self.full_range.or(next.full_range),
        }
    }
}

/// What the *encoder* wrote into the stream itself, out of the parameter sets
/// the demuxers already hold: the SPS VUI for H.264 and HEVC (Annex-B framed,
/// which is how both demuxers keep them) and the sequence header OBU for AV1.
///
/// Empty [`Tags`] for VP9, whose colour space lives in the uncompressed header
/// of every frame rather than out of band, and for anything malformed: this is
/// a middle tier, and the tier below it always has an answer.
pub fn bitstream_tags(codec: Codec, sets: &[u8]) -> Tags {
    match codec {
        // 7 is an H.264 SPS, 33 an HEVC one (whose NAL header is two bytes and
        // whose type sits in bits 1..6 of the first).
        Codec::H264 => nal_units(sets)
            .find(|n| n.first().is_some_and(|b| b & 0x1f == 7))
            .map(|n| h264_sps(&rbsp(&n[1..])))
            .unwrap_or_default(),
        Codec::Hevc => nal_units(sets)
            .find(|n| n.first().is_some_and(|b| (b >> 1) & 0x3f == 33))
            .filter(|n| n.len() > 2)
            .map(|n| hevc_sps(&rbsp(&n[2..])))
            .unwrap_or_default(),
        Codec::Av1 => av1_sequence_header(sets),
        Codec::Vp9 => Tags::default(),
    }
}

/// The Annex-B NAL units in `buf`, payloads only: start codes are three or four
/// bytes and both are written in practice (the demuxers write four, an in-band
/// stream may carry either).
fn nal_units(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut at = 0;
    std::iter::from_fn(move || {
        // Find the next start code, then the one after it, which is where this
        // unit ends.
        let start = (at..buf.len().saturating_sub(2)).find(|&i| buf[i..i + 3] == [0, 0, 1])? + 3;
        let end = (start..buf.len().saturating_sub(2))
            .find(|&i| buf[i..i + 3] == [0, 0, 1])
            .map(|i| {
                if i > start && buf[i - 1] == 0 {
                    i - 1
                } else {
                    i
                }
            })
            .unwrap_or(buf.len());
        at = end;
        Some(&buf[start..end])
    })
}

/// A NAL payload with its emulation-prevention bytes removed (H.264 §7.4.1.1,
/// H.265 §7.4.2): a `00 00 03` in the coded bytes is a `00 00` that would
/// otherwise have looked like a start code, and the `03` is not part of the
/// syntax.
fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros == 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// MSB-first bit reader over an RBSP or an OBU. Reads past the end return zero
/// and raise [`Bits::overrun`], which every parser here checks before it
/// believes anything it read -- a truncated parameter set must fall to the next
/// tier, not report the colour of whatever the zeros decoded to.
struct Bits<'a> {
    buf: &'a [u8],
    /// Bit position, MSB of byte 0 first.
    at: usize,
    over: bool,
}

impl<'a> Bits<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            at: 0,
            over: false,
        }
    }

    fn overrun(&self) -> bool {
        self.over
    }

    fn bit(&mut self) -> u32 {
        let Some(&byte) = self.buf.get(self.at / 8) else {
            self.over = true;
            return 0;
        };
        let bit = u32::from(byte >> (7 - self.at % 8)) & 1;
        self.at += 1;
        bit
    }

    /// `u(n)`, up to 32 bits.
    fn u(&mut self, n: u32) -> u32 {
        (0..n).fold(0, |acc, _| (acc << 1) | self.bit())
    }

    /// Bits nothing here reads, in one step -- profile/tier records and the
    /// like, which run to 88 bits at a time.
    fn skip(&mut self, n: usize) {
        self.at += n;
        if self.at > self.buf.len() * 8 {
            self.over = true;
        }
    }

    /// `ue(v)`, unsigned exp-Golomb: N leading zeros, a one, then N more bits.
    /// Capped at 32 leading zeros, which is past any legal value and is what
    /// keeps a garbage set from spinning here.
    fn ue(&mut self) -> u32 {
        let mut zeros = 0;
        while self.bit() == 0 {
            zeros += 1;
            if zeros >= 32 || self.over {
                self.over = true;
                return 0;
            }
        }
        ((1u32 << zeros) - 1).wrapping_add(self.u(zeros))
    }

    /// `se(v)`, signed exp-Golomb: the unsigned code zig-zagged.
    fn se(&mut self) -> i32 {
        let k = self.ue();
        if k % 2 == 0 {
            -((k / 2) as i32)
        } else {
            ((k + 1) / 2) as i32
        }
    }
}

/// H.264 `seq_parameter_set_data` (ITU-T H.264 §7.3.2.1.1) walked as far as the
/// VUI's `colour_description` (Annex E, §E.1.1). Everything before it is read
/// only to know how many bits it took -- which is why the scaling lists matter:
/// a High-profile SPS that carries them and is walked as if it did not lands the
/// VUI read somewhere in the middle of a coefficient.
fn h264_sps(rbsp: &[u8]) -> Tags {
    let mut b = Bits::new(rbsp);
    let profile_idc = b.u(8);
    b.skip(8 + 8); // constraint flags + reserved, level_idc
    b.ue(); // seq_parameter_set_id
    // The chroma/depth block exists only in the profiles that can code
    // something other than 8-bit 4:2:0 (§7.3.2.1.1, the profile_idc list).
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        let chroma_format_idc = b.ue();
        if chroma_format_idc == 3 {
            b.bit(); // separate_colour_plane_flag
        }
        b.ue(); // bit_depth_luma_minus8
        b.ue(); // bit_depth_chroma_minus8
        b.bit(); // qpprime_y_zero_transform_bypass_flag
        if b.bit() == 1 {
            // seq_scaling_matrix_present_flag: 8 lists (12 for 4:4:4), the
            // first six 16 coefficients long and the rest 64 (§7.3.2.1.1.1).
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if b.bit() == 1 {
                    scaling_list(&mut b, if i < 6 { 16 } else { 64 });
                }
            }
        }
    }
    b.ue(); // log2_max_frame_num_minus4
    match b.ue() {
        // pic_order_cnt_type
        0 => {
            b.ue(); // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            b.bit(); // delta_pic_order_always_zero_flag
            b.se(); // offset_for_non_ref_pic
            b.se(); // offset_for_top_to_bottom_field
            let cycle = b.ue();
            if cycle > 255 {
                return Tags::default();
            }
            for _ in 0..cycle {
                b.se(); // offset_for_ref_frame[i]
            }
        }
        _ => {}
    }
    b.ue(); // max_num_ref_frames
    b.bit(); // gaps_in_frame_num_value_allowed_flag
    b.ue(); // pic_width_in_mbs_minus1
    b.ue(); // pic_height_in_map_units_minus1
    if b.bit() == 0 {
        b.bit(); // mb_adaptive_frame_field_flag, when !frame_mbs_only_flag
    }
    b.bit(); // direct_8x8_inference_flag
    if b.bit() == 1 {
        // frame_cropping_flag: four offsets
        for _ in 0..4 {
            b.ue();
        }
    }
    if b.bit() == 0 {
        return Tags::default(); // no VUI at all
    }
    vui_colour(&mut b)
}

/// One H.264 scaling list (§7.3.2.1.1.1): `size` deltas, the run stopping early
/// once a `nextScale` of zero says the rest of the list is the default one.
fn scaling_list(b: &mut Bits, size: usize) {
    let mut next = 8;
    for _ in 0..size {
        if next != 0 {
            next = (next + b.se() + 256) % 256;
        }
        if b.overrun() {
            return;
        }
    }
}

/// HEVC `seq_parameter_set_rbsp` (ITU-T H.265 §7.3.2.2.1) to its VUI
/// (§E.2.1). The two places bits are easy to lose are the profile/tier record
/// (§7.3.3) and the short-term reference picture sets (§7.3.7), both of which
/// are counted here rather than read.
fn hevc_sps(rbsp: &[u8]) -> Tags {
    let mut b = Bits::new(rbsp);
    b.skip(4); // sps_video_parameter_set_id
    let max_sub_layers_minus1 = b.u(3);
    b.bit(); // sps_temporal_id_nesting_flag
    profile_tier_level(&mut b, max_sub_layers_minus1);
    b.ue(); // sps_seq_parameter_set_id
    let chroma_format_idc = b.ue();
    if chroma_format_idc == 3 {
        b.bit(); // separate_colour_plane_flag
    }
    b.ue(); // pic_width_in_luma_samples
    b.ue(); // pic_height_in_luma_samples
    if b.bit() == 1 {
        // conformance_window_flag
        for _ in 0..4 {
            b.ue();
        }
    }
    b.ue(); // bit_depth_luma_minus8
    b.ue(); // bit_depth_chroma_minus8
    let log2_max_poc_lsb = b.ue() + 4;
    let ordering_info = b.bit() == 1;
    let first = if ordering_info {
        0
    } else {
        max_sub_layers_minus1
    };
    for _ in first..=max_sub_layers_minus1 {
        b.ue(); // sps_max_dec_pic_buffering_minus1
        b.ue(); // sps_max_num_reorder_pics
        b.ue(); // sps_max_latency_increase_plus1
    }
    for _ in 0..6 {
        // log2_min_luma_coding_block_size_minus3 through
        // max_transform_hierarchy_depth_intra: six plain ue(v)s.
        b.ue();
    }
    if b.bit() == 1 && b.bit() == 1 {
        // scaling_list_enabled_flag && sps_scaling_list_data_present_flag
        scaling_list_data(&mut b);
    }
    b.bit(); // amp_enabled_flag
    b.bit(); // sample_adaptive_offset_enabled_flag
    if b.bit() == 1 {
        // pcm_enabled_flag
        b.skip(8); // pcm_sample_bit_depth_luma/chroma_minus1
        b.ue(); // log2_min_pcm_luma_coding_block_size_minus3
        b.ue(); // log2_diff_max_min_pcm_luma_coding_block_size
        b.bit(); // pcm_loop_filter_disabled_flag
    }
    let sets = b.ue();
    if sets > 64 || b.overrun() {
        return Tags::default();
    }
    let mut counts = Vec::with_capacity(sets as usize);
    for i in 0..sets as usize {
        st_ref_pic_set(&mut b, i, &mut counts);
        if b.overrun() {
            return Tags::default();
        }
    }
    if b.bit() == 1 {
        // long_term_ref_pics_present_flag
        let lt = b.ue();
        if lt > 64 {
            return Tags::default();
        }
        for _ in 0..lt {
            b.skip(log2_max_poc_lsb as usize); // lt_ref_pic_poc_lsb_sps[i]
            b.bit(); // used_by_curr_pic_lt_sps_flag[i]
        }
    }
    b.bit(); // sps_temporal_mvp_enabled_flag
    b.bit(); // strong_intra_smoothing_enabled_flag
    if b.bit() == 0 {
        return Tags::default(); // no VUI at all
    }
    vui_colour(&mut b)
}

/// `profile_tier_level` (§7.3.3), counted: 2+1+5 bits of general profile, 32
/// compatibility flags, 48 bits of source/reserved flags and an 8-bit level,
/// then one 88-bit record per sub-layer that declares one.
fn profile_tier_level(b: &mut Bits, max_sub_layers_minus1: u32) {
    b.skip(2 + 1 + 5 + 32 + 48 + 8);
    let mut present = [(false, false); 8];
    for slot in present.iter_mut().take(max_sub_layers_minus1 as usize) {
        *slot = (b.bit() == 1, b.bit() == 1);
    }
    if max_sub_layers_minus1 > 0 {
        // reserved_zero_2bits up to eight sub-layers.
        b.skip(2 * (8 - max_sub_layers_minus1 as usize));
    }
    for &(profile, level) in present.iter().take(max_sub_layers_minus1 as usize) {
        if profile {
            b.skip(88);
        }
        if level {
            b.skip(8);
        }
    }
}

/// `scaling_list_data` (§7.3.4): four sizes of six matrices each (the last size
/// has two), either predicted from an earlier matrix or written out as deltas.
fn scaling_list_data(b: &mut Bits) {
    for size_id in 0..4 {
        let step = if size_id == 3 { 3 } else { 1 };
        for _ in (0..6).step_by(step) {
            if b.bit() == 0 {
                b.ue(); // scaling_list_pred_matrix_id_delta
                continue;
            }
            let coefs = 64.min(1 << (4 + (size_id << 1)));
            if size_id > 1 {
                b.se(); // scaling_list_dc_coef_minus8
            }
            for _ in 0..coefs {
                b.se(); // scaling_list_delta_coef
            }
            if b.overrun() {
                return;
            }
        }
    }
}

/// One `st_ref_pic_set` (§7.3.7), kept only for the number of pictures it
/// holds: that count is what the *next* set spends a flag per picture on when
/// it predicts from this one, so losing it loses the bit position.
///
/// ponytail: the predicted case counts flags rather than deriving the POCs
/// (§7.4.8, (7-59)..(7-71)), so a set whose `deltaRps` cancels a reference's
/// own delta exactly -- a picture at POC distance zero, which is the current
/// picture and which no encoder emits -- would be counted one too high. The
/// upgrade path is carrying the `DeltaPocS0/S1` arrays through the loop; the
/// price of being wrong is a VUI that reads as garbage and falls to the
/// resolution heuristic, not a bad picture.
fn st_ref_pic_set(b: &mut Bits, idx: usize, counts: &mut Vec<u32>) {
    if idx > 0 && b.bit() == 1 {
        b.bit(); // delta_rps_sign
        b.ue(); // abs_delta_rps_minus1
        let mut kept = 0;
        for _ in 0..=counts[idx - 1] {
            // used_by_curr_pic_flag, and use_delta_flag only when it is not.
            let used = b.bit() == 1;
            kept += u32::from(used || b.bit() == 1);
        }
        counts.push(kept);
        return;
    }
    let negative = b.ue();
    let positive = b.ue();
    if negative > 64 || positive > 64 {
        b.over = true;
        counts.push(0);
        return;
    }
    for _ in 0..negative + positive {
        b.ue(); // delta_poc_s0/s1_minus1
        b.bit(); // used_by_curr_pic_s0/s1_flag
    }
    counts.push(negative + positive);
}

/// The head of a VUI, H.264 (§E.1.1) and HEVC (§E.2.1) alike -- the two are
/// byte-identical up to and including `colour_description`, which is all that is
/// read here.
fn vui_colour(b: &mut Bits) -> Tags {
    if b.bit() == 1 {
        // aspect_ratio_info_present_flag
        if b.u(8) == 255 {
            // Extended_SAR: the ratio is written out rather than indexed.
            b.skip(32);
        }
    }
    if b.bit() == 1 {
        b.bit(); // overscan_appropriate_flag
    }
    if b.bit() == 0 {
        return Tags::default(); // no video_signal_type at all
    }
    b.skip(3); // video_format
    let full_range = b.bit();
    if b.bit() == 0 || b.overrun() {
        // colour_description_present_flag: the range flag still stands on its
        // own, which is what an untagged-but-full-range stream says.
        return Tags {
            full_range: (!b.overrun()).then_some(full_range == 1),
            ..Tags::default()
        };
    }
    let (primaries, transfer, matrix) = (b.u(8), b.u(8), b.u(8));
    let _ = primaries;
    if b.overrun() {
        return Tags::default();
    }
    Tags::from_codes(
        u64::from(matrix),
        u64::from(transfer),
        u64::from(full_range) + 1,
    )
}

/// `color_config` out of the sequence header OBU (AV1 §5.5.1, §5.5.2), which is
/// what `av1C`/`CodecPrivate` carries and what both demuxers re-inject ahead of
/// every keyframe. The whole header before it is counted, and most of it is
/// conditional -- the operating points and the decoder model in particular.
fn av1_sequence_header(obus: &[u8]) -> Tags {
    let Some(payload) = sequence_header_obu(obus) else {
        return Tags::default();
    };
    let mut b = Bits::new(payload);
    let seq_profile = b.u(3);
    b.bit(); // still_picture
    let reduced = b.bit() == 1;
    let mut decoder_model = false;
    if reduced {
        b.skip(5); // seq_level_idx[0]
    } else {
        if b.bit() == 1 {
            // timing_info: two 32-bit ticks and, if the interval is fixed, a
            // uvlc() -- the one variable-length code in this header.
            b.skip(64);
            if b.bit() == 1 {
                uvlc(&mut b);
            }
            decoder_model = b.bit() == 1;
            if decoder_model {
                b.skip(5 + 32 + 5 + 5);
            }
        }
        let initial_display_delay = b.bit() == 1;
        let operating_points = b.u(5) + 1;
        for _ in 0..operating_points {
            b.skip(12); // operating_point_idc[i]
            let level = b.u(5);
            if level > 7 {
                b.bit(); // seq_tier[i]
            }
            if decoder_model && b.bit() == 1 {
                // operating_parameters_info: two delays of the length the
                // decoder model declared, plus low_delay_mode_flag. The lengths
                // were skipped above, so a file with a decoder model is where
                // this walk stops being able to count.
                return Tags::default();
            }
            if initial_display_delay && b.bit() == 1 {
                b.skip(4); // initial_display_delay_minus_1[i]
            }
        }
    }
    let width_bits = b.u(4) + 1;
    let height_bits = b.u(4) + 1;
    b.skip((width_bits + height_bits) as usize); // max_frame_width/height_minus_1
    if !reduced && b.bit() == 1 {
        b.skip(4 + 3); // delta/additional frame id lengths
    }
    b.skip(3); // use_128x128_superblock, enable_filter_intra, intra_edge_filter
    if !reduced {
        b.skip(4); // enable_interintra_compound .. enable_dual_filter
        let order_hint = b.bit() == 1;
        if order_hint {
            b.skip(2); // enable_jnt_comp, enable_ref_frame_mvs
        }
        // seq_choose_screen_content_tools, or the forced value when it is not
        // chosen; the integer-mv pair after it exists only when the tools do.
        let screen_content = if b.bit() == 1 { 2 } else { b.bit() };
        if screen_content > 0 && b.bit() == 0 {
            b.bit(); // seq_force_integer_mv
        }
        if order_hint {
            b.skip(3); // order_hint_bits_minus_1
        }
    }
    b.skip(3); // enable_superres, enable_cdef, enable_restoration
    av1_color_config(&mut b, seq_profile)
}

/// The payload of the first sequence header OBU (type 1) in a low-overhead OBU
/// stream (AV1 §5.3.1): a one-byte header, an optional extension byte, and an
/// optional leb128 size.
fn sequence_header_obu(mut buf: &[u8]) -> Option<&[u8]> {
    while let Some((&head, rest)) = buf.split_first() {
        let kind = (head >> 3) & 0xf;
        let mut rest = rest;
        if head & 0x4 != 0 {
            rest = rest.get(1..)?; // obu_extension_flag: temporal/spatial ids
        }
        let size = if head & 0x2 != 0 {
            let (size, read) = leb128(rest)?;
            rest = rest.get(read..)?;
            size.min(rest.len())
        } else {
            rest.len()
        };
        if kind == 1 {
            return rest.get(..size);
        }
        buf = rest.get(size..)?;
    }
    None
}

/// leb128 as AV1 writes it (§4.10.5): seven bits a byte, low group first, the
/// top bit saying another follows.
fn leb128(buf: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for i in 0..8 {
        let &byte = buf.get(i)?;
        value |= usize::from(byte & 0x7f) << (i * 7);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// `uvlc()` (AV1 §4.10.3): leading zeros, then that many bits, biased -- the
/// same shape as exp-Golomb with a different bias.
fn uvlc(b: &mut Bits) {
    let mut zeros = 0;
    while b.bit() == 0 && !b.overrun() {
        zeros += 1;
        if zeros >= 32 {
            b.over = true;
            return;
        }
    }
    b.skip(zeros);
}

/// `color_config` (AV1 §5.5.2). The bit depth and the subsampling are read only
/// as far as they cost bits before `color_range`.
fn av1_color_config(b: &mut Bits, seq_profile: u32) -> Tags {
    let high_bitdepth = b.bit() == 1;
    if seq_profile == 2 && high_bitdepth {
        b.bit(); // twelve_bit
    }
    let mono_chrome = seq_profile != 1 && b.bit() == 1;
    let described = b.bit() == 1;
    let (primaries, transfer, matrix) = if described {
        (b.u(8), b.u(8), b.u(8))
    } else {
        (2, 2, 2) // CP/TC/MC_UNSPECIFIED
    };
    if b.overrun() {
        return Tags::default();
    }
    let full_range = if mono_chrome {
        b.bit() == 1
    } else if primaries == 1 && transfer == 13 && matrix == 0 {
        // sRGB in an identity matrix is full range by definition, and the flag
        // is not written.
        true
    } else {
        b.bit() == 1
    };
    if b.overrun() {
        return Tags::default();
    }
    Tags {
        full_range: Some(full_range),
        ..Tags::from_codes(u64::from(matrix), u64::from(transfer), 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demux::Demuxer;
    use std::path::{Path, PathBuf};

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    fn color_of(name: &str) -> ColorDescription {
        let (meta, _) = Demuxer::open(&asset(name)).unwrap_or_else(|e| panic!("open {name}: {e}"));
        meta.color
    }

    /// The tier order itself: a container tag beats both a bitstream tag and the
    /// heuristic, field by field.
    #[test]
    fn tiers_resolve_in_order() {
        let container = Tags::from_codes(9, 16, 1);
        let bitstream = Tags::from_codes(1, 1, 2);
        assert_eq!(
            ColorDescription::resolve(container, bitstream, 2160),
            ColorDescription {
                matrix: Matrix::Bt2020Ncl,
                transfer: Transfer::Pq,
                full_range: false,
            }
        );
        // A container that tagged only the range leaves the rest to the SPS.
        let range_only = Tags::from_codes(0, 0, 2);
        assert_eq!(
            ColorDescription::resolve(range_only, bitstream, 480),
            ColorDescription {
                matrix: Matrix::Bt709,
                transfer: Transfer::Sdr,
                full_range: true,
            }
        );
        // Nothing anywhere: the resolution decides, and only the resolution.
        let none = Tags::default();
        assert_eq!(
            ColorDescription::resolve(none, none, 1080).matrix,
            Matrix::Bt709
        );
        assert_eq!(
            ColorDescription::resolve(none, none, 719).matrix,
            Matrix::Bt601
        );
        assert_eq!(
            ColorDescription::default(),
            ColorDescription::resolve(none, none, 480)
        );
    }

    /// Reserved and unimplemented code points are "said nothing", not a wrong
    /// answer: that is what makes them fall through.
    #[test]
    fn unknown_codes_fall_through() {
        assert_eq!(Tags::from_codes(2, 2, 3), Tags::default());
        assert_eq!(Tags::from_codes(7, 0, 0).matrix, None); // SMPTE 240M
        assert_eq!(Tags::from_codes(6, 1, 1).transfer, Some(Transfer::Sdr));
        assert_eq!(Tags::from_codes(0, 18, 2).transfer, Some(Transfer::Hlg));
    }

    /// The bit reader, against a hand-built pattern: exp-Golomb is where a
    /// mis-read shifts everything after it.
    #[test]
    fn bit_reader_reads_exp_golomb() {
        // 1 | 010 | 011 | 00100 : ue = 0, 1, 2, 3
        let mut b = Bits::new(&[0b1010_0110, 0b0100_0000]);
        assert_eq!((b.ue(), b.ue(), b.ue(), b.ue()), (0, 1, 2, 3));
        assert!(!b.overrun());
        // se zig-zags: 0, 1, -1, 2
        let mut b = Bits::new(&[0b1010_0110, 0b0100_0000]);
        assert_eq!((b.se(), b.se(), b.se(), b.se()), (0, 1, -1, 2));
        // Past the end reads zero and says so.
        let mut b = Bits::new(&[0xff]);
        b.skip(8);
        assert_eq!(b.u(4), 0);
        assert!(b.overrun());
    }

    /// Emulation prevention: the `03` is syntax-invisible, a third `00` is not.
    #[test]
    fn rbsp_drops_emulation_bytes() {
        assert_eq!(rbsp(&[0, 0, 3, 1, 0, 0, 3, 0]), vec![0, 0, 1, 0, 0, 0]);
        assert_eq!(rbsp(&[0, 3, 3]), vec![0, 3, 3]);
    }

    /// A generated fixture whose *container* says BT.601 at 720 lines, where the
    /// heuristic would have said BT.709: proof the container tier is read and
    /// that it wins. Both containers, because both parse it themselves.
    #[test]
    fn container_tags_win_over_the_heuristic() {
        for name in ["test_bt601.mp4", "test_bt601.mkv"] {
            assert_eq!(
                color_of(name),
                ColorDescription {
                    matrix: Matrix::Bt601,
                    transfer: Transfer::Sdr,
                    full_range: false,
                },
                "{name}"
            );
        }
    }

    /// Bits written out as `'0'`/`'1'`, MSB first, for a hand-built SPS.
    fn bits_of(pattern: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, c) in pattern.chars().filter(|c| !c.is_whitespace()).enumerate() {
            if i % 8 == 0 {
                out.push(0);
            }
            if c == '1' {
                *out.last_mut().unwrap() |= 0x80 >> (i % 8);
            }
        }
        out
    }

    /// A High-profile SPS written by hand, because no encoder here writes one
    /// with its scaling lists in the *sequence* parameter set (x264 puts its
    /// quant matrices in the PPS; the HEVC fixture below has real ones). The
    /// lists are what makes this test worth having: read as if they were not
    /// there, the VUI lands mid-coefficient and the colour comes out as noise.
    #[test]
    fn h264_scaling_lists_are_skipped_before_the_vui() {
        let sps = concat!(
            "01100100",  // profile_idc 100 (High)
            "00000000",  // constraint flags + reserved
            "00011110",  // level_idc 30
            "1",         // seq_parameter_set_id ue(0)
            "010",       // chroma_format_idc ue(1), 4:2:0
            "1",         // bit_depth_luma_minus8 ue(0)
            "1",         // bit_depth_chroma_minus8 ue(0)
            "0",         // qpprime_y_zero_transform_bypass_flag
            "1",         // seq_scaling_matrix_present_flag
            "1",         // ...list 0 present
            "000010001", // its first delta, se(-8): nextScale hits 0, so the
            // remaining 15 coefficients are the default ones and cost no bits
            "0000000",     // lists 1..7 absent
            "1",           // log2_max_frame_num_minus4 ue(0)
            "1",           // pic_order_cnt_type ue(0)
            "1",           // log2_max_pic_order_cnt_lsb_minus4 ue(0)
            "010",         // max_num_ref_frames ue(1)
            "0",           // gaps_in_frame_num_value_allowed_flag
            "00000101000", // pic_width_in_mbs_minus1 ue(39), i.e. 640
            "000011110",   // pic_height_in_map_units_minus1 ue(29), i.e. 480
            "1",           // frame_mbs_only_flag
            "1",           // direct_8x8_inference_flag
            "0",           // frame_cropping_flag
            "1",           // vui_parameters_present_flag
            "0",           // aspect_ratio_info_present_flag
            "0",           // overscan_info_present_flag
            "1",           // video_signal_type_present_flag
            "101",         // video_format (unspecified)
            "0",           // video_full_range_flag
            "1",           // colour_description_present_flag
            "00000001",    // colour_primaries 1
            "00000001",    // transfer_characteristics 1
            "00000001",    // matrix_coefficients 1 (BT.709)
        );
        assert_eq!(
            h264_sps(&bits_of(sps)),
            Tags {
                matrix: Some(Matrix::Bt709),
                transfer: Some(Transfer::Sdr),
                full_range: Some(false),
            }
        );
    }

    /// The HEVC twin of the test above, and hand-built for the same reason:
    /// x265 writes `sps_scaling_list_data_present_flag` only for a *custom*
    /// list, and the fixture below carries the default one (a flag and no
    /// data). Custom lists are not exotic -- they are what a psy-tuned anime
    /// encode ships -- so the branch that counts them has to be measured.
    #[test]
    fn hevc_scaling_list_data_is_skipped_before_the_vui() {
        let mut sps = String::new();
        sps.push_str("0000"); // sps_video_parameter_set_id
        sps.push_str("000"); // sps_max_sub_layers_minus1: one layer
        sps.push('0'); // sps_temporal_id_nesting_flag
        sps.push_str(&"0".repeat(96)); // profile_tier_level, general only
        sps.push('1'); // sps_seq_parameter_set_id ue(0)
        sps.push_str("010"); // chroma_format_idc ue(1), 4:2:0
        sps.push_str("0000000001010000001"); // pic_width_in_luma_samples ue(640)
        sps.push_str("00000000111100001"); // pic_height_in_luma_samples ue(480)
        sps.push('0'); // conformance_window_flag
        sps.push_str("11"); // bit_depth_luma/chroma_minus8 ue(0)
        sps.push_str("00101"); // log2_max_pic_order_cnt_lsb_minus4 ue(4)
        sps.push('1'); // sps_sub_layer_ordering_info_present_flag
        sps.push_str("111"); // ...its three ue(0)s, for the one layer
        sps.push_str("111111"); // the six coding/transform-block ue(0)s
        sps.push_str("11"); // scaling_list_enabled + sps_scaling_list_data_present
        // scaling_list_data: the first 4x4 matrix written out (16 se(0) deltas,
        // no DC coefficient at this size), every other matrix predicted from an
        // earlier one -- 6+6+6+2 matrices in all.
        sps.push('1');
        sps.push_str(&"1".repeat(16));
        sps.push_str(&"01".repeat(5 + 6 + 6 + 2));
        sps.push_str("000"); // amp, sample_adaptive_offset, pcm_enabled
        sps.push('1'); // num_short_term_ref_pic_sets ue(0)
        sps.push('0'); // long_term_ref_pics_present_flag
        sps.push_str("00"); // sps_temporal_mvp, strong_intra_smoothing
        sps.push('1'); // vui_parameters_present_flag
        sps.push_str("00"); // aspect_ratio_info_present, overscan_info_present
        sps.push('1'); // video_signal_type_present_flag
        sps.push_str("101"); // video_format (unspecified)
        sps.push('0'); // video_full_range_flag
        sps.push('1'); // colour_description_present_flag
        sps.push_str("000000010000000100000001"); // primaries/transfer/matrix 1
        assert_eq!(
            hevc_sps(&bits_of(&sps)),
            Tags {
                matrix: Some(Matrix::Bt709),
                transfer: Some(Transfer::Sdr),
                full_range: Some(false),
            }
        );
    }

    /// The bitstream tier, in the three codecs that carry one: fixtures tagged
    /// through the *encoder* alone (x264/x265 VUI, SVT-AV1 `color_config`) and
    /// muxed with no `colr` box at all, at 480 lines where the heuristic would
    /// have said BT.601. The absence of the box is asserted too: without it a
    /// fixture that grew container tags would pass this for the wrong reason.
    /// The H.264 one is High profile with `cqm=jvt`, i.e. a real SPS with
    /// scaling lists in front of its VUI.
    #[test]
    fn bitstream_tags_beat_the_heuristic() {
        for name in ["test_vui_h264.mp4", "test_vui_hevc.mp4", "test_vui_av1.mp4"] {
            let bytes = std::fs::read(asset(name)).unwrap_or_else(|e| panic!("read {name}: {e}"));
            assert!(
                !bytes.windows(4).any(|w| w == b"colr"),
                "{name} carries a colr box, so this would not be testing the bitstream"
            );
            assert_eq!(color_of(name).matrix, Matrix::Bt709, "{name}");
        }
    }

    /// An untagged 360-line fixture: nothing in the container, nothing in the
    /// stream, so the heuristic answers -- BT.601 limited, which is what the
    /// engine assumed of everything before any of this was read.
    #[test]
    fn untagged_sd_falls_to_the_heuristic() {
        assert_eq!(color_of("test_mismatch.mp4"), ColorDescription::default());
    }

    /// A real HDR film: BT.2020 non-constant-luminance, PQ, limited range, all
    /// three off the Matroska `Colour` element. Gated on the file being here --
    /// it is a 25 GB download, not a fixture.
    #[test]
    fn real_hdr_film_reads_bt2020_pq() {
        let path = Path::new(
            "/path/to/a-real-4k-hdr10-film.mkv",
        );
        if !path.exists() {
            eprintln!("skipped: {} is not on this machine", path.display());
            return;
        }
        let (meta, _) = Demuxer::open(path).expect("open the HDR film");
        assert_eq!(
            meta.color,
            ColorDescription {
                matrix: Matrix::Bt2020Ncl,
                transfer: Transfer::Pq,
                full_range: false,
            }
        );
    }

    /// A real 720p film with no colour tags anywhere -- neither container nor
    /// SPS -- which is the case the heuristic exists for: BT.709, because it is
    /// HD material. Gated for the same reason as the one above.
    #[test]
    fn real_untagged_hd_film_reads_bt709() {
        let path = Path::new(
            "/path/to/a-real-h264-dual-audio-film.mkv",
        );
        if !path.exists() {
            eprintln!("skipped: {} is not on this machine", path.display());
            return;
        }
        let (meta, _) = Demuxer::open(path).expect("open the untagged film");
        assert_eq!(
            meta.color,
            ColorDescription {
                matrix: Matrix::Bt709,
                transfer: Transfer::Sdr,
                full_range: false,
            }
        );
    }
}
