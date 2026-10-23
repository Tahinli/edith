//! Writes interleaved PCM as a `.flac` file through `ec-flac`, the family's own
//! encoder (fixed 4096-sample blocks, LPC, Rice partitions, stereo
//! decorrelation, and an MD5 in `STREAMINFO` that any decoder can check).
//!
//! The export path only ever asks for 16 bits; the bit depth is a parameter so
//! the parity tests can write 24-bit streams through the same function.

use std::path::Path;

use ec_flac::EncoderConfig;

/// Encode `samples` (interleaved by channel, each already within
/// `bits_per_sample`) and write the stream to `out`.
///
/// `EncoderConfig::default()` is the level the family ships. Edith has no
/// FLAC compression setting, so there is nothing to map: the export always
/// asks for that default.
pub fn write(
    out: &Path,
    samples: &[i32],
    channels: usize,
    bits_per_sample: u32,
    sample_rate: u32,
) -> crate::Result<()> {
    let bytes = ec_flac::encode(
        &EncoderConfig::default(),
        samples,
        channels,
        bits_per_sample,
        sample_rate,
    )
    .map_err(|e| format!("flac encode: {e}"))?;
    std::fs::write(out, bytes)?;
    Ok(())
}
