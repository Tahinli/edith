//! Waveform peaks for drawing a clip's audio: min/max per time bucket, taken
//! off the same read-only decode path playback uses. Path in, peaks out — no
//! project model, so a lane rework cannot reach it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::thread;

use crate::AudioSession;
use crate::audio::AudioChunk;

/// `(min, max)` of every sample in each `1 / buckets_per_sec` window of
/// `stream` of `path`'s audio, from media time 0 (priming already trimmed by
/// the decoder). Channels are folded together: one envelope per clip, not per
/// channel.
///
/// `Ok(None)` for a file with no audio track — a silent source is valid, not a
/// failure. Values are clamped to `[-1.0, 1.0]`, and a bucket's pair always
/// straddles zero, so silence draws as a flat line.
///
/// Decoding the whole file runs at ~1700x realtime for an mp4's stereo AAC and
/// ~50x for a film's 5.1 AAC in an mkv (six channels through `rusty_aac`), but
/// it is linear in source length either way: callers cache the result per source
/// *and stream* — two streams of one file are two different envelopes, and a
/// cache keyed on the path alone would draw the first one under both. Memory is
/// flat in it, though: chunks are consumed as they arrive and only the buckets
/// are kept (2 hours at 10/s is 70k pairs).
///
/// A source long enough for it is decoded in **windows on several threads** and
/// the buckets stitched, which is what that linear cost is divided by:
/// `open_multi_streams` takes a window per call and seeks to it, and the fold to
/// one envelope is what makes the join safe — a bucket two windows straddle is
/// folded from both, and min/max cannot double-count. A film that cost 23 s of
/// one core costs about a quarter of that; a clip short enough that the extra
/// opens would outweigh the decode they save ([`jobs_for`]) is read exactly as
/// it always was, on this thread.
pub fn peaks(
    path: impl AsRef<Path>,
    stream: usize,
    buckets_per_sec: u32,
) -> crate::Result<Option<Vec<(f32, f32)>>> {
    peaks_over(path.as_ref(), stream, buckets_per_sec, None)
}

/// [`peaks`] with the split forced to `jobs` windows, which is how a test asks
/// for the same envelope by both routes.
fn peaks_over(
    path: &Path,
    stream: usize,
    buckets_per_sec: u32,
    jobs: Option<usize>,
) -> crate::Result<Option<Vec<(f32, f32)>>> {
    let Some((meta, rx)) = open_window(path, stream, 0.0, f64::INFINITY)? else {
        return Ok(None);
    };
    // Kept fractional: 44100 / 30 is not whole, and rounding it would drift a
    // bucket every few seconds over a long source.
    let per_bucket = f64::from(meta.sample_rate) / f64::from(buckets_per_sec.max(1));
    let channels = (meta.channels as usize).max(1);
    let rate = f64::from(meta.sample_rate.max(1));
    // A track whose length the container does not state cannot be cut into
    // windows at all: `secs` is 0 and the whole of it is read on this thread,
    // exactly as it was before there was a split.
    let secs = meta.total_samples.unwrap_or(0) as f64 / rate;
    let jobs = jobs.unwrap_or_else(|| jobs_for(secs)).max(1);
    if jobs == 1 {
        return Ok(Some(fold(rx, per_bucket, channels, u64::MAX)));
    }
    let window = secs / jobs as f64;
    // Every other window is spawned before this thread folds its own, so they
    // all decode side by side; each carries its own error home rather than
    // taking a worker down.
    let rest: Vec<_> = (1..jobs)
        .map(|k| {
            let path = path.to_path_buf();
            let start = window * k as f64;
            // The last window runs to the end of the file: `total_samples` is
            // the container's word for a length, and a tail past it must still
            // be drawn.
            let end = match k + 1 == jobs {
                true => f64::INFINITY,
                false => window * (k + 1) as f64,
            };
            thread::Builder::new()
                .name("waveform".into())
                .spawn(move || -> crate::Result<Vec<(f32, f32)>> {
                    match open_window(&path, stream, start, end)? {
                        Some((_, rx)) => Ok(fold(rx, per_bucket, channels, u64::MAX)),
                        None => Ok(Vec::new()),
                    }
                })
        })
        .collect::<Result<_, _>>()?;
    // This window ends where the next one's decoder was told to start, by the
    // same arithmetic, so the two meet with no gap; the chunk that straddles the
    // joint is folded into both and min/max does not double-count.
    let mut peaks = fold(rx, per_bucket, channels, (window * rate) as u64);
    for handle in rest {
        let part = handle.join().map_err(|_| "waveform worker panicked")??;
        merge(&mut peaks, &part);
    }
    Ok(Some(peaks))
}

/// How many threads a source of `secs` is worth splitting across: one below two
/// minutes of sound, where the extra opens (a Matroska's is a cluster walk) cost
/// more than the decode they would save, and never more than the machine has —
/// nor more than a handful, since an import asks this of every source at once.
///
/// ponytail: that last clause is a ceiling and not a bound — a library import of
/// twenty films runs as many of these as the background executor has threads,
/// each with eight of its own. The upgrade path is one pool the waveforms share,
/// which is also where a "visible range first" order would live.
fn jobs_for(secs: f64) -> usize {
    const PER_JOB_SECS: f64 = 60.0;
    const CEILING: usize = 8;
    if !secs.is_finite() || secs < 2.0 * PER_JOB_SECS {
        return 1;
    }
    let cores = thread::available_parallelism().map_or(1, |n| n.get());
    ((secs / PER_JOB_SECS) as usize).min(cores).min(CEILING).max(1)
}

/// `part` folded into `peaks` bucket for bucket. Both are indexed absolutely,
/// so this is a straight min/max: an empty bucket is `(0.0, 0.0)` and every
/// value is clamped to `[-1.0, 1.0]`, which makes the empty pair the identity.
fn merge(peaks: &mut Vec<(f32, f32)>, part: &[(f32, f32)]) {
    if part.len() > peaks.len() {
        peaks.resize(part.len(), (0.0, 0.0));
    }
    for (slot, &(lo, hi)) in peaks.iter_mut().zip(part) {
        slot.0 = slot.0.min(lo);
        slot.1 = slot.1.max(hi);
    }
}

/// One window of one stream, opened the way playback opens it.
fn open_window(
    path: &Path,
    stream: usize,
    start_secs: f64,
    end_secs: f64,
) -> crate::Result<Option<(crate::AudioMeta, Receiver<AudioChunk>)>> {
    let sources = [(PathBuf::from(path), stream)];
    AudioSession::open_multi_streams(&sources, &[(Some(0), start_secs, end_secs)])
}

/// Min/max per bucket of everything `rx` carries, at absolute bucket positions:
/// `start_sample` counts from the source's own first audible sample whatever
/// window the session was opened at. Stops at the first chunk beginning at or
/// past `stop_sample`, and dropping the receiver there is what stops that
/// window's decoder.
fn fold(
    rx: Receiver<AudioChunk>,
    per_bucket: f64,
    channels: usize,
    stop_sample: u64,
) -> Vec<(f32, f32)> {
    let mut peaks: Vec<(f32, f32)> = Vec::new();
    for chunk in rx {
        if chunk.start_sample >= stop_sample {
            break;
        }
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
    peaks
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{peaks, peaks_over};

    const BPS: u32 = 10;

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    #[test]
    fn peaks_follow_the_1hz_volume_pulse() {
        let peaks = peaks(asset("test_av.mp4"), 0, BPS)
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

    /// A song's clip draws like any other: this goes through the same
    /// `open_multi_streams` the timeline plays with, which reads a standalone
    /// audio file on stream 0 as readily as an mp4's AAC track.
    #[test]
    fn a_standalone_audio_file_has_peaks_too() {
        let peaks = peaks(asset("test_tone.mp3"), 0, BPS)
            .expect("open")
            .expect("test_tone.mp3 is audio");
        let want = 3 * BPS as usize; // 3 s of tone, mp3 padding may spill one
        assert!(
            peaks.len().abs_diff(want) <= 2,
            "{} buckets, want {want} +/-2",
            peaks.len()
        );
        // The fixture carries the A/V one's 1 Hz envelope, so the middle second
        // has a dip and a peak. As a ratio, and a looser one: the tone sits
        // around an eighth of full scale and mp3 does not preserve a true zero.
        let level = |&(lo, hi): &(f32, f32)| hi - lo;
        let band = &peaks[BPS as usize..][..BPS as usize];
        let dip = band.iter().map(level).fold(f32::MAX, f32::min);
        let loudest = band.iter().map(level).fold(0.0, f32::max);
        assert!(loudest > 0.05, "peak level only {loudest}");
        assert!(dip < 0.2 * loudest, "dip {dip} against peak {loudest}");
    }

    /// The shape the ask came from: a film in an mkv with a 5.1 AAC track,
    /// which no symphonia decoder takes (`aac: aac too complex`) and which
    /// therefore reaches the lane through `rusty_aac` and the stereo fold. A
    /// clip of one draws a waveform like any other -- an envelope that is not
    /// flat -- rather than the empty band a silent file makes.
    #[test]
    fn a_five_one_aac_mkv_draws_an_envelope() {
        let peaks = peaks(asset("test_hevc10.mkv"), 0, BPS)
            .expect("the 5.1 AAC track decodes")
            .expect("test_hevc10.mkv has an audio track");
        let want = 2 * BPS as usize; // 2 s of tones
        assert!(
            peaks.len().abs_diff(want) <= 2,
            "{} buckets, want {want} +/-2",
            peaks.len()
        );
        // Six tones, one per channel, folded to stereo: every bucket carries
        // signal, and a decode that came out silent (or clipped to a rail) is
        // exactly what this refuses.
        for (i, &(lo, hi)) in peaks.iter().enumerate() {
            assert!(
                (0.02..2.0).contains(&(hi - lo)),
                "bucket {i} is ({lo}, {hi})"
            );
        }
    }

    /// The split is an optimisation and nothing else: four windows decoded side
    /// by side draw the very envelope the single-threaded walk draws, bucket for
    /// bucket. The joints are where a window that started a sample late or
    /// stopped a chunk early would show, so this is the check that a film's
    /// waveform is still the film's.
    #[test]
    fn a_split_decode_draws_the_same_envelope() {
        let file = asset("test_av.mp4");
        let whole = peaks_over(&file, 0, BPS, Some(1))
            .expect("open")
            .expect("test_av.mp4 has an audio track");
        let split = peaks_over(&file, 0, BPS, Some(4))
            .expect("open")
            .expect("test_av.mp4 has an audio track");
        assert_eq!(whole.len(), split.len(), "the split lost or grew buckets");
        // Not bit-equality: a window seeks into the middle of an AAC stream and
        // its filterbank is primed from a preceding packet rather than carried
        // over, so a bucket can land a ten-thousandth away. A lane is drawn a
        // hundred pixels tall, where that is a thousandth of a pixel; a window
        // that started late or stopped early is off by whole tenths and is what
        // this catches.
        for (i, (w, s)) in whole.iter().zip(&split).enumerate() {
            assert!(
                (w.0 - s.0).abs() < 1e-3 && (w.1 - s.1).abs() < 1e-3,
                "bucket {i}: whole {w:?}, split {s:?}"
            );
        }
    }

    #[test]
    fn video_only_source_has_no_peaks() {
        assert!(
            peaks(asset("test_baseline.mp4"), 0, BPS)
                .expect("open")
                .is_none()
        );
    }

    /// Two streams of one file are two envelopes: the lane draws what the clip
    /// actually plays, so a cache keyed on the path alone would be a lie the
    /// user can see. Stream 1 of the fixture is 2 s of 220 Hz mono against
    /// stream 0's 4-second-long pulsed stereo pair.
    #[test]
    fn each_stream_of_a_file_has_its_own_envelope() {
        let multi = asset("test_multiaudio.mp4");
        let zero = peaks(&multi, 0, BPS).expect("open").expect("stream 0");
        let one = peaks(&multi, 1, BPS).expect("open").expect("stream 1");
        assert!(!zero.is_empty() && !one.is_empty());
        // Bucket for bucket, not merely somewhere: a cache keyed on the path
        // alone would hand the lane the *first* stream's shape for both, and
        // that is what this refuses.
        assert_ne!(zero, one, "both streams drew the same envelope");
        assert_ne!(zero[0], one[0], "...and they differ from the first bucket");
        // Stream 2 is AC-3, and it decodes now: it draws its own envelope like
        // any other stream rather than being refused.
        let two = peaks(&multi, 2, BPS).expect("AC-3 opens").expect("stream 2");
        assert!(!two.is_empty(), "the AC-3 stream drew nothing");
    }
}
