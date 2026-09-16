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
/// ~260x for a film's 5.1 AAC in an mkv (six channels through `ec-aac`), but
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

/// How many buckets the process-wide envelope memo may hold before it gives
/// them back. A bucket is a pair of `f32`s, so this is a ~64 MB ceiling over
/// every *source* a session has asked for -- not over the asks themselves --
/// and a single source larger than it is not held at all.
const PEAKS_MEMO_BUCKETS: usize = 8 << 20;

/// One file's envelope as the memo holds it: shared, so the second ask for the
/// same source is a refcount rather than a decode of the whole file.
pub(crate) type SharedPeaks = std::sync::Arc<Vec<(f32, f32)>>;

/// The identity a memo entry is keyed by: what was asked for *and* the file it
/// was answered from, so a source replaced on disk is read again rather than
/// served the envelope of bytes that are gone.
#[derive(PartialEq, Eq, Hash)]
struct PeaksKey {
    path: PathBuf,
    stream: usize,
    buckets_per_sec: u32,
    bytes: u64,
    mtime: Option<std::time::SystemTime>,
}

impl PeaksKey {
    fn of(path: &Path, stream: usize, buckets_per_sec: u32) -> Self {
        let stat = std::fs::metadata(path).ok();
        Self {
            path: path.to_path_buf(),
            stream,
            buckets_per_sec,
            bytes: stat.as_ref().map_or(0, std::fs::Metadata::len),
            mtime: stat.and_then(|m| m.modified().ok()),
        }
    }
}

/// The memo and how many buckets it is holding. One lock for both: a hit is a
/// lookup and a refcount, which is the whole of what the caller saves.
///
/// `std::sync::Mutex`, not `parking_lot`'s: edith's workspace has no
/// `parking_lot` of its own and this crate locks `std`'s everywhere (the audio
/// device, the tap), so a new dependency is not what a memo is worth.
fn peaks_memo() -> &'static std::sync::Mutex<(PeaksMemo, usize)> {
    static MEMO: std::sync::LazyLock<std::sync::Mutex<(PeaksMemo, usize)>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new((PeaksMemo::new(), 0)));
    &MEMO
}

/// Every envelope the process has been asked for, by identity.
type PeaksMemo = std::collections::HashMap<PeaksKey, SharedPeaks>;

/// [`peaks`] as a handle the caller can hold: the same envelope, shared rather
/// than copied.
///
/// The visualizer seat asks this of every picture rebuild it makes
/// ([`DecodeSession::open_visualizer`]): a rebuild happens once per look edit,
/// and a look edit is what a drag across the colour wheel is -- so an envelope
/// decoded per rebuild is a whole-file audio decode per pointer sample, for an
/// answer that has not changed.
///
/// Keyed on the file's own stat ([`PeaksKey`]) and bounded by
/// [`PEAKS_MEMO_BUCKETS`] *of what it holds*: past the ceiling the memo lets
/// everything go rather than evicting one entry at a time, because the entries
/// are large and a re-decode is the honest cost of asking for more than the
/// ceiling. A single source whose own envelope is bigger than the ceiling is
/// not memoized at all (see [`peaks_shared`]), so the ceiling is one -- an
/// entry that no clear can bring the memo back under would be a memo that
/// clears on every other ask.
pub(crate) fn peaks_shared(
    path: &Path,
    stream: usize,
    buckets_per_sec: u32,
) -> crate::Result<Option<SharedPeaks>> {
    let key = PeaksKey::of(path, stream, buckets_per_sec);
    if let Some(hit) = peaks_memo().lock().unwrap().0.get(&key) {
        return Ok(Some(SharedPeaks::clone(hit)));
    }
    let Some(peaks) = peaks_over(path, stream, buckets_per_sec, None)? else {
        return Ok(None);
    };
    let shared = SharedPeaks::new(peaks);
    let mut memo = peaks_memo().lock().unwrap();
    // An entry bigger than the ceiling is never memoized at all: keeping it
    // would put the count permanently over the ceiling, so the next ask of any
    // *other* key would clear the map and drop it -- and this source would pay
    // a whole-file decode per rebuild anyway, which is the stall the memo is
    // here to remove. The caller gets its envelope either way.
    if shared.len() <= PEAKS_MEMO_BUCKETS {
        if memo.1 + shared.len() > PEAKS_MEMO_BUCKETS {
            memo.0.clear();
            memo.1 = 0;
        }
        // The map may already hold this very key: two workers that missed
        // together both decode, and the slower one inserts after the faster
        // one. The count follows what the map *holds*, so the replaced entry's
        // buckets come off before the new one's go on -- otherwise every such
        // race leaves the count high for good (only a clear resets it) and
        // later asks clear the memo early, dropping entries that then pay a
        // whole-file decode to be rebuilt.
        if let Some(previous) = memo.0.insert(key, SharedPeaks::clone(&shared)) {
            memo.1 -= previous.len();
        }
        memo.1 += shared.len();
    }
    Ok(Some(shared))
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
/// corner-cut: that last clause is a ceiling and not a bound — a library import of
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

    use super::{peaks, peaks_over, peaks_shared};

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
    /// therefore reaches the lane through `ec-aac` and the stereo fold. A
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

    /// The memo is keyed on the *ask*, not on the file: a second visualizer
    /// rebuild of one source reuses the envelope, and one of another stream is
    /// decoded rather than handed the first one's -- the lie
    /// [`each_stream_of_a_file_has_its_own_envelope`] refuses, one cache layer
    /// down, where the visualizer seat reads it.
    #[test]
    fn the_shared_memo_answers_per_stream() {
        let multi = asset("test_multiaudio.mp4");
        let zero = peaks_shared(&multi, 0, BPS)
            .expect("open")
            .expect("stream 0");
        let again = peaks_shared(&multi, 0, BPS)
            .expect("open")
            .expect("stream 0, second ask");
        // Shared, not decoded again -- which is the whole of what the second
        // visualizer rebuild saves.
        assert!(
            std::sync::Arc::ptr_eq(&zero, &again),
            "the second ask decoded the file again"
        );
        // ...and the very envelope a plain ask draws, so nothing is served
        // through the memo that the decoder would not have said.
        let plain = peaks(&multi, 0, BPS).expect("open").expect("stream 0");
        assert_eq!(*zero, plain);
        let one = peaks_shared(&multi, 1, BPS)
            .expect("open")
            .expect("stream 1");
        assert_ne!(*zero, *one, "a stream took another's memo entry");
    }

    /// A source replaced on disk is read again: the key carries the file's own
    /// stat, so the memo cannot paint the old file's envelope for a clip that
    /// no longer plays it. Asked of a *copy*, because the fixtures are
    /// read-only for every other test in this binary.
    #[test]
    fn a_replaced_source_is_read_again() {
        let path = std::env::temp_dir().join(format!("edith-peaks-memo-{}.mp4", std::process::id()));
        let short = std::fs::read(asset("test_multiaudio.mp4")).expect("read the fixture");
        let long = std::fs::read(asset("test_av.mp4")).expect("read the fixture");
        // The two halves below are the length half and the clock half of the
        // key, so their sizes have to differ for the first of them to be it.
        assert_ne!(short.len(), long.len(), "fixtures must differ in length");

        std::fs::write(&path, &short).expect("write the copy");
        let first = peaks_shared(&path, 0, BPS).expect("open").expect("copy");
        let again = peaks_shared(&path, 0, BPS).expect("open").expect("copy");
        assert!(std::sync::Arc::ptr_eq(&first, &again), "the copy was not memoized");

        // Other bytes behind the same path. A key without the file's length
        // (or without any stat at all) serves `first` here -- the envelope of
        // bytes that are gone.
        std::fs::write(&path, &long).expect("rewrite the copy");
        let replaced = peaks_shared(&path, 0, BPS).expect("open").expect("copy");
        assert_ne!(*replaced, *first, "the memo served the replaced file's envelope");
        assert_eq!(
            *replaced,
            peaks(&path, 0, BPS).expect("open").expect("copy"),
            "the memo's answer is not what a plain ask draws"
        );

        // The *same* bytes written again: same length, a newer clock. Resolution
        // is the filesystem's, so give it a moment to move.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, &long).expect("touch the copy");
        let touched = peaks_shared(&path, 0, BPS).expect("open").expect("copy");
        assert_eq!(*touched, *replaced);
        // A key with length but no mtime hands the old handle back -- for a file
        // that changed under it, which is the serve the mtime is in the key for.
        assert!(
            !std::sync::Arc::ptr_eq(&replaced, &touched),
            "the mtime is not part of the key"
        );

        let _ = std::fs::remove_file(&path);
    }
}
