//! Waveform peaks for drawing a clip's audio: min/max per time bucket, taken
//! off the same read-only decode path playback uses. Path in, peaks out — no
//! project model, so a lane rework cannot reach it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::thread;

use crate::audio::AudioChunk;
use crate::AudioSession;

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
#[derive(PartialEq, Eq, Hash, Clone)]
struct PeaksKey {
    path: PathBuf,
    stream: usize,
    buckets_per_sec: u32,
    bytes: u64,
    mtime: Option<std::time::SystemTime>,
}

impl PeaksKey {
    /// The same identity for an ask that has nothing to do with a rate: the
    /// scalars [`viz_summary`] reads off a file are its samples, not its
    /// buckets, and one measuring is enough for every rate asked of it. `0` is
    /// not a rate any ask can use ([`per_bucket_of`] floors it), so a summary
    /// never collides with an envelope.
    fn stream_of(path: &Path, stream: usize) -> Self {
        Self::of(path, stream, 0)
    }

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

/// Test-only: how many times an ask *for one key* had to decode rather than
/// answer from what the memo already held. A second ask of the same source --
/// trackless or not -- must not move it, which is what makes "the memo
/// answered" observable.
///
/// Keyed by the ask rather than counted process-wide: a test's claim is about
/// *its* source, the suite runs in parallel by default, and a decode another
/// test made would otherwise land inside the delta -- failing the claim for a
/// reason that has nothing to do with the code. Every reader's source is a
/// private copy of a fixture, so its key is that test's alone.
#[cfg(test)]
static MEMO_DECODES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<PeaksKey, usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// [`MEMO_DECODES`] for one ask, for a test that measures a delta across its own.
#[cfg(test)]
fn memo_decodes(path: &Path, stream: usize, buckets_per_sec: u32) -> usize {
    MEMO_DECODES
        .lock()
        .unwrap()
        .get(&PeaksKey::of(path, stream, buckets_per_sec))
        .copied()
        .unwrap_or(0)
}

/// What the memo holds for one key: an envelope, or the stable answer that the
/// source carries no audio track at all. The second is worth keeping too -- a
/// trackless source re-opens its container on every ask otherwise, the same
/// per-rebuild cost the envelope memo exists to remove -- and it holds no
/// buckets, so the counter stays honest about what the map holds.
enum MemoEntry {
    /// A source with an audio track, shared so a second ask is a refcount.
    Envelope(SharedPeaks),
    /// A source with no audio track: a valid, empty answer ([`peaks`]'s
    /// `Ok(None)`), kept rather than re-derived from the file each time.
    NoTrack,
}

impl MemoEntry {
    /// The answer this entry stands for, as [`peaks_shared`] returns it.
    fn envelope(&self) -> Option<SharedPeaks> {
        match self {
            MemoEntry::Envelope(shared) => Some(SharedPeaks::clone(shared)),
            MemoEntry::NoTrack => None,
        }
    }
}

/// Every answer the process has been asked for, by identity.
type PeaksMemo = std::collections::HashMap<PeaksKey, MemoEntry>;

/// [`peaks`] as a handle the caller can hold: the same envelope, shared rather
/// than copied, under a ceiling.
///
/// Keyed on the file's own stat ([`PeaksKey`]) and bounded by [`cap`] *of what
/// it holds*: past the ceiling the memo lets everything go rather than evicting
/// one entry at a time, because the entries are large and a re-decode is the
/// honest cost of asking for more than the ceiling. A single source whose own
/// envelope is bigger than the ceiling is not memoized at all (the caller still
/// gets its handle), so the ceiling is one -- an entry that no clear can bring
/// the memo back under would be a memo that clears on every other ask.
///
/// A trackless source's `Ok(None)` is kept too: it is an answer the file will
/// give again, and without it a visualizer clip on a silent source re-opens
/// the container on every picture rebuild -- the whole cost the memo removes,
/// paid once per look edit. It holds no buckets, so it never counts against
/// the ceiling. A failed ask is *not* kept: a decode that failed once may fail
/// for a reason that has gone away, and caching it would make that permanent.
///
/// The visualizer seat does not ask this directly any more -- it asks
/// [`viz_envelope`], which answers with this for a source the memo holds and
/// with windows of it for one it cannot. `cap` is [`PEAKS_MEMO_BUCKETS`] there;
/// a test drives it down to a handful of buckets so the over-the-ceiling path
/// is walked without an eight-million-bucket fixture.
fn peaks_shared_capped(
    path: &Path,
    stream: usize,
    buckets_per_sec: u32,
    cap: usize,
) -> crate::Result<Option<SharedPeaks>> {
    let key = PeaksKey::of(path, stream, buckets_per_sec);
    if let Some(hit) = peaks_memo().lock().unwrap().0.get(&key) {
        return Ok(hit.envelope());
    }
    #[cfg(test)]
    {
        *MEMO_DECODES.lock().unwrap().entry(key.clone()).or_insert(0) += 1;
    }
    // A failure is *not* memoized: `?` leaves the map untouched, so a decode
    // that failed once -- a file caught mid-write, a busy device -- is retried
    // rather than frozen for the process's life. Only the two successful
    // answers are kept: the envelope, and the trackless `None`.
    let answer = peaks_over(path, stream, buckets_per_sec, None)?;
    let entry = match answer {
        None => MemoEntry::NoTrack,
        Some(peaks) => MemoEntry::Envelope(SharedPeaks::new(peaks)),
    };
    // The handle the caller gets is the answer itself, whether or not the memo
    // keeps a copy: an over-the-ceiling envelope is still returned below.
    let handle = entry.envelope();
    let mut guard = peaks_memo().lock().unwrap();
    let (memo, count) = &mut *guard;
    memoize(memo, count, cap, key, entry);
    Ok(handle)
}

/// What a memo holds a bucket count of. Both memos below are ceilinged in
/// *buckets*, so every entry has to say how many it brings -- and one
/// arithmetic serves both rather than two that can drift apart.
trait Bucketed {
    fn buckets(&self) -> usize;
}

impl Bucketed for MemoEntry {
    fn buckets(&self) -> usize {
        match self {
            MemoEntry::Envelope(shared) => shared.len(),
            MemoEntry::NoTrack => 0,
        }
    }
}

impl Bucketed for SharedPeaks {
    fn buckets(&self) -> usize {
        self.len()
    }
}

/// Record `entry` under `key` in `memo`, keeping `count` equal to the buckets
/// the map holds. The arithmetic is what keeps the ceiling a ceiling:
///
/// - an entry bigger than `cap` is not held at all -- keeping it would put the
///   count permanently over the ceiling, so the next ask of any *other* key
///   would clear the map and drop it, and this source would pay a whole-file
///   decode per rebuild anyway. The caller keeps the handle it was handed;
/// - when the new entry would take the count past `cap`, the map is emptied
///   rather than evicted one entry at a time (the entries are large);
/// - an insert that replaces an entry the count already includes subtracts the
///   replaced length first, so the racing-workers case -- two workers that
///   missed one key each decode and insert it -- does not leave the count high
///   for good: only a clear resets it, and a high count makes later asks clear
///   early, dropping entries that then pay a whole-file decode to be rebuilt.
fn memoize<K: std::hash::Hash + Eq, V: Bucketed>(
    memo: &mut std::collections::HashMap<K, V>,
    count: &mut usize,
    cap: usize,
    key: K,
    entry: V,
) {
    let len = entry.buckets();
    if len > cap {
        return;
    }
    if *count + len > cap {
        memo.clear();
        *count = 0;
    }
    if let Some(previous) = memo.insert(key, entry) {
        *count -= previous.buckets();
    }
    *count += len;
}

/// How much sound one window of an over-the-ceiling source covers: 30 s at the
/// visualizer's 4000 buckets a second is 120k buckets, a megabyte, against the
/// 230 MB the whole of a two-hour film's envelope is.
const VIZ_WINDOW_SECS: f64 = 30.0;

/// Decoded past each end of that window. A seek lands where it lands (a
/// standalone mp3's pre-roll is 8192 samples), so the first and last buckets of
/// a window's own decode can hold part of a bucket the neighbouring window
/// covers too; three seconds either side keeps those edges well clear of the
/// two-second window a frame actually asks for.
const VIZ_WINDOW_SEEK_SECS: f64 = 3.0;

/// How many buckets the window memo holds before it gives them back: 4M
/// buckets is 32 MB, some thirty windows -- fifteen minutes of sound about the
/// playhead, for every source in the session together, against the eight
/// million a single film's envelope would want. Past it the map is emptied
/// rather than evicted one window at a time: the paint slides forward, so the
/// window it is in is the one worth losing last, and re-decoding it is a second
/// of sound rather than two hours.
const VIZ_WINDOW_MEMO_BUCKETS: usize = 4 << 20;

/// The loudest bucket of an envelope: what a visualizer column is scaled by
/// ([`crate::decode::visualizer_i420`]), and therefore a property of the
/// *whole* file -- a window that derived it from itself would rescale every
/// column as it slid.
pub(crate) fn peak_of(peaks: &[(f32, f32)]) -> f32 {
    peaks
        .iter()
        .map(|&(lo, hi)| lo.abs().max(hi.abs()))
        .fold(0.0f32, f32::max)
}

/// What a visualizer frame needs from the whole file, in two scalars: how many
/// samples it carries, and the loudest of them. Neither depends on the
/// bucketing -- a bucket is as loud as the loudest sample in it, and a sample
/// lands in the last bucket whatever that bucket is -- so both come off one
/// pass that keeps no buckets at all ([`viz_summary`]). A film's two hours at
/// 4000 buckets a second is 230 MB of pairs; this is sixteen bytes of it.
pub(crate) struct VizSummary {
    /// One past the file's last sample, counted from its first audible one --
    /// the same arithmetic [`fold`] counts buckets with.
    samples: u64,
    /// The rate those samples were counted at, which is what turns an ask's
    /// buckets-per-second into samples per bucket. Carried here because the
    /// summary outlives the probe that read it.
    sample_rate: u32,
    /// The file's loudest sample, clamped to full scale like every bucket
    /// value.
    pub(crate) peak: f32,
}

impl VizSummary {
    /// The file's length in buckets of `per_bucket` samples: what
    /// `peaks.len()` would have been. `fold` puts a sample in
    /// `(sample / per_bucket)` truncated, so the last sample decides the last
    /// bucket -- and a file with no samples has none.
    fn buckets(&self, per_bucket: f64) -> usize {
        if self.samples == 0 {
            return 0;
        }
        ((self.samples - 1) as f64 / per_bucket) as usize + 1
    }
}

/// The visualizer seat's envelope: what a picture of a clip is painted from.
///
/// The seat is re-opened on every look edit and every seek, so the second ask
/// has to be a refcount. For a source whose whole envelope the memo holds -- a
/// song's, a short clip's -- [`peaks_shared_capped`] answers exactly that. A
/// film's is past the memo's ceiling and never held, and what the seat asks for
/// one is a *window* of it about the frame's own time, which is all a
/// two-second picture reads, plus the pass that reads the whole file's two
/// scalars once ([`VizSummary`]).
pub(crate) enum VizEnvelope {
    /// No audio track -- and what the seat falls back to when the ask itself
    /// fails: a frame always exists, and is never worth failing a span over.
    Silent,
    /// The memo holds the whole envelope: the same slice every frame, as it
    /// always was.
    Whole(SharedPeaks),
    /// Past the ceiling: the file's two scalars, and one window of it at a time.
    Windowed(VizWindows),
}

impl VizEnvelope {
    /// The envelope the frame at `t_secs` is painted from: the whole of it, or
    /// the window `t_secs` falls in -- decoded once per window per process.
    pub(crate) fn view_at(&mut self, t_secs: f64) -> crate::decode::VizView<'_> {
        match self {
            Self::Silent => crate::decode::VizView::whole(&[]),
            Self::Whole(shared) => crate::decode::VizView::whole(shared.as_slice()),
            Self::Windowed(windows) => windows.view_at(t_secs),
        }
    }
}

/// One source's windows, and the window the paint is in.
pub(crate) struct VizWindows {
    /// The ask: file, stream and rate, so a window of the same source asked at
    /// two rates is two windows.
    key: PeaksKey,
    /// The rate the ask named, in buckets per second. The file's own rate is in
    /// the summary; this and the two of them make samples per bucket, and the
    /// ask is what turns a frame's time into buckets and back.
    buckets_per_sec: u32,
    summary: std::sync::Arc<VizSummary>,
    /// The window the last frame was served from, and the window it is: a
    /// sliding paint asks this thirty times a second, and only a look edit or a
    /// seek ever moves it to another.
    current: Option<(usize, SharedPeaks)>,
}

impl VizWindows {
    /// Samples per bucket, at the rate this seat asked for.
    fn per_bucket(&self) -> f64 {
        per_bucket_of(self.summary.sample_rate, self.buckets_per_sec)
    }

    /// Buckets per second -- the ask's own rate. A bucket holds a fraction of a
    /// sample at a rate above the file's, which is exactly why the file's own
    /// rate has to be the one that divides.
    fn per_sec(&self) -> f64 {
        f64::from(self.buckets_per_sec.max(1))
    }

    /// The window `t_secs` falls in: a *window* of 30 s at a fixed grid, so the
    /// same second of the same file is the same window whichever seat asks.
    fn view_at(&mut self, t_secs: f64) -> crate::decode::VizView<'_> {
        let per_sec = self.per_sec();
        let total = self.summary.buckets(self.per_bucket());
        let per_window = (VIZ_WINDOW_SECS * per_sec) as usize;
        let seek = (VIZ_WINDOW_SEEK_SECS * per_sec) as usize;
        let t_secs = if t_secs.is_finite() {
            t_secs.max(0.0)
        } else {
            0.0
        };
        let first = ((t_secs * per_sec) as usize / per_window) * per_window;
        // The decode is asked for from the seek margin *below* the window's own
        // first bucket, and the whole of what it returns is served: `from` is
        // where the slice starts, absolutely, which is the one thing a caller
        // cannot recover from the slice itself. The margin is what the frames at
        // the window's own edges read -- the painter's window is two seconds
        // wide and centred on the frame -- and it is a *seek* margin rather than
        // a served one, because a decode's own first and last buckets can hold
        // part of a bucket the neighbouring window covers too.
        let from = first.saturating_sub(seek);
        if first >= total {
            // Past the end of the file -- which is where a source shorter than
            // the clip playing it leaves the last frames. Nothing to decode:
            // the painter measures "past the file" against `total` and draws
            // silence there, exactly as it did when it held the whole envelope.
            return crate::decode::VizView::window(&[], first, total, self.summary.peak, per_sec);
        }
        if self.current.as_ref().map(|(at, _)| *at) != Some(first) {
            // A window that will not open keeps the frame's own silence for the
            // rest of it: the seat is what remembers that, and only while it is
            // painting in this window -- the memo keeps successes only, so the
            // next seek (and the next window) asks the file again, and a source
            // that comes back (a share remounting, a file finishing its write)
            // paints again. Re-asking every frame instead would hammer a file
            // that is not there thirty times a second.
            let window = viz_window(
                &self.key,
                self.summary.sample_rate,
                self.buckets_per_sec,
                from,
                per_window + 2 * seek,
            )
            .unwrap_or_else(|_| SharedPeaks::new(Vec::new()));
            self.current = Some((first, window));
        }
        let (at, peaks) = self.current.as_ref().expect("just set");
        debug_assert_eq!(
            *at, first,
            "the seat kept a window other than the one it asked for"
        );
        crate::decode::VizView::window(peaks.as_slice(), from, total, self.summary.peak, per_sec)
    }
}

/// A window's identity: the file it came from, and its own first bucket.
#[derive(PartialEq, Eq, Hash)]
struct WindowKey {
    file: PeaksKey,
    from: usize,
}

impl WindowKey {
    fn of(file: &PeaksKey, from: usize) -> Self {
        Self {
            file: file.clone(),
            from,
        }
    }
}

/// The windows every seat of this process has asked for, and how many buckets
/// they hold between them.
fn viz_windows(
) -> &'static std::sync::Mutex<(std::collections::HashMap<WindowKey, SharedPeaks>, usize)> {
    static MEMO: std::sync::LazyLock<
        std::sync::Mutex<(std::collections::HashMap<WindowKey, SharedPeaks>, usize)>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new((std::collections::HashMap::new(), 0)));
    &MEMO
}

/// The whole-file scalars every over-the-ceiling source has been measured for,
/// by the stream's own identity: [`VizSummary`] is sixteen bytes, so this map
/// is bounded by how many *streams* a session has asked about rather than by
/// how long they are.
fn viz_summaries(
) -> &'static std::sync::Mutex<std::collections::HashMap<PeaksKey, std::sync::Arc<VizSummary>>> {
    static MEMO: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<PeaksKey, std::sync::Arc<VizSummary>>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    &MEMO
}

/// Test-only: how many times the visualizer's own ask had to open the file --
/// a window of an envelope, or one window of a [`viz_summary`] pass. The
/// second ask for the same window of the same source must not move it.
#[cfg(test)]
static VIZ_DECODES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// [`VIZ_DECODES`], for a test that measures a delta across its own asks.
#[cfg(test)]
fn viz_decodes() -> usize {
    VIZ_DECODES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Test-only: how many frames of *sound* the summary pass folded
/// ([`fold_summary`]); not the window decodes, so a delta across one ask is how
/// many times over the file's own length that pass read. The windows partition
/// the file, so a pass is one file's worth -- and a pass that also folded the
/// ask's whole-file probe would be up to twice that.
#[cfg(test)]
static VIZ_FOLDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`VIZ_FOLDED`], for a test that measures a delta across its own ask.
#[cfg(test)]
fn viz_folded() -> u64 {
    VIZ_FOLDED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The visualizer seat's own ask: the whole envelope when the memo holds it,
/// and otherwise windows of it. Never decoded per frame either way -- the
/// per-frame cost of a look drag is what this exists to remove.
pub(crate) fn viz_envelope(
    path: &Path,
    stream: usize,
    buckets_per_sec: u32,
) -> crate::Result<VizEnvelope> {
    viz_envelope_capped(path, stream, buckets_per_sec, PEAKS_MEMO_BUCKETS)
}

/// [`viz_envelope`] against a ceiling a test can drive down to a handful of
/// buckets, so the windowed route is walked on a five-second fixture. `cap` is
/// [`PEAKS_MEMO_BUCKETS`] everywhere but in a test.
pub(crate) fn viz_envelope_capped(
    path: &Path,
    stream: usize,
    buckets_per_sec: u32,
    cap: usize,
) -> crate::Result<VizEnvelope> {
    let key = PeaksKey::of(path, stream, buckets_per_sec);
    // An answer the memo already holds ends it, without opening anything: a
    // song's whole envelope, or the stable "no audio track" a silent source
    // gives.
    match peaks_memo().lock().unwrap().0.get(&key) {
        Some(MemoEntry::Envelope(shared)) => {
            return Ok(VizEnvelope::Whole(SharedPeaks::clone(shared)))
        }
        Some(MemoEntry::NoTrack) => return Ok(VizEnvelope::Silent),
        None => {}
    }
    // A source this process has already measured past the ceiling: the summary
    // is the answer, and it costs an open to have asked for one.
    let stream_key = PeaksKey::stream_of(path, stream);
    if let Some(summary) = viz_summaries().lock().unwrap().get(&stream_key) {
        let summary = std::sync::Arc::clone(summary);
        return Ok(VizEnvelope::Windowed(VizWindows {
            key,
            buckets_per_sec,
            summary,
            current: None,
        }));
    }
    // A miss, so the file has to be looked at once. The container's own word
    // for its length decides which ask this is, before anything is decoded:
    // what the memo can hold is asked for exactly as it always was, and only
    // what it cannot is served in windows.
    let Some((meta, rx)) = open_window(path, stream, 0.0, f64::INFINITY)? else {
        let mut guard = peaks_memo().lock().unwrap();
        let (memo, count) = &mut *guard;
        memoize(memo, count, cap, key, MemoEntry::NoTrack);
        return Ok(VizEnvelope::Silent);
    };
    let per_bucket = per_bucket_of(meta.sample_rate, buckets_per_sec);
    let fits = match meta.total_samples {
        // The stated length is the container's, which is a lower bound on what
        // the decode finds (a tail past it is still drawn), so a file near the
        // ceiling can turn out larger. The windowed route below catches that.
        Some(samples) => (samples as f64 / per_bucket) as usize + 1 <= cap,
        // A length the container does not state cannot be windowed -- neither
        // by this ask nor by the split inside it. Read exactly as it always
        // was, and held when it fits.
        None => true,
    };
    if fits {
        // The receiver the probe opened is dropped here: the whole-file ask
        // opens its own, which is the only ask this path has ever made.
        drop(rx);
        let answer = peaks_shared_capped(path, stream, buckets_per_sec, cap)?;
        return Ok(match answer {
            None => VizEnvelope::Silent,
            Some(shared) if shared.len() > cap => {
                // The container understated its length, so this ask paid for a
                // whole envelope it cannot keep. Measure the file off it --
                // both scalars are exact here -- and hand the seat windows of
                // it instead, so the *next* rebuild is the cheap one.
                let summary = std::sync::Arc::new(VizSummary {
                    samples: (shared.len() as f64 * per_bucket) as u64,
                    sample_rate: meta.sample_rate,
                    peak: peak_of(&shared),
                });
                viz_summaries()
                    .lock()
                    .unwrap()
                    .insert(stream_key, summary.clone());
                VizEnvelope::Windowed(VizWindows {
                    key,
                    buckets_per_sec,
                    summary,
                    current: None,
                })
            }
            Some(shared) => VizEnvelope::Whole(shared),
        });
    }
    // Past the ceiling: the file's two scalars off the probe's own window and
    // its neighbours, then a window of it per painted frame.
    let summary = viz_summary(path, stream, &meta, rx)?;
    Ok(VizEnvelope::Windowed(VizWindows {
        key,
        buckets_per_sec,
        summary,
        current: None,
    }))
}

/// The two scalars of [`VizSummary`], read off one pass that keeps no buckets.
///
/// The same windows, the same threads and the same opens [`peaks_over`] decodes
/// a whole envelope with -- `rx` is that ask's own first window, the probe's --
/// because the peak has to be the peak of the envelope the memo would have
/// held: a frame scaled by any other number is a different picture. A stop
/// sample would be the one way to lose a bucket the envelope has (and, with it,
/// the loudest sample in the file), so there are none here: what the windows
/// fold between them is everything the file delivers.
fn viz_summary(
    path: &Path,
    stream: usize,
    meta: &crate::AudioMeta,
    rx: Receiver<AudioChunk>,
) -> crate::Result<std::sync::Arc<VizSummary>> {
    let key = PeaksKey::stream_of(path, stream);
    if let Some(hit) = viz_summaries().lock().unwrap().get(&key) {
        return Ok(std::sync::Arc::clone(hit));
    }
    let rate = f64::from(meta.sample_rate.max(1));
    let channels = (meta.channels as usize).max(1);
    let secs = meta.total_samples.unwrap_or(0) as f64 / rate;
    let jobs = jobs_for(secs).max(1);
    let window = secs / jobs as f64;
    let rest: Vec<_> = (1..jobs)
        .map(|k| {
            let path = path.to_path_buf();
            let start = window * k as f64;
            let end = match k + 1 == jobs {
                true => f64::INFINITY,
                false => window * (k + 1) as f64,
            };
            #[cfg(test)]
            VIZ_DECODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            thread::Builder::new().name("waveform".into()).spawn(
                move || -> crate::Result<(f32, u64)> {
                    match open_window(&path, stream, start, end)? {
                        Some((window_meta, window_rx)) => Ok(fold_summary(
                            window_rx,
                            u64::MAX,
                            (window_meta.channels as usize).max(1),
                        )),
                        None => Ok((0.0, 0)),
                    }
                },
            )
        })
        .collect::<Result<_, _>>()?;
    #[cfg(test)]
    VIZ_DECODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The first window stops at the joint the second one was told to start at,
    // exactly as the envelope's own split stops it -- and the windows between
    // them are the file, so a pass reads it once. Folding this receiver whole
    // (it is the ask's probe, opened at the file's end) would have every later
    // window decode the same sound again: measured at 1.5x the file on a
    // two-window split and 1.875x at the eight-window ceiling, on the critical
    // path of the first look edit. A window with no joint to stop at -- the only
    // window there is -- folds everything it is handed, which is what the
    // single-threaded envelope ask does with it.
    let stop = match jobs {
        1 => u64::MAX,
        _ => (window * rate) as u64,
    };
    let (mut peak, mut samples) = fold_summary(rx, stop, channels);
    for handle in rest {
        let (part_peak, part_samples) = handle.join().map_err(|_| "waveform worker panicked")??;
        peak = peak.max(part_peak);
        samples = samples.max(part_samples);
    }
    let summary = std::sync::Arc::new(VizSummary {
        samples,
        sample_rate: meta.sample_rate,
        peak,
    });
    viz_summaries()
        .lock()
        .unwrap()
        .insert(key, std::sync::Arc::clone(&summary));
    Ok(summary)
}

/// One window of one file's envelope, decoded and kept. A window is what the
/// memo can hold of a film: thirty seconds of sound about the frame's own time,
/// so the look edit that re-opens the seat decodes a window rather than the
/// file, and the *second* edit reads the one the first left.
fn viz_window(
    key: &PeaksKey,
    sample_rate: u32,
    buckets_per_sec: u32,
    from: usize,
    buckets: usize,
) -> crate::Result<SharedPeaks> {
    let memo_key = WindowKey::of(key, from);
    if let Some(hit) = viz_windows().lock().unwrap().0.get(&memo_key) {
        return Ok(SharedPeaks::clone(hit));
    }
    #[cfg(test)]
    VIZ_DECODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Both units, from the two rates: `per_bucket` is what the fold counts
    // samples in, `per_sec` is what a window's own ends are named in -- and a
    // bucket is a fraction of a *second* over the ask's rate, never of a sample,
    // whatever either rate is.
    let per_bucket = per_bucket_of(sample_rate, buckets_per_sec);
    let per_sec = f64::from(buckets_per_sec.max(1));
    let start = from as f64 / per_sec;
    let end = (from + buckets) as f64 / per_sec;
    let Some((meta, rx)) = open_window(&key.path, key.stream, start, end)? else {
        // The track the probe found is gone (the file changed under the key):
        // an empty window, which paints its own stretch of the flat line.
        return Ok(SharedPeaks::new(Vec::new()));
    };
    let channels = (meta.channels as usize).max(1);
    let peaks = SharedPeaks::new(fold_window(rx, per_bucket, channels, from));
    let mut guard = viz_windows().lock().unwrap();
    let (memo, count) = &mut *guard;
    // The ceiling arithmetic is [`memoize`]'s, on a map whose entries are
    // windows: an entry bigger than the ceiling is handed back but not held,
    // and one that replaces another subtracts what it replaced.
    memoize(
        memo,
        count,
        VIZ_WINDOW_MEMO_BUCKETS,
        memo_key,
        SharedPeaks::clone(&peaks),
    );
    Ok(peaks)
}

/// Min/max per bucket of everything `rx` carries, from `from` on: the window's
/// own first bucket is `from`, and anything the decoder hands over before it --
/// a seek lands where it lands -- is dropped rather than folded at an index the
/// window does not have. The *last* bucket is partial too, which is what the
/// seek margin in [`viz_window`] is for.
fn fold_window(
    rx: Receiver<AudioChunk>,
    per_bucket: f64,
    channels: usize,
    from: usize,
) -> Vec<(f32, f32)> {
    let mut peaks: Vec<(f32, f32)> = Vec::new();
    for chunk in rx {
        for (frame, values) in chunk.samples.chunks(channels).enumerate() {
            let bucket = ((chunk.start_sample + frame as u64) as f64 / per_bucket) as usize;
            if bucket < from {
                continue;
            }
            let i = bucket - from;
            if i >= peaks.len() {
                peaks.resize(i + 1, (0.0, 0.0));
            }
            let slot = &mut peaks[i];
            for &v in values {
                let v = v.clamp(-1.0, 1.0);
                slot.0 = slot.0.min(v);
                slot.1 = slot.1.max(v);
            }
        }
    }
    peaks
}

/// [`fold`]'s two answers without its buckets: the loudest sample the window
/// carries, and how far the file it came from runs. Clamped and counted
/// exactly as `fold` clamps and counts them, so the peak of a whole-file pass
/// is the peak of the whole-file envelope.
fn fold_summary(rx: Receiver<AudioChunk>, stop_sample: u64, channels: usize) -> (f32, u64) {
    let (mut peak, mut samples) = (0.0f32, 0u64);
    for chunk in rx {
        if chunk.start_sample >= stop_sample {
            break;
        }
        #[cfg(test)]
        VIZ_FOLDED.fetch_add(
            (chunk.samples.len() / channels) as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        samples = samples.max(chunk.start_sample + (chunk.samples.len() / channels) as u64);
        for &v in &chunk.samples {
            peak = peak.max(v.clamp(-1.0, 1.0).abs());
        }
    }
    (peak, samples)
}

/// Samples per bucket. Guarded at one both ways, because a container that does
/// not state a rate (or states a zero one) would otherwise make this zero,
/// every sample would land in bucket `usize::MAX`, and the fold's own
/// `resize(bucket + 1)` would be a length that does not exist. One keeps the
/// fold total, and `buckets_per_sec` is floored where it is asked for as well.
fn per_bucket_of(sample_rate: u32, buckets_per_sec: u32) -> f64 {
    f64::from(sample_rate.max(1)) / f64::from(buckets_per_sec.max(1))
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
    let per_bucket = per_bucket_of(meta.sample_rate, buckets_per_sec);
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
            thread::Builder::new().name("waveform".into()).spawn(
                move || -> crate::Result<Vec<(f32, f32)>> {
                    match open_window(&path, stream, start, end)? {
                        Some((_, rx)) => Ok(fold(rx, per_bucket, channels, u64::MAX)),
                        None => Ok(Vec::new()),
                    }
                },
            )
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
    ((secs / PER_JOB_SECS) as usize)
        .min(cores)
        .min(CEILING)
        .max(1)
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

    use super::{
        memo_decodes, memoize, peak_of, peaks, peaks_over, peaks_shared_capped, viz_decodes,
        viz_envelope, viz_envelope_capped, viz_folded, MemoEntry, PeaksKey, PeaksMemo, SharedPeaks,
        PEAKS_MEMO_BUCKETS,
    };

    const BPS: u32 = 10;

    /// The memo's own ask, as the seat's used to be: a ceiling a test does not
    /// want to think about is the process's.
    fn peaks_shared(
        path: &std::path::Path,
        stream: usize,
        buckets_per_sec: u32,
    ) -> crate::Result<Option<SharedPeaks>> {
        peaks_shared_capped(path, stream, buckets_per_sec, PEAKS_MEMO_BUCKETS)
    }

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    /// A private copy of a fixture, so the memo key a test drives is not one
    /// another test in this binary may already hold.
    fn copy_of(name: &str, tag: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("edith-peaks-{tag}-{}.mp4", std::process::id()));
        std::fs::write(&path, std::fs::read(asset(name)).expect("read the fixture"))
            .expect("write the copy");
        path
    }

    /// One test at a time, for the whole module.
    ///
    /// Three of the seams these tests read -- [`MEMO_DECODES`], [`VIZ_DECODES`]
    /// and [`VIZ_FOLDED`] -- are process-wide, so "the second ask decoded
    /// nothing" is a claim about every test in this binary at once. Without this
    /// lock it holds only when the suite is run the way it is documented to be
    /// run (`--test-threads=1`); under `cargo test`'s own parallel default a
    /// pass belonging to a test running beside this one lands inside the delta,
    /// and the claim fails for a reason that has nothing to do with the code
    /// ([`a_summary_pass_folds_the_file_once`] and the parent revision's
    /// [`an_envelope_over_the_ceiling_is_not_held`] are both red that way).
    ///
    /// Every test in the module takes it, not only the readers: a test that
    /// *decodes* moves the counters the readers are measuring. The cost is that
    /// this module's tests do not run in parallel with each other -- they are the
    /// fixture-heavy ones, and the suite runs them serially anyway. Keying each
    /// counter by the file it was for would buy the parallelism back, and is the
    /// upgrade path if this lock ever shows up in a profile.
    fn counters_serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A poisoned lock is another test's panic, not this one's failure: the
        // counters are still readable and mutual exclusion is the whole point.
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn peaks_follow_the_1hz_volume_pulse() {
        let _serial = counters_serial();
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
        let _serial = counters_serial();
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
        let _serial = counters_serial();
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
        let _serial = counters_serial();
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
        let _serial = counters_serial();
        assert!(peaks(asset("test_baseline.mp4"), 0, BPS)
            .expect("open")
            .is_none());
    }

    /// Two streams of one file are two envelopes: the lane draws what the clip
    /// actually plays, so a cache keyed on the path alone would be a lie the
    /// user can see. Stream 1 of the fixture is 2 s of 220 Hz mono against
    /// stream 0's 4-second-long pulsed stereo pair.
    #[test]
    fn each_stream_of_a_file_has_its_own_envelope() {
        let _serial = counters_serial();
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
        let two = peaks(&multi, 2, BPS)
            .expect("AC-3 opens")
            .expect("stream 2");
        assert!(!two.is_empty(), "the AC-3 stream drew nothing");
    }

    /// The memo is keyed on the *ask*, not on the file: a second visualizer
    /// rebuild of one source reuses the envelope, and one of another stream is
    /// decoded rather than handed the first one's -- the lie
    /// [`each_stream_of_a_file_has_its_own_envelope`] refuses, one cache layer
    /// down, where the visualizer seat reads it.
    #[test]
    fn the_shared_memo_answers_per_stream() {
        let _serial = counters_serial();
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
        let _serial = counters_serial();
        let path =
            std::env::temp_dir().join(format!("edith-peaks-memo-{}.mp4", std::process::id()));
        let short = std::fs::read(asset("test_multiaudio.mp4")).expect("read the fixture");
        let long = std::fs::read(asset("test_av.mp4")).expect("read the fixture");
        // The two halves below are the length half and the clock half of the
        // key, so their sizes have to differ for the first of them to be it.
        assert_ne!(short.len(), long.len(), "fixtures must differ in length");

        std::fs::write(&path, &short).expect("write the copy");
        let first = peaks_shared(&path, 0, BPS).expect("open").expect("copy");
        let again = peaks_shared(&path, 0, BPS).expect("open").expect("copy");
        assert!(
            std::sync::Arc::ptr_eq(&first, &again),
            "the copy was not memoized"
        );

        // Other bytes behind the same path. A key without the file's length
        // (or without any stat at all) serves `first` here -- the envelope of
        // bytes that are gone.
        std::fs::write(&path, &long).expect("rewrite the copy");
        let replaced = peaks_shared(&path, 0, BPS).expect("open").expect("copy");
        assert_ne!(
            *replaced, *first,
            "the memo served the replaced file's envelope"
        );
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

    /// A trackless source is an answer too, and the memo keeps it: without that
    /// a visualizer clip on a silent source re-opens the container on every
    /// picture rebuild -- the whole cost the memo exists to remove, paid once
    /// per look edit. Proved by counting decodes: the second ask must not take
    /// one. A copy is used so the key is this test's alone.
    #[test]
    fn a_trackless_source_is_memoized_too() {
        let _serial = counters_serial();
        let path = copy_of("test_baseline.mp4", "none");

        let before = memo_decodes(&path, 0, BPS);
        let first = peaks_shared(&path, 0, BPS).expect("open");
        assert!(first.is_none(), "test_baseline.mp4 has no audio track");
        let after_first = memo_decodes(&path, 0, BPS);
        assert_eq!(after_first - before, 1, "the first ask did not decode");

        let second = peaks_shared(&path, 0, BPS).expect("open");
        assert!(second.is_none());
        assert_eq!(
            memo_decodes(&path, 0, BPS),
            after_first,
            "the second ask re-opened a trackless source"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A trackless answer holds no buckets, so the ceiling counts only
    /// envelopes -- and a source that gains an audio track (a re-encode) swaps
    /// the `NoTrack` entry for an envelope without stranding the old count.
    #[test]
    fn a_trackless_answer_holds_no_buckets() {
        let _serial = counters_serial();
        let mut memo = PeaksMemo::new();
        let mut count = 0;
        let key = || PeaksKey::of(std::path::Path::new("edith-peaks-none-key"), 0, BPS);

        memoize(&mut memo, &mut count, 100, key(), MemoEntry::NoTrack);
        assert_eq!(count, 0, "a trackless answer counted buckets");
        assert!(memo.contains_key(&key()));

        memoize(
            &mut memo,
            &mut count,
            100,
            key(),
            MemoEntry::Envelope(SharedPeaks::new(vec![(0.0, 0.0); 4])),
        );
        assert_eq!(count, 4, "the envelope did not replace the trackless count");
    }

    /// An envelope larger than the ceiling is handed to the caller but never
    /// held, so a clear can always bring the memo back under the ceiling. The
    /// real ceiling is 8M buckets (~35 min at 10/s), unreachable from a
    /// fixture, so this drives the same code through a ceiling of ten buckets
    /// against a five-second source (about fifty buckets). Covers: the
    /// over-the-ceiling *decision* and the caller-still-gets-its-handle half.
    /// Does not cover: an end-to-end decode past eight million buckets.
    #[test]
    fn an_envelope_over_the_ceiling_is_not_held() {
        let _serial = counters_serial();
        let file = copy_of("test_av.mp4", "over");
        let before = memo_decodes(&file, 0, BPS);
        let first = peaks_shared_capped(&file, 0, BPS, 10)
            .expect("open")
            .expect("test_av.mp4 has an audio track");
        assert!(first.len() > 10, "the fixture must exceed the test ceiling");
        let mid = memo_decodes(&file, 0, BPS);
        assert_eq!(mid - before, 1, "the first ask did not decode");

        let second = peaks_shared_capped(&file, 0, BPS, 10)
            .expect("open")
            .expect("test_av.mp4 has an audio track");
        assert_eq!(*first, *second, "the caller did not get its envelope");
        assert_eq!(
            memo_decodes(&file, 0, BPS) - mid,
            1,
            "an envelope over the ceiling was held"
        );

        let _ = std::fs::remove_file(&file);
    }

    /// An insert that replaces an entry the counter already includes subtracts
    /// the replaced length: the racing-workers case, where two workers that
    /// missed one key each decode and insert it. The count follows what the map
    /// *holds*, so a missing subtraction leaves it high for good -- only a
    /// clear resets it, and later asks then clear early, dropping entries that
    /// pay a whole-file decode to be rebuilt. Driven through the same [`memoize`]
    /// the ask path uses (a second *ask* on one key hits the memo, so it cannot
    /// reach the insert): without the subtraction the second insert would read
    /// 3 + 5 = 8.
    #[test]
    fn replacing_an_entry_subtracts_the_old_buckets() {
        let _serial = counters_serial();
        let mut memo = PeaksMemo::new();
        let mut count = 0;
        let key = || PeaksKey::of(std::path::Path::new("edith-peaks-replace-key"), 0, BPS);

        memoize(
            &mut memo,
            &mut count,
            100,
            key(),
            MemoEntry::Envelope(SharedPeaks::new(vec![(0.0, 0.0); 3])),
        );
        assert_eq!(count, 3, "the first insert was not counted");

        memoize(
            &mut memo,
            &mut count,
            100,
            key(),
            MemoEntry::Envelope(SharedPeaks::new(vec![(0.0, 0.0); 5])),
        );
        assert_eq!(count, 5, "the replaced entry's buckets were not subtracted");
        assert_eq!(memo.len(), 1, "the replace added a second entry");
    }

    /// A window is where it says it is: on a fixture whose envelope is *not*
    /// periodic, so a slice labelled a margin below where it starts cannot hide.
    /// A sine hides it -- a shift by a whole number of periods leaves every
    /// bucket identical, which is exactly how a +3 s mislabel survived a 440 Hz
    /// fixture at 8 kHz (3 s is 1320 whole periods) -- so this is noise, in
    /// FLAC (a seek lands on the sample, so the buckets must be *equal*, not
    /// merely close).
    ///
    /// Every place a window can go wrong: the file's head, its middle, both
    /// sides of a 30 s grid line, and the last window it has. At each: every
    /// bucket a two-second picture can reach is the envelope's, and the frame is
    /// byte for byte the one the *export* path paints from the whole envelope
    /// ([`crate::decode::visualizer_i420`]).
    #[test]
    fn a_window_of_a_non_periodic_file_is_where_it_says_it_is() {
        let _serial = counters_serial();
        let path =
            std::env::temp_dir().join(format!("edith-peaks-noise-{}.flac", std::process::id()));
        let generated = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg("anoisesrc=color=white:amplitude=0.5:sample_rate=8000:duration=120")
            .args(["-c:a", "flac"])
            .arg(&path)
            .status()
            .map(|status| status.success() && path.exists())
            .unwrap_or(false);
        if !generated {
            eprintln!("no ffmpeg: a non-periodic fixture cannot be generated, skipping");
            return;
        }
        let bps = crate::decode::VIZ_BUCKETS_PER_SEC;
        let per_sec = f64::from(bps);
        let plain = peaks(&path, 0, bps)
            .expect("open")
            .expect("the noise is audio");
        assert!(
            plain.len().abs_diff(120 * bps as usize) < 100,
            "the envelope is {} buckets, not the 120 s it was generated for",
            plain.len()
        );
        let peak = peak_of(&plain);
        let mut scratch = crate::decode::VizScratch::default();
        for secs in [0.0, 15.0, 29.5, 30.5, 60.0, 89.5, 119.0] {
            // A ceiling of a thousand buckets, so the windowed route is the one
            // walked (the route is what the real ceiling takes for a film).
            let mut seat = viz_envelope_capped(&path, 0, bps, 1000).expect("open");
            let view = seat.view_at(secs);
            assert!(
                !view.peaks.is_empty(),
                "t={secs}: the window decoded nothing"
            );
            assert_eq!(
                view.peak, peak,
                "t={secs}: the window's peak is not the file's"
            );
            assert_eq!(
                view.total,
                plain.len(),
                "t={secs}: the length is not the file's"
            );
            // The buckets the frame at `secs` can reach: a second either way,
            // plus the bucket the interpolation reads past the end.
            let from = ((secs - 1.0).max(0.0) * per_sec) as usize;
            let to = (((secs + 1.0) * per_sec) as usize + 2).min(plain.len());
            let (mut compared, mut wrong, mut out_of_reach) = (0, 0, 0);
            for abs in from..to {
                match (view.at(abs), plain.get(abs)) {
                    (Some(window), Some(&whole)) => {
                        compared += 1;
                        if window != whole {
                            wrong += 1;
                        }
                    }
                    _ => out_of_reach += 1,
                }
            }
            assert!(
                compared > 1000,
                "t={secs}: only {compared} buckets were comparable ({out_of_reach} out of reach)"
            );
            assert_eq!(
                (wrong, out_of_reach),
                (0, 0),
                "t={secs}: {wrong} of {compared} windowed buckets are not the envelope's, \
                 {out_of_reach} out of reach"
            );
            let whole = crate::decode::visualizer_i420(&mut scratch, &plain, secs, 320, 180, 0, 5);
            let window =
                crate::decode::visualizer_window_i420(&mut scratch, &view, secs, 320, 180, 0, 5);
            assert_eq!(
                whole, window,
                "t={secs}: the window painted a frame the whole envelope does not"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    /// The summary pass reads the file *once*. Its windows partition it -- the
    /// first one stops at the joint the next was told to start at -- so the
    /// frames it folds are the file's own, and folding the ask's whole-file
    /// probe on top of them (which is what it used to do) is up to twice a file
    /// for nothing, on the critical path of the first look edit.
    #[test]
    fn a_summary_pass_folds_the_file_once() {
        let _serial = counters_serial();
        let path =
            std::env::temp_dir().join(format!("edith-peaks-once-{}.flac", std::process::id()));
        let generated = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg("anoisesrc=color=white:amplitude=0.5:sample_rate=8000:duration=120")
            .args(["-c:a", "flac"])
            .arg(&path)
            .status()
            .map(|status| status.success() && path.exists())
            .unwrap_or(false);
        if !generated {
            eprintln!("no ffmpeg: a film-length fixture cannot be generated, skipping");
            return;
        }
        let bps = crate::decode::VIZ_BUCKETS_PER_SEC;
        let frames = 120 * 8000;
        let before = viz_folded();
        let mut seat = viz_envelope_capped(&path, 0, bps, 1000).expect("open");
        let view = seat.view_at(60.0);
        assert!(!view.peaks.is_empty(), "the window decoded nothing");
        let folded = viz_folded() - before;
        assert!(
            folded >= frames - frames / 10,
            "the summary pass folded {folded} frames of a {frames}-frame file: it lost a window"
        );
        assert!(
            folded <= frames + frames / 20,
            "the summary pass folded {folded} frames of a {frames}-frame file, {:.2}x over",
            folded as f64 / frames as f64
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A container that states no sample rate (or a zero one) must not make the
    /// samples-per-bucket arithmetic zero: `sample / 0.0` is infinite, `as
    /// usize` saturates, and the fold's own `resize(bucket + 1)` is then a
    /// length that does not exist -- a panic in debug, a wrap and an
    /// out-of-bounds index in release. Both asks that bucket samples share the
    /// guard, so this is the one arithmetic they both use.
    #[test]
    fn a_zero_sample_rate_still_buckets_samples() {
        let _serial = counters_serial();
        // A rate of zero is read as one, and the rate is what divides either
        // way, so the arithmetic stays finite and positive.
        assert_eq!(super::per_bucket_of(0, 4000), 1.0 / 4000.0);
        assert_eq!(super::per_bucket_of(44100, 0), 44100.0);
        // ...and a fold over a chunk at that rate is total: one bucket per
        // sample rather than an index that does not exist.
        let per_bucket = super::per_bucket_of(0, 1);
        assert_eq!(per_bucket, 1.0);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(crate::audio::AudioChunk {
            start_sample: 0,
            samples: vec![0.5; 4],
        })
        .expect("send");
        drop(tx);
        let peaks = super::fold(rx, per_bucket, 1, u64::MAX);
        assert_eq!(peaks.len(), 4, "the fold lost or invented buckets");
        assert_eq!(peaks[0], (0.0, 0.5), "a bucket must still straddle zero");
    }

    /// The visualizer's ask for a source past the memo's ceiling: the first ask
    /// pays one pass for the file's two scalars and one window of it, and every
    /// ask after that -- the ones a look drag makes, once per pointer sample at
    /// the same playhead -- decodes nothing at all. Driven with a ceiling of ten
    /// buckets against a five-second source (fifty buckets), which is the same
    /// route the eight-million-bucket ceiling takes for a film.
    #[test]
    fn a_window_of_an_over_the_ceiling_source_is_decoded_once() {
        let _serial = counters_serial();
        let file = copy_of("test_av.mp4", "window");
        let plain = peaks(&file, 0, BPS).expect("open").expect("audio track");

        let before = viz_decodes();
        let mut first = viz_envelope_capped(&file, 0, BPS, 10).expect("open");
        assert!(
            matches!(first, super::VizEnvelope::Windowed(_)),
            "the fixture must be past the test ceiling"
        );
        let view = first.view_at(2.5);
        assert!(!view.peaks.is_empty(), "the window decoded nothing");
        // The two scalars are the file's own and not an approximation of them:
        // the painter's whole-file peak and its "past the end of the file" both
        // come from here, and a frame scaled by anything else is another frame.
        assert_eq!(
            view.peak,
            peak_of(&plain),
            "the window's peak is not the file's"
        );
        assert_eq!(
            view.total,
            plain.len(),
            "the windowed length is not the envelope's"
        );
        let after_first = viz_decodes();
        assert!(after_first > before, "the first ask decoded nothing");

        // The same second, three more times, the way a drag asks it: a new seat
        // every time, and no decode in any of them.
        for ask in 0..3 {
            let mut again = viz_envelope_capped(&file, 0, BPS, 10).expect("open");
            let view = again.view_at(2.5);
            assert_eq!(view.total, plain.len());
            assert!(
                viz_decodes() == after_first,
                "ask {} re-decoded a window the memo holds",
                ask + 2
            );
        }
        let _ = std::fs::remove_file(&file);
    }

    /// A window paints the frame the whole envelope paints: the picture of a
    /// clip must not change because the seat stopped holding two hours of sound
    /// to draw thirty seconds of it. The window here is cut out of the envelope
    /// itself, so this is the painter's half of the promise -- both looks, and
    /// times from the file's first frame to past its last.
    #[test]
    fn a_window_paints_the_frame_the_whole_envelope_paints() {
        let _serial = counters_serial();
        let file = asset("test_av.mp4");
        // The painter's own rate: a view carries the rate its envelope
        // was drawn at and indexes the slice by it, so this envelope is
        // drawn at exactly the rate the painter reads it at -- the
        // visualizer's own ([`crate::decode::VIZ_BUCKETS_PER_SEC`]).
        let per_sec = f64::from(crate::decode::VIZ_BUCKETS_PER_SEC);
        let whole = peaks(&file, 0, crate::decode::VIZ_BUCKETS_PER_SEC)
            .expect("open")
            .expect("audio track");
        let peak = peak_of(&whole);
        let mut scratch = crate::decode::VizScratch::default();
        for flags in [0u8, crate::decode::VIZ_FAST] {
            for secs in [0.0, 0.4, 1.0, 2.5, 4.9, 5.2] {
                let whole_frame =
                    crate::decode::visualizer_i420(&mut scratch, &whole, secs, 320, 180, flags, 7);
                // The buckets a two-second picture can reach: a second either
                // side of the frame's own time, plus the bucket the
                // interpolation reads past it.
                let from = ((secs - 1.0).max(0.0) * per_sec) as usize;
                let to = (((secs + 1.0) * per_sec) as usize + 2).min(whole.len());
                let view = crate::decode::VizView::window(
                    &whole[from..to],
                    from,
                    whole.len(),
                    peak,
                    per_sec,
                );
                let window_frame = crate::decode::visualizer_window_i420(
                    &mut scratch,
                    &view,
                    secs,
                    320,
                    180,
                    flags,
                    7,
                );
                assert_eq!(
                    whole_frame, window_frame,
                    "the window painted {secs}s (flags {flags}) differently"
                );
            }
        }
    }

    /// The long source the defect is about: forty minutes of sound, whose own
    /// envelope at the visualizer's 4000 buckets a second is 9.6M buckets --
    /// past [`PEAKS_MEMO_BUCKETS`], so the memo can never hold it and the
    /// windowed route is the only one there is. What a window of it draws is
    /// what the whole envelope draws, byte for byte, and the second and third
    /// ask decode nothing.
    ///
    /// The fixture is generated here (`ffmpeg -f lavfi -i
    /// sine=frequency=440:duration=2400 -ar 8000 -ac 1 -c:a flac`) and is FLAC
    /// on purpose: a seek into a lossless frame lands on the sample, so a
    /// window of it holds exactly the buckets the whole-file decode holds --
    /// which is the promise a caching change has to keep, and what a lossy
    /// codec's seeked filterbank cannot (a bucket lands a ten-thousandth away,
    /// as [`a_split_decode_draws_the_same_envelope`] already bounds it). The
    /// envelope's own length is asserted against the forty minutes asked for,
    /// so a fixture that came out at another scale cannot pass this vacuously.
    /// Skipped, loudly, where ffmpeg is not installed.
    #[test]
    fn a_film_length_source_paints_the_same_frame_in_windows() {
        let _serial = counters_serial();
        let path =
            std::env::temp_dir().join(format!("edith-peaks-long-{}.flac", std::process::id()));
        let generated = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=2400")
            .args(["-ar", "8000", "-ac", "1", "-c:a", "flac"])
            .arg(&path)
            .status()
            .map(|status| status.success() && path.exists())
            .unwrap_or(false);
        if !generated {
            eprintln!("no ffmpeg: a film-length fixture cannot be generated, skipping");
            return;
        }
        let bps = crate::decode::VIZ_BUCKETS_PER_SEC;
        let plain = peaks(&path, 0, bps)
            .expect("open")
            .expect("the tone is audio");
        let secs = 2400 * bps as usize;
        assert!(
            plain.len().abs_diff(secs) < 100,
            "the envelope is {} buckets, not the 2400 s of sound it was generated for",
            plain.len()
        );
        assert!(
            plain.len() > PEAKS_MEMO_BUCKETS,
            "{} buckets is not past the memo's ceiling",
            plain.len()
        );

        let before = viz_decodes();
        let mut seat = viz_envelope(&path, 0, bps).expect("open");
        let t = 1200.0;
        let view = seat.view_at(t);
        assert!(!view.peaks.is_empty(), "the window decoded nothing");
        assert_eq!(
            view.peak,
            peak_of(&plain),
            "the window's peak is not the file's"
        );
        assert_eq!(
            view.total,
            plain.len(),
            "the windowed length is not the envelope's"
        );
        // The window's own buckets are the envelope's, bucket for bucket: the
        // time the seats ask for in the middle of a film is the time a window
        // has to get exactly as right as two hours of decode did.
        let mut checked = 0;
        for abs in (t * f64::from(bps)) as usize..((t + 20.0) * f64::from(bps)) as usize {
            let (Some(window), Some(&whole)) = (view.at(abs), plain.get(abs)) else {
                continue;
            };
            assert_eq!(window, whole, "bucket {abs} differs from the envelope's");
            checked += 1;
        }
        assert!(checked > 10_000, "only {checked} buckets were compared");

        let mut scratch = crate::decode::VizScratch::default();
        for secs in [t - 1.0, t, t + 1.0] {
            let whole = crate::decode::visualizer_i420(&mut scratch, &plain, secs, 320, 180, 0, 3);
            let window =
                crate::decode::visualizer_window_i420(&mut scratch, &view, secs, 320, 180, 0, 3);
            assert_eq!(whole, window, "the window painted {secs}s differently");
        }
        let after_first = viz_decodes();
        assert!(after_first > before, "the first ask decoded nothing");

        // A look drag on a film: a new seat per pointer sample, at the same
        // playhead, and not one decode between them.
        for ask in 0..3 {
            let mut again = viz_envelope(&path, 0, bps).expect("open");
            let view = again.view_at(t);
            assert_eq!(view.total, plain.len());
            assert!(
                viz_decodes() == after_first,
                "ask {} re-decoded a window the memo holds",
                ask + 2
            );
        }
        let _ = std::fs::remove_file(&path);
    }
}
