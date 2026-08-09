//! Waveform peaks for drawing a clip's audio: min/max per time bucket, taken
//! off the same read-only decode path playback uses. Path in, peaks out — no
//! project model, so a lane rework cannot reach it.

use std::path::Path;

use crate::AudioSession;

/// `(min, max)` of every sample in each `1 / buckets_per_sec` window of `path`'s
/// audio, from media time 0 (priming already trimmed by the decoder). Channels
/// are folded together: one envelope per clip, not per channel.
///
/// `Ok(None)` for a file with no audio track — a silent source is valid, not a
/// failure. Values are clamped to `[-1.0, 1.0]`, and a bucket's pair always
/// straddles zero, so silence draws as a flat line.
///
/// Decoding the whole file runs at ~1700x realtime, but it is still linear in
/// source length: callers cache the result per source, never per repaint.
pub fn peaks(
    path: impl AsRef<Path>,
    buckets_per_sec: u32,
) -> crate::Result<Option<Vec<(f32, f32)>>> {
    let Some((meta, rx)) = AudioSession::open(path)? else {
        return Ok(None);
    };
    // Kept fractional: 44100 / 30 is not whole, and rounding it would drift a
    // bucket every few seconds over a long source.
    let per_bucket = f64::from(meta.sample_rate) / f64::from(buckets_per_sec.max(1));
    let channels = (meta.channels as usize).max(1);
    let mut peaks: Vec<(f32, f32)> = Vec::new();

    for chunk in rx {
        for (frame, values) in chunk.samples.chunks(channels).enumerate() {
            let bucket = ((chunk.start_sample + frame as u64) as f64 / per_bucket) as usize;
            if bucket >= peaks.len() {
                peaks.resize(bucket + 1, (0.0, 0.0));
            }
            let slot = &mut peaks[bucket];
            for &v in values {
                let v = v.clamp(-1.0, 1.0);
                slot.0 = slot.0.min(v);
                slot.1 = slot.1.max(v);
            }
        }
    }
    Ok(Some(peaks))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::peaks;

    const BPS: u32 = 10;

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    #[test]
    fn peaks_follow_the_1hz_volume_pulse() {
        let peaks = peaks(asset("test_av.mp4"), BPS)
            .expect("open")
            .expect("test_av.mp4 has an audio track");
        // 5 s of source; the container's tail padding may spill one bucket.
        let want = 5 * BPS as usize;
        assert!(
            peaks.len().abs_diff(want) <= 1,
            "{} buckets, want {want} +/-1",
            peaks.len()
        );

        for (i, &(lo, hi)) in peaks.iter().enumerate() {
            assert!(
                (-1.0..=0.0).contains(&lo) && (0.0..=1.0).contains(&hi),
                "bucket {i} is ({lo}, {hi})"
            );
        }

        // The fixture's envelope is 0.5 + 0.5*sin(2*PI*t): a full silence every
        // second at t = 0.75, a full-scale peak at t = 0.25. So every second's
        // quietest bucket must be the 8th (t in [0.7, 0.8)) — a bucketing that
        // was off by a scale factor or dropped chunk positions would not line
        // the dips up second after second.
        for second in 0..5 {
            let band = &peaks[second * BPS as usize..][..BPS as usize];
            let level = |&(lo, hi): &(f32, f32)| hi - lo;
            let quietest = (0..band.len())
                .min_by(|&a, &b| level(&band[a]).total_cmp(&level(&band[b])))
                .expect("non-empty band");
            assert!(
                (6..=8).contains(&quietest),
                "second {second}: dip at bucket {quietest}, want 7 +/-1"
            );
            // Depth as a ratio, not an absolute: the fixture's sines sit around
            // an eighth of full scale, and that is the encoder's business.
            let (dip, loudest) = (
                level(&band[quietest]),
                band.iter().map(level).fold(0.0, f32::max),
            );
            assert!(loudest > 0.05, "second {second}: peak level only {loudest}");
            assert!(
                dip < 0.1 * loudest,
                "second {second}: dip {dip} against peak {loudest}, want near silence"
            );
        }
    }

    #[test]
    fn video_only_source_has_no_peaks() {
        assert!(
            peaks(asset("test_baseline.mp4"), BPS)
                .expect("open")
                .is_none()
        );
    }
}
