//! The `profile` a video stream states, spelled as ffprobe spells its
//! `stream=profile` field ([`crate::VideoFacts::profile`]). Each answer is read
//! from the stream's own configuration: the H.264 SPS, the HEVC `hvcC`
//! `general_profile_idc`, the AV1 sequence header's `seq_profile`, or the VP9
//! frame header's profile bits, or the VP8 frame tag's version bits. Anything
//! else is `None`.

use ec_h264_syntax::{AnnexBIter, Sps, unescape_rbsp};

/// The H.264 profile the first SPS in `sets` states, where `sets` is Annex-B
/// framed the way both demuxers keep their parameter sets.
pub(crate) fn h264(sets: &[u8]) -> Option<&'static str> {
    let nal = AnnexBIter::new(sets).find(|n| n.first().is_some_and(|b| b & 0x1f == 7))?;
    let mut rbsp = Vec::new();
    unescape_rbsp(&nal[1..], &mut rbsp);
    let sps = Sps::parse(&rbsp).ok()?;
    h264_name(sps.profile_idc, sps.constraint_flags)
}

/// Whether the first SPS in `sets` has `frame_mbs_only_flag` 0.
pub(crate) fn h264_interlaced(sets: &[u8]) -> bool {
    let Some(nal) = AnnexBIter::new(sets).find(|n| n.first().is_some_and(|b| b & 0x1f == 7)) else {
        return false;
    };
    let mut rbsp = Vec::new();
    unescape_rbsp(&nal[1..], &mut rbsp);
    Sps::parse(&rbsp).is_ok_and(|sps| !sps.frame_mbs_only)
}

/// The name of an H.264 `profile_idc` under its `constraint_setX_flag` bits
/// (set0 is bit 7). Constrained Baseline is set1 on a Baseline stream, and the
/// intra variants are set3 on the High 10, 4:2:2 and 4:4:4 Predictive profiles.
fn h264_name(idc: u8, flags: u8) -> Option<&'static str> {
    let (set1, set3) = (flags & 0x40 != 0, flags & 0x10 != 0);
    Some(match idc {
        66 if set1 => "Constrained Baseline",
        66 => "Baseline",
        77 => "Main",
        88 => "Extended",
        100 => "High",
        110 if set3 => "High 10 Intra",
        110 => "High 10",
        122 if set3 => "High 4:2:2 Intra",
        122 => "High 4:2:2",
        244 if set3 => "High 4:4:4 Intra",
        244 => "High 4:4:4 Predictive",
        44 => "CAVLC 4:4:4",
        _ => return None,
    })
}

/// The HEVC profile of an `hvcC` record: its `general_profile_idc`, the low five
/// bits of the byte after the configuration version.
pub(crate) fn hevc_record(hvcc: &[u8]) -> Option<&'static str> {
    Some(match hvcc.get(1)? & 0x1f {
        1 => "Main",
        2 => "Main 10",
        3 => "Main Still Picture",
        4 => "Rext",
        _ => return None,
    })
}

/// The AV1 profile the sequence header OBU in `obus` states (`seq_profile`, the
/// top three bits of its first payload byte). `obus` is the `av1C` configuration
/// OBUs, or the sequence header as a bitstream carries it.
pub(crate) fn av1(obus: &[u8]) -> Option<&'static str> {
    let mut at = 0;
    while let Some(&header) = obus.get(at) {
        let kind = (header >> 3) & 0xf;
        at += 1 + usize::from((header >> 2) & 1);
        let len = if (header >> 1) & 1 == 1 {
            let (len, used) = leb128(obus.get(at..)?)?;
            at += used;
            len
        } else {
            obus.len().saturating_sub(at)
        };
        let payload = obus.get(at..at.checked_add(len)?)?;
        if kind == 1 {
            return Some(match payload.first()? >> 5 {
                0 => "Main",
                1 => "High",
                2 => "Professional",
                _ => return None,
            });
        }
        at += len;
    }
    None
}

/// The VP9 profile of a frame: `profile_low_bit` and `profile_high_bit`, the two
/// bits after the two-bit frame marker, read most significant first.
pub(crate) fn vp9(frame: &[u8]) -> Option<&'static str> {
    let first = *frame.first()?;
    Some(match ((first >> 4) & 1) << 1 | (first >> 5) & 1 {
        0 => "Profile 0",
        1 => "Profile 1",
        2 => "Profile 2",
        _ => "Profile 3",
    })
}

/// The VP8 profile of a frame: the `version` field of its frame tag, the first
/// three bytes read as a little-endian 24-bit value, whose bits 1 to 3 hold it.
/// Versions above 3 name no profile ffprobe knows, so they are `None`.
pub(crate) fn vp8(frame: &[u8]) -> Option<&'static str> {
    if frame.len() < 3 {
        return None;
    }
    match (frame[0] >> 1) & 7 {
        0 => Some("0"),
        1 => Some("1"),
        2 => Some("2"),
        3 => Some("3"),
        _ => None,
    }
}

/// The ProRes flavour a sample entry's fourcc names, in Apple's own words (the name the
/// ProRes white paper and every NLE use, not ffprobe's `Standard` / `4444 XQ`): `422 Proxy`
/// (`apco`), `422 LT` (`apcs`), `422` (`apcn`), `422 HQ` (`apch`), `4444` (`ap4h`) and
/// `4444 XQ` (`ap4x`). The frame header states none of it, so a track whose fourcc is something
/// else (or that has none, as in a Matroska or AVI wrapper) states no profile.
pub(crate) fn prores(fourcc: &[u8; 4]) -> Option<&'static str> {
    Some(match fourcc {
        b"apco" => "422 Proxy",
        b"apcs" => "422 LT",
        b"apcn" => "422",
        b"apch" => "422 HQ",
        b"ap4h" => "4444",
        b"ap4x" => "4444 XQ",
        _ => return None,
    })
}

/// The Avid profile of a DNxHD / DNxHR frame. The DNxHR compression ids (1270 to 1274) are
/// the profiles themselves: `DNxHR 444`, `DNxHR HQX`, `DNxHR HQ`, `DNxHR SQ`, `DNxHR LB`. A
/// DNxHD one is named by its nominal data rate, which is constant: a frame's bytes over its
/// duration (`bytes` of the first sample at `fps`), snapped to the nearest of Avid's rates
/// within 6 %, with an `x` for the 10-bit ones (`DNxHD 145`, `DNxHD 220x`). `None` for a rate
/// that is none of Avid's, or an unknown frame rate.
pub(crate) fn dnxhd(cid: u32, bit_depth: u32, bytes: usize, fps: f64) -> Option<&'static str> {
    match cid {
        1270 => return Some("DNxHR 444"),
        1271 => return Some("DNxHR HQX"),
        1272 => return Some("DNxHR HQ"),
        1273 => return Some("DNxHR SQ"),
        1274 => return Some("DNxHR LB"),
        _ => {}
    }
    if fps <= 0.0 {
        return None;
    }
    let mbps = bytes as f64 * 8.0 * fps / 1e6;
    const RATES: [u32; 12] = [36, 45, 60, 75, 90, 100, 110, 115, 120, 145, 175, 185];
    let nearest = RATES
        .iter()
        .chain([&220u32])
        .copied()
        .min_by(|&a, &b| (f64::from(a) - mbps).abs().total_cmp(&(f64::from(b) - mbps).abs()))?;
    if (f64::from(nearest) - mbps).abs() > f64::from(nearest) * 0.06 {
        return None;
    }
    let ten = bit_depth >= 10;
    Some(match (nearest, ten) {
        (36, _) => "DNxHD 36",
        (45, _) => "DNxHD 45",
        (60, _) => "DNxHD 60",
        (75, _) => "DNxHD 75",
        (90, false) => "DNxHD 90",
        (90, true) => "DNxHD 90x",
        (100, _) => "DNxHD 100",
        (110, _) => "DNxHD 110x",
        (115, _) => "DNxHD 115",
        (120, _) => "DNxHD 120",
        (145, _) => "DNxHD 145",
        (175, false) => "DNxHD 175",
        (175, true) => "DNxHD 175x",
        (185, false) => "DNxHD 185",
        (185, true) => "DNxHD 185x",
        (220, false) => "DNxHD 220",
        (220, true) => "DNxHD 220x",
        _ => return None,
    })
}

/// The MPEG-4 Part 2 profile of a track: `H.263` for a 3GPP / QuickTime H.263 sample entry, else
/// the `profile_and_level_indication` of the Visual Object Sequence header (`00 00 01 B0`) in
/// `vol` -- `Simple` (levels 0 to 3), `Advanced Simple` (`0xF0` to `0xF5`) or the other names
/// ISO/IEC 14496-2 Annex G gives. `None` for a configuration with no sequence header, which
/// states none.
pub(crate) fn mpeg4(entry: Option<&[u8; 4]>, vol: &[u8]) -> Option<&'static str> {
    if matches!(entry, Some(b"s263" | b"h263")) {
        return Some("H.263");
    }
    let at = vol.windows(4).position(|w| w == [0, 0, 1, 0xB0])?;
    Some(match *vol.get(at + 4)? {
        0x01..=0x03 | 0x08 | 0x09 => "Simple",
        0x10..=0x12 => "Simple Scalable",
        0x21 | 0x22 => "Core",
        0x32..=0x34 => "Main",
        0x42 => "N-bit",
        0x51..=0x55 => "Scalable Texture",
        0x61 | 0x62 => "Simple Face",
        0x63 | 0x64 => "Simple FBA",
        0x71..=0x74 => "Basic Animated Texture",
        0x81..=0x85 => "Hybrid",
        0x91..=0x95 => "Advanced Real Time Simple",
        0xA1..=0xA3 => "Core Scalable",
        0xB1..=0xB3 => "Advanced Coding Efficiency",
        0xC1..=0xC3 => "Advanced Core",
        0xD1..=0xD3 => "Advanced Scalable Texture",
        0xF0..=0xF5 => "Advanced Simple",
        0xF7..=0xFF => "Fine Granularity Scalability",
        _ => return None,
    })
}

/// The DV flavour of a frame's system, in the broadcast names: `DV25` (consumer DV, DVCAM and
/// DVCPRO25 -- one bit rate, one stream type), `DVCPRO50` and `DVCPRO HD`.
pub(crate) fn dv(profile: &ec_dv::Profile) -> &'static str {
    if profile.is_hd() {
        "DVCPRO HD"
    } else if profile.video_stype == 4 {
        "DVCPRO50"
    } else {
        "DV25"
    }
}

/// The layout of uncompressed video, from the `BITMAPINFOHEADER` an AVI keeps or the one the mov
/// demuxer synthesizes (marked `mov ` where `biSize` sits): the FourCC's usual name (`YUY2`,
/// `UYVY`, `YVYU`, `I420`, `YV12`, `NV12`, `Y800`, `v210`), else by depth for a DIB (`BGR24`,
/// `BGRA`, `RGB555`, `PAL8`). A mov entry is named by its own FourCC and depth: `raw ` is `RGB24`,
/// `ARGB` or `RGB555`, `2vuy` is `UYVY`, `yuvs` `YUY2`, `yuv2` `yuv2`, `v210`, `v410`, `b16g` `Gray16`.
pub(crate) fn raw(bih: &[u8]) -> &'static str {
    let bits = bih.get(14..16).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    if bih.get(..4) == Some(&b"mov "[..]) {
        return match (bih.get(16..20), bits) {
            (Some(b"raw "), 32) => "ARGB",
            (Some(b"raw "), 16) => "RGB555",
            (Some(b"raw "), _) => "RGB24",
            (Some(b"2vuy"), _) => "UYVY",
            (Some(b"yuvs"), _) => "YUY2",
            (Some(b"yuv2"), _) => "yuv2",
            (Some(b"v210"), _) => "v210",
            (Some(b"v410"), _) => "v410",
            (Some(b"b16g"), _) => "Gray16",
            _ => "raw",
        };
    }
    let tag = bih.get(16..20).map(|t| t.to_ascii_uppercase());
    match tag.as_deref() {
        Some(b"YUY2" | b"YUYV" | b"YUV2" | b"V422") => "YUY2",
        Some(b"UYVY" | b"2VUY") => "UYVY",
        Some(b"YVYU") => "YVYU",
        Some(b"I420" | b"IYUV") => "I420",
        Some(b"YV12") => "YV12",
        Some(b"NV12") => "NV12",
        Some(b"Y800" | b"Y8  " | b"GREY") => "Y800",
        Some(b"V210") => "v210",
        _ if bits == 8 => "PAL8",
        _ if bits == 15 || bits == 16 => "RGB555",
        _ if bits == 32 => "BGRA",
        _ => "BGR24",
    }
}

/// An unsigned LEB128 value and how many bytes it took.
fn leb128(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for (i, &b) in bytes.iter().take(8).enumerate() {
        value |= usize::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_names_follow_the_constraint_flags() {
        assert_eq!(h264_name(66, 0), Some("Baseline"));
        assert_eq!(h264_name(66, 0x40), Some("Constrained Baseline"));
        assert_eq!(h264_name(100, 0x10), Some("High"));
        assert_eq!(h264_name(110, 0), Some("High 10"));
        assert_eq!(h264_name(110, 0x10), Some("High 10 Intra"));
        assert_eq!(h264_name(122, 0), Some("High 4:2:2"));
        assert_eq!(h264_name(244, 0), Some("High 4:4:4 Predictive"));
        assert_eq!(h264_name(200, 0), None);
    }

    #[test]
    fn av1_reads_seq_profile_from_a_sized_obu() {
        // A sequence header OBU (type 1, size field) of two bytes, seq_profile 2.
        assert_eq!(av1(&[0x0a, 0x02, 0x40, 0x00]), Some("Professional"));
        // An av1C-style record: a metadata OBU first, then the sequence header.
        assert_eq!(av1(&[0x22, 0x01, 0x00, 0x0a, 0x01, 0x20]), Some("High"));
    }

    #[test]
    fn vp9_reads_the_profile_bits() {
        assert_eq!(vp9(&[0x82]), Some("Profile 0"));
        assert_eq!(vp9(&[0xa2]), Some("Profile 1"));
        assert_eq!(vp9(&[0x92]), Some("Profile 2"));
        assert_eq!(vp9(&[0xb2]), Some("Profile 3"));
    }

    #[test]
    fn vp8_reads_the_version_bits_of_the_frame_tag() {
        // Bit 0 is !key_frame, bits 1..=3 the version.
        assert_eq!(vp8(&[0x00, 0x00, 0x00]), Some("0"));
        assert_eq!(vp8(&[0x01, 0x00, 0x00]), Some("0"));
        assert_eq!(vp8(&[0x02, 0x00, 0x00]), Some("1"));
        assert_eq!(vp8(&[0x04, 0x00, 0x00]), Some("2"));
        assert_eq!(vp8(&[0x07, 0x00, 0x00]), Some("3"));
        assert_eq!(vp8(&[0x08, 0x00, 0x00]), None);
        assert_eq!(vp8(&[0x00, 0x00]), None);
    }
}
