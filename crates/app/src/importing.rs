//! Bringing a file in: the library's tabs, the import and scan state, the stages.

use crate::*;

/// The library's three categories, in the order the giants list them: the
/// pictures, the sound, and the words. A file is in exactly one of them, so a
/// tab is a question with an answer rather than a filter with a guess.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LibraryTab {
    Media,
    Audio,
    Text,
}

pub(crate) const LIBRARY_TABS: [LibraryTab; 3] =
    [LibraryTab::Media, LibraryTab::Audio, LibraryTab::Text];

impl LibraryTab {
    pub(crate) fn label(self) -> &'static str {
        match self {
            LibraryTab::Media => "Media",
            LibraryTab::Audio => "Audio",
            LibraryTab::Text => "Text",
        }
    }

    /// Whether a source belongs on this tab. Subtitles are not sources at all
    /// -- they are the [`Player::subtitle_section`] under the list -- so the
    /// Text tab holds no rows of its own and says so.
    pub(crate) fn holds(self, path: &Path) -> bool {
        match self {
            LibraryTab::Media => !engine::is_audio(path),
            LibraryTab::Audio => engine::is_audio(path),
            LibraryTab::Text => false,
        }
    }

    /// What an empty tab says instead of being a blank column.
    pub(crate) fn empty(self) -> &'static str {
        match self {
            LibraryTab::Media => "No video or stills yet — Import, or drop a file on the window",
            LibraryTab::Audio => "No sound yet — Import, or drop a file on the window",
            LibraryTab::Text => {
                "No subtitles yet — Add subtitles from a file, or drop an .srt on the window"
            }
        }
    }
}

/// What is known about a source's audio. Three states and not two, because a
/// file whose peaks have not come back yet must not be drawn as one that has no
/// audio at all: the first shows a bed, the second shows nothing.
#[derive(Clone)]
pub(crate) enum Wave {
    /// Asked for; the decode is running on a background thread.
    Loading,
    /// The file has no audio track. An answer, not a miss.
    Silent,
    /// The decode failed. Drawn as its own mark rather than as [`Self::Silent`]:
    /// "this file's sound could not be read" and "this file has no sound" look
    /// the same on a lane, and the first is a bug report waiting to happen.
    Failed,
    Peaks(Arc<Vec<(f32, f32)>>),
}

/// The bench's own version of [`Wave`] for the picture instead of the sound: one
/// representative decoded frame per file, kept and reused across every clip cut
/// from it (DESIGN.md §5's "real thumbnails across video clips", §12 step 5 --
/// this is the frame, not the ink; the ink comes from [`crate::source_tint`]
/// until real extraction lands). Audio-only files never enter this map (see
/// `cache_media`'s filter), so `Silent` has no counterpart here.
#[derive(Clone)]
pub(crate) enum Thumb {
    /// Asked for; the decode is running on a background thread.
    Loading,
    /// The decode failed or the file turned out to carry no picture after all.
    Failed,
    Ready(Arc<gpui::RenderImage>),
}

/// What a source's stand-in is up to ([`engine::proxy`]). Kept per file and
/// filled like every other library fact -- presence means "asked", so a repaint
/// mid-encode cannot start a second one.
pub(crate) enum Proxy {
    /// The header is being read to find out whether this file wants one. Its
    /// own state because it is what makes "how many are in flight" a number:
    /// the answer comes back on a worker, and until it does this file may still
    /// turn into an encode.
    Asked,
    /// A file this machine cuts at speed as it is: no stand-in, and none
    /// wanted ([`engine::proxy::wanted`]).
    Native,
    /// Being made, in the background, while the film keeps playing.
    Making(engine::proxy::Job),
    /// Stopped by hand ([`Player::cancel_proxy`]) and not yet gone: the worker
    /// gives up at its next frame and deletes the half-written file itself, and
    /// the job is held until it does. Kept as a state rather than dropped
    /// because the encode may still *beat* the cancel -- a stand-in that is
    /// already written is a file in the cache and is reported as one.
    Cancelling(engine::proxy::Job),
    /// Stopped, and the worker has gone. Not [`Self::Failed`]: nothing went
    /// wrong, somebody asked for it -- and it stays in the map so no repaint
    /// starts the encode that was just stopped over again.
    Cancelled,
    /// There is one in the cache. What the switch actually plays.
    Ready,
    /// It could not be made: the film itself is what plays, and the row says
    /// so rather than leaving a bar that never fills.
    Failed,
}

/// The import a worker is reading, as the line above the panel shows it. No
/// fraction anywhere: neither read reports how far into the file it has come,
/// so what is honest is the file's name, the stage, and two clocks -- one that
/// proves the window is answering, one that says the stage has not moved.
pub(crate) struct Import {
    pub(crate) path: PathBuf,
    /// When the worker started, for the elapsed clock.
    pub(crate) started: Instant,
    /// Written by the worker between its two reads, read here every repaint.
    pub(crate) stage: Arc<std::sync::atomic::AtomicU8>,
    /// The stage the line last saw and when it last changed. The pair is the
    /// whole stall detector: a stage older than [`IMPORT_STALL`] is what the
    /// honest wording is for.
    pub(crate) seen: ImportStage,
    pub(crate) since: Instant,
    /// Set by the Cancel beside the line ([`Player::cancel_import`]), read at
    /// the landing: what the worker read is dropped instead of joining the
    /// timeline.
    ///
    /// ...and the read with it: the worker installs this as its thread's cancel
    /// ([`engine::demux::with_cancel`]) and every Matroska cluster walk under it
    /// -- the header index and the subtitle pass, the twenty seconds of a cold
    /// 24 GB remux -- gives up at the next element.
    ///
    /// corner-cut: an mp4's header is one read inside `mp4::Mp4Reader`, which
    /// has no flag to poll, so a cancelled mp4 import still finishes its own
    /// header before the worker returns. Ceiling: the disk stays busy for that
    /// read, though nothing waits on it. Upgrade path is our own `moov` parse,
    /// which is a demuxer rewrite rather than a cancel.
    pub(crate) cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl Import {
    /// Reads the worker's stage and keeps the stall clock: it restarts when the
    /// stage actually changes and at no other time, which is what makes "has
    /// not moved in five seconds" a fact rather than a guess about the elapsed
    /// clock. Hands back how long the current stage has stood, so the line has
    /// one place to ask.
    pub(crate) fn poll(&mut self) -> f32 {
        let stage = ImportStage::from_u8(self.stage.load(std::sync::atomic::Ordering::Relaxed));
        if stage != self.seen {
            self.seen = stage;
            self.since = Instant::now();
        }
        self.since.elapsed().as_secs_f32()
    }
}

/// What a scan is *of*: the source, which of its audio streams, and the clip's
/// own `[in, out)` source frames. The range is part of the name because it is
/// part of the read -- a clip cut in half is a different, shorter decode than
/// the whole take was, and levels read for one are not the other's.
pub(crate) type ScanKey = (PathBuf, usize, u32, u32);

/// The silence scan a worker is running, as the card shows it. Same two clocks
/// as an [`Import`] and for the same reason -- one proves the window answers,
/// one says the read has stopped moving -- over a progress that *can* move:
/// a decode knows how far into the sound it has come, so the card says so.
pub(crate) struct SilenceScan {
    /// Source, stream and source range being scanned, which is the cache key
    /// the levels land under and what tells a second open of the same clip from
    /// a new one.
    pub(crate) key: ScanKey,
    /// When the worker started, for the elapsed clock.
    pub(crate) started: Instant,
    /// Written by the worker, read here every repaint. The cancel flag in it is
    /// this side's only word to a scan already running.
    pub(crate) progress: Arc<engine::silence::Progress>,
    /// The tenths-of-a-second mark the line last saw and when it last changed:
    /// the stall detector, exactly [`Import::poll`]'s.
    pub(crate) seen: u64,
    pub(crate) since: Instant,
}

impl SilenceScan {
    /// Reads the worker's mark and keeps the stall clock, restarting it only
    /// when the mark actually moves -- [`Import::poll`]'s contract, over a
    /// number instead of a stage.
    pub(crate) fn poll(&mut self) -> f32 {
        let scanned = self
            .progress
            .scanned
            .load(std::sync::atomic::Ordering::Relaxed);
        if scanned != self.seen {
            self.seen = scanned;
            self.since = Instant::now();
        }
        self.since.elapsed().as_secs_f32()
    }
}

/// How long a stage may sit without moving before the import line stops
/// reading like a progress line and says outright that the wait is the file's,
/// not the window's. Five seconds: a 25 GB remux spends eleven in its header
/// alone, and a person watching a still line for that long has already decided
/// the editor hung.
pub(crate) const IMPORT_STALL: f32 = 5.;

/// Which of an import's two reads the worker is inside. Travels as one atomic,
/// the way an export's progress does ([`engine::ExportHandle`]) -- there is no
/// fraction to send, because neither read reports where in the file it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ImportStage {
    /// The container header: the sample tables, the cue index, the frame count.
    /// Eleven seconds on a cold 29 GB remux, a hundred and fifty milliseconds
    /// once the pages are warm -- the whole reason this runs off the UI thread.
    Header,
    /// The subtitle tracks inside a Matroska, which is a walk over the file's
    /// blocks rather than a header read (~200 ms on a two-hour film).
    Subtitles,
}

impl ImportStage {
    /// The atomic the worker writes; `u8` because that is what an
    /// [`AtomicU8`](std::sync::atomic::AtomicU8) carries.
    pub(crate) fn from_u8(n: u8) -> Self {
        match n {
            0 => Self::Header,
            _ => Self::Subtitles,
        }
    }

    /// What the line calls this stage.
    pub(crate) fn what(self) -> &'static str {
        match self {
            Self::Header => "reading the header",
            Self::Subtitles => "reading the subtitle tracks",
        }
    }
}

/// argv sorted into the file that becomes the timeline and the queue every
/// named file is read through, in the order they were named. The whole of what
/// a launch does before the window is on screen -- and it touches no disk,
/// which is the point: the header walk happens on a worker with the window
/// already up ([`Player::take_import`]).
pub(crate) fn launch_queue(
    args: impl Iterator<Item = PathBuf>,
) -> (Option<PathBuf>, std::collections::VecDeque<PathBuf>) {
    let queue: std::collections::VecDeque<PathBuf> = args.collect();
    (queue.front().cloned(), queue)
}

/// What a queued file turns into. Every file goes through one queue -- argv's
/// first file, argv's extras, a drop, the Import button -- and this is the fork
/// its worker is started on.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub(crate) enum Landing {
    /// The media argv named: it becomes the timeline, and the canvas, the fps,
    /// the title and the export path come from it.
    Open,
    /// A `.edith` argv named: a whole timeline restored, not a source.
    Project,
    /// Everything else: a row in the library, the timeline untouched.
    Import,
}

/// Which of the three a queued file is (`landing` above is the drag's).
/// `opening` is the one path argv named and is cleared as it lands, so a second
/// arrival of the same *media* path -- a drop of the film that is already open
/// -- is an import, which is what a drop has always been.
///
/// A `.edith` is never an import, whichever door it came through: it is a whole
/// timeline and there is nothing to add it to. Argv's, a dropped one and the
/// Import button's are one landing, so the seconds its open costs are the
/// worker's for all three ([`open_ahead`]) and the line above the panel names it
/// while it runs.
pub(crate) fn arrival(opening: Option<&std::path::Path>, path: &std::path::Path) -> Landing {
    match (is_project(path), opening == Some(path)) {
        (true, _) => Landing::Project,
        (false, true) => Landing::Open,
        (false, false) => Landing::Import,
    }
}

/// The whole of what an import shows while it runs: which file, which stage,
/// the clock that proves the window is still answering, and what is behind it
/// in the queue.
///
/// `since` is how long the *stage* has stood still, which is the only movement
/// an unmeasurable read has: past [`IMPORT_STALL`] the line says so in words,
/// because a bar that cannot move and a bar that has stopped look identical.
///
/// `opening` is the file argv named, which is read through the same queue and
/// says so in the same words -- except that it is being *opened*, and a line
/// that called it an import would be describing the wrong thing to the one
/// person who typed the name.
pub(crate) fn import_line(
    name: &str,
    stage: ImportStage,
    elapsed: f32,
    since: f32,
    waiting: usize,
    opening: bool,
) -> String {
    let tail = match waiting {
        0 => String::new(),
        n => format!(" · {n} more waiting"),
    };
    let what = stage.what();
    let verb = match opening {
        true => "OPENING",
        false => "IMPORTING",
    };
    match since >= IMPORT_STALL {
        true => format!(
            "{verb} {name} · still {what} — a big file is minutes of reading, and the window is \
             not frozen · {} elapsed{tail}",
            clock(elapsed)
        ),
        false => format!("{verb} {name} · {what} · {} elapsed{tail}", clock(elapsed)),
    }
}

/// What opening the silence card on a source costs.
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum ScanPlan {
    /// Its levels are already read: the marks are arithmetic on this frame.
    Marks,
    /// Nothing read: a worker, and a card that says so meanwhile.
    Start,
    /// A worker is already reading this very source -- the other half of a take
    /// names the same file, and a second card on it waits for the first read
    /// rather than throwing away the minute already spent.
    Wait,
}

/// Which of the three [`ScanPlan`]s opening the card on `key` means. The whole
/// of the cache policy, and the reason a second film does not cost the first
/// one its levels: what is cached is asked per source, never "the last one".
pub(crate) fn scan_plan(cached: bool, running: Option<&ScanKey>, key: &ScanKey) -> ScanPlan {
    match (cached, running) {
        (true, _) => ScanPlan::Marks,
        (false, Some(at)) if at == key => ScanPlan::Wait,
        _ => ScanPlan::Start,
    }
}

/// The half-open source seconds a [`ScanKey`]'s frames name, at the project's
/// rate -- what [`engine::silence::levels`] is asked to read, and the same
/// arithmetic playback puts a clip's `in_frame` through. A rate that is not a
/// rate reads the whole file rather than an empty window: a scan of nothing is
/// worse than a scan of too much.
pub(crate) fn source_secs(key: &ScanKey, fps: f64) -> (f64, f64) {
    match fps.is_finite() && fps > 0. {
        true => (f64::from(key.2) / fps, f64::from(key.3) / fps),
        false => (0., f64::INFINITY),
    }
}

/// The [`ScanKey`] a background scan lands its whole-file levels under: every
/// frame the source has, so one entry answers for every clip cut from it. A
/// sentinel `out_frame` rather than a fourth map, because [`ScanKey`] is
/// already what [`Player::silence_levels`] is keyed by -- a clip can never
/// itself claim `u32::MAX` as its own `out_frame`, so the two never collide.
pub(crate) fn full_scan_key(path: &Path, stream: usize) -> ScanKey {
    (path.to_path_buf(), stream, 0, u32::MAX)
}

/// The clip's own stretch of a whole-source scan, in the same dBFS windows
/// [`engine::silence::levels`] would have decoded for it directly: window `k`
/// of both is the same [`engine::silence::WINDOWS_PER_SEC`]th of a second from
/// the start of the file, so slicing costs no decode and reads the same
/// numbers the on-demand scan would have. Empty past the end of what the
/// background read has landed so far -- a scan still running answers with
/// what it has, same as [`SilenceScan`]'s own cancel does.
pub(crate) fn slice_whole_levels(
    whole: &[f32],
    fps: f64,
    in_frame: u32,
    out_frame: u32,
) -> Vec<f32> {
    if !(fps.is_finite() && fps > 0.) {
        return Vec::new();
    }
    let window = f64::from(engine::silence::WINDOWS_PER_SEC);
    let start = ((f64::from(in_frame) / fps) * window) as usize;
    let end = (((f64::from(out_frame) / fps) * window).ceil() as usize).min(whole.len());
    match start < end {
        true => whole[start..end].to_vec(),
        false => Vec::new(),
    }
}

/// Whether the card may draw `key`'s marks as arithmetic rather than start a
/// worker for it: its own exact read, or -- new since the scan moved to
/// import -- the whole-source background scan its file is cut from
/// ([`Player::cache_media`]). One function so `Player::open_silence`'s plan
/// and `Player::scan_silences`' lookup share the one contract for what
/// "cached" means, instead of two copies that could drift.
pub(crate) fn silence_cached(
    levels: &std::collections::HashMap<ScanKey, Arc<Vec<f32>>>,
    key: &ScanKey,
) -> bool {
    levels.contains_key(key) || levels.contains_key(&full_scan_key(&key.0, key.1))
}

/// What the silence card says while its worker reads. Unlike an import this
/// one *has* a fraction -- a decode knows how far into the sound it has come --
/// so the line is where it is up to, out of what the header claims the track is
/// (`total` of 0 for a header that does not say, drawn as nothing rather than
/// as a guess). Both in seconds.
///
/// The stall clock is [`IMPORT_STALL`]'s, for its reason: past five seconds
/// without the mark moving, a line that cannot move and a line that has stopped
/// look identical, and only one of them is worth words.
pub(crate) fn silence_line(scanned: f32, total: f32, elapsed: f32, since: f32) -> String {
    let far = match total > 0. {
        true => format!("{} of ~{} scanned", clock(scanned), clock(total)),
        false => format!("{} scanned", clock(scanned)),
    };
    match since >= IMPORT_STALL {
        true => format!(
            "SCANNING · still reading the sound — a big film is minutes of decoding, and the \
             window is not frozen · {far} · {} elapsed",
            clock(elapsed)
        ),
        false => format!("SCANNING · {far} · {} elapsed", clock(elapsed)),
    }
}

/// How often a progress mark is worth keeping, how far back the rate is
/// measured, and the least span that may answer at all. An export crosses
/// hardware and software segments that run at different speeds, so the
/// estimate is a window's average and never the instant's.
pub(crate) const ETA_SAMPLE: f32 = 0.5;
pub(crate) const ETA_WINDOW: f32 = 8.;
pub(crate) const ETA_SPAN: f32 = 1.5;

/// Records where the export has got to and forgets what has fallen out of the
/// window. One mark per `ETA_SAMPLE`, a window's worth kept: a bounded list
/// whichever way the encode goes.
pub(crate) fn note_progress(marks: &mut Vec<(f32, f32)>, elapsed: f32, progress: f32) {
    if marks.last().is_none_or(|&(t, _)| elapsed - t >= ETA_SAMPLE) {
        marks.push((elapsed, progress));
    }
    while marks.len() > 2 && marks[0].0 < elapsed - ETA_WINDOW {
        marks.remove(0);
    }
}

/// Seconds left at the window's rate, or `None` while nothing measurable has
/// happened yet -- which the line says as "estimating…" rather than as a
/// number it would have to take back.
pub(crate) fn eta_secs(marks: &[(f32, f32)], elapsed: f32, progress: f32) -> Option<f32> {
    // A finished pass is not a guess: it is over.
    if progress >= 1. {
        return Some(0.);
    }
    if progress <= 0. || elapsed < ETA_SPAN {
        return None;
    }
    // Two rates, averaged. The window's follows the encode across a
    // hardware-to-software handover; the whole run's is what keeps a window
    // that is all stall from throwing the number minutes out and back. Neither
    // alone reads well: raw window rate spikes eightfold on either edge of a
    // stall, and the run average alone never notices a segment change.
    let overall = progress / elapsed;
    // Never below zero, whatever the marks say. A bar that went *backwards*
    // between two marks -- an export written again from the start after its
    // hardware encoder died -- made this negative, which cancelled the run's
    // own rate and left an answer that read "~0:00 left" over six minutes of
    // software encoding. The bar is monotone at the source now
    // (`engine::export`'s `fetch_max`), and a rate the other way is still not
    // an estimate: a window that has gone nowhere leans on the whole run's
    // rate, which is exactly what a stall does.
    let recent = marks
        .first()
        .filter(|&&(t, _)| elapsed - t >= ETA_SPAN)
        .map_or(overall, |&(t, p)| ((progress - p) / (elapsed - t)).max(0.));
    // Both rates at once can only be zero where nothing has happened at all,
    // which the guards above already answer with "estimating…" -- said in
    // arithmetic as well, because the alternative is an infinite division
    // printed as a clock.
    Some(2. * (1. - progress) / (recent + overall)).filter(|left| left.is_finite())
}
