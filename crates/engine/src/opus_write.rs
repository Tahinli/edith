//! Writes interleaved PCM as a standalone `.opus` file: Opus in Ogg, RFC 7845.
//! `ec-opus` encodes, `ec-ogg` pages.
//!
//! What the file states, and why each number is what it is:
//!
//! - **`OpusHead`**: the channel count; the pre-skip (the encoder's look-ahead
//!   plus one warm-up frame, see [`write`]); the input rate; mapping family 0
//!   for mono and stereo, family 1 (Vorbis channel order, with the stream and
//!   coupled counts and the table) for 3 to 8 channels.
//! - **`OpusTags`**: the vendor, an `ENCODER` tag, the language where the mix
//!   has one, and the chapters as the `CHAPTERxxx` / `CHAPTERxxxNAME` comments
//!   ffmpeg writes and reads.
//! - **Granule positions** at 48 kHz with the pre-skip counted, the last one
//!   trimming the final frame's padding, so the duration is exactly the
//!   samples handed in.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use ec_core::registry::{AudioParameters, CodecId, CodecParameters, MediaParameters, StreamInfo};
use ec_core::{ChannelLayout, Muxer as _, Packet, TimeBase};

/// The only clock an Opus stream has (RFC 7845 §4).
pub const RATE: u32 = 48_000;

/// 20 ms at [`RATE`]: the frame every packet codes.
const FRAME: usize = 960;

/// The rates RFC 6716 gives an Opus stream, in kbps, that a file is clamped into.
pub const MIN_KBPS: u32 = 6;
pub const MAX_KBPS: u32 = 510;

/// What a file with no rate asked for is coded at: transparent for stereo
/// music, and half the AAC default, because Opus is that much better at it.
pub const DEFAULT_KBPS: u32 = 128;

/// The vendor string in `OpusTags`.
const VENDOR: &str = "ec-opus";

/// What travels in `OpusTags` beside the vendor.
#[derive(Debug, Clone, Default)]
pub struct Tags<'a> {
    /// A language code for the `LANGUAGE` comment.
    pub language: Option<&'a str>,
    /// `(start in ns, title)`, sorted by start: `CHAPTERxxx` comments.
    pub chapters: &'a [(u64, String)],
}

/// Where WAV/film channel `c` sits in Vorbis order, per channel count: the
/// order family 1 codes in (RFC 7845 §5.1.1.2). `vorbis[i] = wav[ORDER[i]]`.
fn wav_to_vorbis(channels: usize) -> &'static [usize] {
    match channels {
        1 => &[0],
        2 => &[0, 1],
        3 => &[0, 2, 1],
        4 => &[0, 1, 2, 3],
        5 => &[0, 2, 1, 3, 4],
        6 => &[0, 2, 1, 4, 5, 3],
        7 => &[0, 2, 1, 5, 6, 4, 3],
        _ => &[0, 2, 1, 6, 7, 4, 5, 3],
    }
}

/// `OpusTags` with the vendor, `ENCODER`, `LANGUAGE` and the chapters.
fn opus_tags(tags: &Tags) -> Vec<u8> {
    let mut comments = vec![format!("ENCODER=edith ({VENDOR})")];
    if let Some(lang) = tags.language.filter(|l| !l.is_empty()) {
        comments.push(format!("LANGUAGE={lang}"));
    }
    for (i, (ns, title)) in tags.chapters.iter().enumerate() {
        let ms = ns / 1_000_000;
        let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
        comments.push(format!("CHAPTER{i:03}={h:02}:{m:02}:{s:02}.{ms:03}"));
        comments.push(format!("CHAPTER{i:03}NAME={title}"));
    }
    let mut out = Vec::from(*b"OpusTags");
    out.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    out.extend_from_slice(VENDOR.as_bytes());
    out.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in &comments {
        out.extend_from_slice(&(c.len() as u32).to_le_bytes());
        out.extend_from_slice(c.as_bytes());
    }
    out
}

/// Encode `pcm` (interleaved, `channels` wide in WAV/film order, at [`RATE`])
/// at `kbps` total and write the file to `out`. `input_rate` is what the
/// header reports as the rate the sound arrived at. `tick` runs once per
/// packet with the fraction done, and an `Err` from it stops the write (the
/// export's cancel).
///
/// Every stream is pinned to CELT: its look-ahead is one fixed number, so the
/// pre-skip holds for the whole file and every stream of a surround one lines
/// up with the others. The first frame is coded twice -- once to warm the
/// encoder, which has no history behind it and rings, and once for real --
/// and the warm-up is inside the pre-skip, exactly what pre-skip is for.
pub fn write(
    out: &Path,
    pcm: &[f32],
    channels: usize,
    input_rate: u32,
    kbps: u32,
    tags: &Tags,
    tick: &mut dyn FnMut(f32) -> crate::Result<()>,
) -> crate::Result<()> {
    let (family, n_streams, coupled, table) = ec_opus::ogg::default_mapping(channels)
        .ok_or_else(|| format!("Opus maps 1 to 8 channels, and this mix has {channels}"))?;
    let (n_streams, coupled) = (usize::from(n_streams), usize::from(coupled));
    let frames = pcm.len() / channels;
    if frames == 0 {
        return Err("opus encode: no samples is not a stream".into());
    }
    let order = wav_to_vorbis(channels);

    let total = kbps.clamp(MIN_KBPS, MAX_KBPS) * 1000;
    let share = total / (2 * coupled + (n_streams - coupled)) as u32;
    let mut encoders = Vec::with_capacity(n_streams);
    for s in 0..n_streams {
        let width = if s < coupled { 2 } else { 1 };
        let mut enc = ec_opus::Encoder::new(RATE, width, ec_opus::Application::Audio)
            .map_err(|e| format!("the Opus encoder refused {width} channels: {e}"))?;
        enc.set_mode(Some(ec_opus::Mode::Celt));
        enc.set_bitrate(share * width as u32);
        encoders.push(enc);
    }
    let pre_skip = encoders[0].look_ahead(FRAME) + FRAME;
    let pre_skip16 = u16::try_from(pre_skip).map_err(|_| "opus encode: pre-skip over 65535")?;
    // Exactly as many packets as the pre-skip plus the samples need: the last
    // one carries the tail and its granule trims the padding after it.
    let count = (pre_skip + frames).div_ceil(FRAME);

    let head = ec_opus::ogg::opus_head(
        channels as u8,
        pre_skip16,
        input_rate,
        0,
        (family != 0).then_some((family, n_streams as u8, coupled as u8, table.as_slice())),
    );
    let time_base = TimeBase::try_new(1, i64::from(RATE)).map_err(|e| format!("ogg mux: {e}"))?;
    let mut params = CodecParameters::new(CodecId::Opus);
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: RATE,
        layout: match channels {
            1 => ChannelLayout::Mono,
            _ => ChannelLayout::Stereo,
        },
        ..AudioParameters::default()
    });
    let mut stream = StreamInfo::new(0, time_base, params);
    stream.duration = Some(frames as i64);
    stream.start_time = Some(0);
    let mut muxer = ec_ogg::OggMuxer::new(BufWriter::new(File::create(out)?));
    muxer.set_page_target_bytes(Some(4096));
    muxer.add_stream(stream).map_err(|e| format!("ogg mux: {e}"))?;
    for header in [head, opus_tags(tags)] {
        let mut packet = Packet::new(0, time_base, header);
        packet.flags.header = true;
        muxer.write_packet(&packet).map_err(|e| format!("ogg mux: {e}"))?;
    }

    let mut frame = vec![0f32; FRAME * channels];
    let mut stream_buf = vec![0f32; FRAME * 2];
    let mut bytes = vec![0u8; 1500 * n_streams];
    for p in 0..count {
        tick(p as f32 / count as f32)?;
        // Packet 0 is the warm-up: the first frame, coded again as packet 1.
        let from = p.saturating_sub(1) * FRAME;
        let block = &pcm[(from * channels).min(pcm.len())..((from + FRAME) * channels).min(pcm.len())];
        frame[..block.len()].copy_from_slice(block);
        frame[block.len()..].fill(0.0);
        let mut len = 0;
        for (s, enc) in encoders.iter_mut().enumerate() {
            let width = if s < coupled { 2 } else { 1 };
            for off in 0..width {
                let coded = if s < coupled { 2 * s + off } else { coupled + s };
                // The Vorbis-order channel that feeds this coded one.
                let vorbis = table.iter().position(|&m| usize::from(m) == coded);
                for i in 0..FRAME {
                    stream_buf[i * width + off] =
                        vorbis.map_or(0.0, |v| frame[i * channels + order[v]]);
                }
            }
            let input = &stream_buf[..FRAME * width];
            let n = if s + 1 == n_streams {
                enc.encode_float(input, FRAME, &mut bytes[len..])
            } else {
                enc.encode_self_delimited(input, FRAME, &mut bytes[len..])
            }
            .map_err(|e| format!("Opus encode failed: {e}"))?;
            len += n;
        }
        let granule = match p + 1 == count {
            true => pre_skip + frames,
            false => (p + 1) * FRAME,
        };
        let mut packet =
            Packet::new(0, time_base, bytes[..len].to_vec()).with_duration(FRAME as i64);
        packet.side_data.push(ec_ogg::granule_side_data(granule as i64));
        muxer.write_packet(&packet).map_err(|e| format!("ogg mux: {e}"))?;
    }
    muxer.finish().map_err(|e| format!("ogg mux: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tags_carry_the_chapters_the_way_ffmpeg_writes_them() {
        let chapters = vec![(0, "Intro".to_string()), (61_500_000_000, "Two".to_string())];
        let tags = opus_tags(&Tags { language: Some("eng"), chapters: &chapters });
        let text = String::from_utf8_lossy(&tags);
        for want in ["ENCODER=edith", "LANGUAGE=eng", "CHAPTER000=00:00:00.000", "CHAPTER000NAME=Intro",
            "CHAPTER001=00:01:01.500", "CHAPTER001NAME=Two"] {
            assert!(text.contains(want), "{want} in {text}");
        }
        assert_eq!(u32::from_le_bytes(tags[19..23].try_into().unwrap()), 6, "comment count");
    }
}
