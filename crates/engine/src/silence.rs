//! Where a clip goes quiet: RMS levels off the same read-only decode path the
//! waveform takes, and the stretches of them nobody wants to watch.
//!
//! Three steps, kept apart so only the first one costs a decode: [`levels`]
//! turns a *stretch* of a file into one loudness number per 20 ms window,
//! [`regions`] turns those numbers plus a [`Settings`] into silent stretches of
//! seconds from where that read began, and [`timeline_regions`] puts those on
//! the timeline through one clip's own range and rate. A card tweaking a
//! threshold re-runs the last two and decodes nothing.
//!
//! The stretch is the *clip's*, not the file's: a take cut in half is half a
//! decode, and the half nobody is looking at is never read (which is what a
//! person means by "scan this clip").
//!
//! Loudness in **dBFS**, which is the unit every tool that does this speaks
//! (ffmpeg's `silencedetect` noise floor, auto-editor's threshold) -- and RMS
//! rather than the peak [`crate::waveform::peaks`] draws, because a single
//! sample spike in a breath is not speech and a peak meter cannot tell them
//! apart.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::AudioSession;
use crate::project::{Clip, Speed};

/// Loudness windows per second: 50, i.e. 20 ms each -- short enough to place a
/// cut inside one frame at any sane rate, long enough that one glottal pulse is
/// not a silence of its own.
pub const WINDOWS_PER_SEC: u32 = 50;

/// What a window with no samples in it, or with digital nothing in it, reads
/// as: a floor rather than `-inf`, so a level is always a number a comparison
/// and a `Display` can take.
pub const SILENT_DB: f32 = -120.;

/// What counts as a silence worth cutting. Seconds, not frames: detection runs
/// on the *sound*, whose sample rate is its own business, and the frame grid
/// is applied once at the end ([`timeline_regions`]).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// At or below this level a window is quiet.
    pub threshold_db: f32,
    /// Quiet shorter than this is forgiven -- the pause between two words is
    /// not a cut.
    pub min_silence: f64,
    /// Kept at both ends of every region, so a cut never lands on the consonant
    /// that starts the next word.
    pub padding: f64,
    /// A kept sliver shorter than this joins the silences either side of it:
    /// breaths and mouth clicks between two long silences would otherwise
    /// survive as one-frame confetti nobody can watch.
    pub min_keep: f64,
}

impl Default for Settings {
    /// The conservative end of what shipped tools default to (auto-editor
    /// ~-28 dBFS / 0.2 s margin, `silencedetect` -60 dB / 2 s): a first run
    /// that leaves a little too much is a run nobody has to undo.
    fn default() -> Self {
        Self {
            threshold_db: -40.,
            min_silence: 0.5,
            padding: 0.15,
            min_keep: 0.15,
        }
    }
}

/// One RMS level in dBFS per [`WINDOWS_PER_SEC`]th of a second of `stream` of
/// `path`, over the half-open `range` of **source seconds** -- the same decode
/// [`crate::waveform::peaks`] runs and with the same contract: channels folded
/// together, `Ok(None)` for a file with **no audio track at all**, which is a
/// source to refuse by name rather than one long silence to cut.
///
/// `levels[0]` is the window at `range.0`, never the window at media time 0:
/// what is read is what is asked for, so a clip that plays a minute of a film
/// costs a minute of decode and the rest of the file is never touched. Whole
/// file is `(0., f64::INFINITY)`.
///
/// Linear in the range's length at ~1700x realtime, so a ten-minute take scans
/// in well under a second; a caller caches it per source, stream and range,
/// because the settings above are all applied to the result and none of them
/// needs a second decode.
pub fn levels(
    path: impl AsRef<Path>,
    stream: usize,
    range: (f64, f64),
) -> crate::Result<Option<Vec<f32>>> {
    levels_with_progress(path, stream, range, &Progress::default())
}

/// What a running [`levels_with_progress`] says about itself while it runs, and
/// the one word that stops it. Tenths of a second rather than a fraction: how
/// far into the sound the decode has come is a fact the moment it starts, where
/// a percentage needs a duration the header does not always carry.
///
/// Shared by an [`Arc`](std::sync::Arc) between the worker and whatever is
/// drawing it, the way an export's progress is.
#[derive(Default, Debug)]
pub struct Progress {
    /// Source time decoded so far, in tenths of a second. Only ever grows.
    pub scanned: AtomicU64,
    /// What the header claims the whole track is, in tenths of a second, or `0`
    /// for a file that does not say -- the estimate is drawn as `~` and a `0`
    /// is drawn as nothing rather than as a lie.
    pub total: AtomicU64,
    /// Set by whoever asked for the scan when nobody is waiting for it any
    /// more. The loop below reads it once per chunk and gives up there; the
    /// decoder behind it stops with the dropped receiver.
    pub cancel: AtomicBool,
}

/// [`levels`] reporting where it has come to and stopping when it is told to.
///
/// A **cancelled scan returns what it had got to**, which is a prefix of the
/// track and not an answer: the caller sets [`Progress::cancel`], so the caller
/// is the one that must check it before believing the result.
pub fn levels_with_progress(
    path: impl AsRef<Path>,
    stream: usize,
    range: (f64, f64),
    progress: &Progress,
) -> crate::Result<Option<Vec<f32>>> {
    let from = range.0.max(0.);
    let sources = [(path.as_ref().to_path_buf(), stream)];
    let Some((meta, rx)) = AudioSession::open_multi_streams(&sources, &[(Some(0), from, range.1)])?
    else {
        return Ok(None);
    };
    let rate = f64::from(meta.sample_rate).max(1.);
    if let Some(total) = meta.total_samples {
        // What is left to read, not what the file is: the line has to count out
        // of the stretch the scan actually decodes or it stalls at a third.
        let secs = (total as f64 / rate).min(range.1) - from;
        progress.total.store((secs.max(0.) * 10.) as u64, Relaxed);
    }
    // Fractional for `peaks`'s reason: 44100 / 50 is whole, 44100 / 30 is not,
    // and a rounded window drifts a bucket every few seconds over a long take.
    let per_window = f64::from(meta.sample_rate) / f64::from(WINDOWS_PER_SEC);
    let channels = (meta.channels as usize).max(1);
    // Chunks are stamped in the source's own samples, counted from where the
    // segment starts reading -- so a scan of a clip an hour in would otherwise
    // build an hour of empty windows in front of it, and every one of those
    // reads as digital silence and would fuse with the first real quiet of the
    // clip. The window the range begins at is window zero here instead.
    let base = (from * f64::from(WINDOWS_PER_SEC)) as usize;
    // Sum of squares and how many samples went into it -- the two halves of a
    // mean square, kept in f64 because a loud minute is millions of terms.
    let mut sums: Vec<(f64, u64)> = Vec::new();
    for chunk in rx {
        // Once per chunk, not per sample: a cancelled scan may run one block
        // longer, and a progress line moves in blocks anyway.
        if progress.cancel.load(Relaxed) {
            break;
        }
        progress.scanned.store(
            ((chunk.start_sample as f64 / rate - from).max(0.) * 10.) as u64,
            Relaxed,
        );
        for (frame, values) in chunk.samples.chunks(channels).enumerate() {
            let at = ((chunk.start_sample + frame as u64) as f64 / per_window) as usize;
            let window = at.saturating_sub(base);
            if window >= sums.len() {
                sums.resize(window + 1, (0., 0));
            }
            let slot = &mut sums[window];
            for &v in values {
                slot.0 += f64::from(v) * f64::from(v);
                slot.1 += 1;
            }
        }
    }
    Ok(Some(sums.iter().map(|&(sq, n)| dbfs(sq, n)).collect()))
}

/// Mean square -> dBFS, floored at [`SILENT_DB`]: full scale is 1.0, so a
/// window of digital zero is `-inf` and is reported as the floor instead.
fn dbfs(square_sum: f64, samples: u64) -> f32 {
    if samples == 0 {
        return SILENT_DB;
    }
    let rms = (square_sum / samples as f64).sqrt();
    if rms <= 0. {
        return SILENT_DB;
    }
    (20. * rms.log10()).max(f64::from(SILENT_DB)) as f32
}

/// The silent stretches of a `levels` track, as half-open `(from, to)` seconds
/// **from where the read began**, in order and never overlapping.
///
/// Pure arithmetic: this is what a card re-runs on every keystroke. The three
/// settings are applied in the order a person means them -- quiet runs first,
/// then the short ones forgiven ([`Settings::min_silence`]), then a
/// [`Settings::padding`] left at both ends so the cut never touches speech, and
/// last the slivers too short to keep ([`Settings::min_keep`]) swallowed by the
/// silences around them. Padding before the merge on purpose: what the merge
/// measures is what would actually survive the cut.
pub fn regions(levels: &[f32], cfg: Settings) -> Vec<(f64, f64)> {
    let window = 1. / f64::from(WINDOWS_PER_SEC);
    let mut quiet: Vec<(f64, f64)> = Vec::new();
    let mut run: Option<usize> = None;
    for (i, &db) in levels.iter().enumerate() {
        match (db <= cfg.threshold_db, run) {
            (true, None) => run = Some(i),
            (false, Some(from)) => {
                quiet.push((from as f64 * window, i as f64 * window));
                run = None;
            }
            _ => {}
        }
    }
    if let Some(from) = run {
        quiet.push((from as f64 * window, levels.len() as f64 * window));
    }
    let padded = quiet
        .into_iter()
        .filter(|&(from, to)| to - from >= cfg.min_silence)
        .map(|(from, to)| (from + cfg.padding, to - cfg.padding))
        .filter(|&(from, to)| to > from);
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (from, to) in padded {
        match out.last_mut() {
            Some(last) if from - last.1 < cfg.min_keep => last.1 = to,
            _ => out.push((from, to)),
        }
    }
    out
}

/// Where those source stretches sit on the timeline, as `(start, len)` frame
/// pairs of the lane `clip` is on -- what a preview marks and what
/// [`crate::Project::cut_regions`] cuts.
///
/// `regions` are seconds from the clip's **in point**, which is where
/// [`levels`] was asked to start reading; they land at `in_frame` plus that.
/// Only the part of each region the clip actually plays survives -- a region
/// running past `out_frame` is trimmed to it and one starting beyond it is
/// dropped, so a short read or a rounding at the tail cannot mark a frame the
/// clip does not have. Positions go through the clip's **own rate**
/// ([`Speed::timeline_at`]), so a silence in a 2x clip marks the frames it is
/// really on.
///
/// Both edges are rounded **inward** -- start up, end down -- so a frame that
/// is partly speech stays outside the region at every rate. An empty remainder
/// is dropped rather than returned as a zero-length cut.
pub fn timeline_regions(clip: &Clip, fps: f64, regions: &[(f64, f64)]) -> Vec<(u32, u32)> {
    if !(fps.is_finite() && fps > 0.) {
        return Vec::new();
    }
    let frame = |secs: f64, round: fn(f64) -> f64| {
        let n = round(secs * fps);
        n.clamp(0., f64::from(u32::MAX)) as u32
    };
    regions
        .iter()
        .filter_map(|&(from, to)| {
            let first = clip.in_frame.saturating_add(frame(from, f64::ceil));
            let last = clip
                .in_frame
                .saturating_add(frame(to, f64::floor))
                .min(clip.out_frame);
            if last <= first {
                return None;
            }
            let start = clip.start + clip.speed.timeline_at(first - clip.in_frame);
            let end = clip.start + floor_timeline(clip.speed, last - clip.in_frame);
            (end > start).then_some((start, end - start))
        })
        .collect()
}

/// [`Speed::timeline_at`] floored instead of ceiled: which timeline frame of a
/// clip a source frame lands on, rounded the way an *end* has to be so a
/// region never eats the frame speech starts on. The ceil is the right rounding
/// for a start and for playback's stamps, and this is its partner.
fn floor_timeline(speed: Speed, offset: u32) -> u32 {
    (u64::from(offset) * 1000 / u64::from(speed.permille())).min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Settings, levels, regions, timeline_regions};
    use crate::project::{Clip, Speed};
    use crate::scale::FitPolicy;

    fn asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(name)
    }

    fn clip(start: u32, in_frame: u32, out_frame: u32, speed: Speed) -> Clip {
        Clip {
            fade_in: 0,
            fade_out: 0,
            transition_out: 0,
            start,
            in_frame,
            out_frame,
            source: 0,
            link: None,
            eq: None,
            color: None,
            transform: None,
            fit: FitPolicy::default(),
            speed,
        }
    }

    /// A level track: `db` for every window, with the named half-open window
    /// ranges dropped to digital silence.
    fn track(windows: usize, db: f32, quiet: &[(usize, usize)]) -> Vec<f32> {
        let mut out = vec![db; windows];
        for &(from, to) in quiet {
            out[from..to].fill(super::SILENT_DB);
        }
        out
    }

    /// Seconds compared as seconds: a window edge is a multiple of 0.02 and a
    /// padding is added to it, neither of which is exact in binary.
    fn near(got: &[(f64, f64)], want: &[(f64, f64)]) {
        assert_eq!(got.len(), want.len(), "{got:?} against {want:?}");
        for (&(a, b), &(c, d)) in got.iter().zip(want) {
            assert!(
                (a - c).abs() < 1e-9 && (b - d).abs() < 1e-9,
                "{got:?} against {want:?}"
            );
        }
    }

    /// The fixture's envelope is `0.5 + 0.5*sin(2*PI*t)`: full silence every
    /// second at t = 0.75. Detection has to land on those five dips and on
    /// nothing else -- a scan that lost the chunk positions, or that measured
    /// peaks instead of RMS, would not line up second after second.
    #[test]
    fn the_dips_of_the_1hz_pulse_are_where_the_silences_are() {
        let levels = levels(asset("test_av.mp4"), 0, (0., f64::INFINITY))
            .expect("open")
            .expect("test_av.mp4 has an audio track");
        // A short forgiveness, because each dip of a sine is brief.
        let cfg = Settings {
            threshold_db: -40.,
            min_silence: 0.08,
            padding: 0.,
            min_keep: 0.,
        };
        let found = regions(&levels, cfg);
        assert_eq!(found.len(), 5, "{found:?}");
        for (second, &(from, to)) in found.iter().enumerate() {
            let dip = second as f64 + 0.75;
            assert!(
                (from..to).contains(&dip),
                "second {second}: {from}..{to} does not cover the dip at {dip}"
            );
        }
        // Nothing survives a forgiveness longer than any dip is.
        assert!(
            regions(
                &levels,
                Settings {
                    min_silence: 1.5,
                    ..cfg
                }
            )
            .is_empty()
        );
    }

    /// Reporting where a scan has got to changes nothing about what it finds,
    /// and the word that stops one is heard: the numbers are the plain scan's
    /// to the last window, the header's duration reaches the card, and a scan
    /// cancelled before its first chunk comes back with a prefix instead of a
    /// track -- which is why a cancelled scan is never cached.
    #[test]
    fn a_scan_that_reports_itself_finds_exactly_what_the_plain_one_does() {
        use super::{Progress, levels_with_progress};
        use std::sync::atomic::Ordering::Relaxed;
        let progress = Progress::default();
        let watched = levels_with_progress(asset("test_av.mp4"), 0, (0., f64::INFINITY), &progress)
            .expect("open")
            .expect("test_av.mp4 has an audio track");
        let plain = levels(asset("test_av.mp4"), 0, (0., f64::INFINITY))
            .expect("open")
            .expect("audio");
        assert_eq!(watched, plain, "a watched scan must find the same levels");
        // Five seconds of fixture, in tenths, from the header and from the last
        // chunk the loop saw.
        assert_eq!(progress.total.load(Relaxed), 50);
        assert!(progress.scanned.load(Relaxed) > 0);
        assert!(progress.scanned.load(Relaxed) <= progress.total.load(Relaxed));
        // Cancelled at the door: the loop gives up at its first chunk, the
        // receiver is dropped and the decoder behind it stops with it.
        let stopped = Progress::default();
        stopped.cancel.store(true, Relaxed);
        let partial = levels_with_progress(asset("test_av.mp4"), 0, (0., f64::INFINITY), &stopped)
            .expect("open")
            .expect("audio");
        assert!(partial.len() < plain.len(), "{} windows", partial.len());
    }

    /// A clip cut in half costs half a decode, and the half it reads is its
    /// own: asked for `[2, 4)` seconds the scan hands back two seconds of
    /// windows, starting at the *second* second -- the fixture's dips at 2.75
    /// and 3.75 land at 0.75 and 1.75 of what comes back, and the three dips
    /// outside the range are not in it at all.
    #[test]
    fn a_range_is_read_from_its_own_start_and_nothing_outside_it_is() {
        let cfg = Settings {
            threshold_db: -40.,
            min_silence: 0.08,
            padding: 0.,
            min_keep: 0.,
        };
        let whole = levels(asset("test_av.mp4"), 0, (0., f64::INFINITY))
            .expect("open")
            .expect("audio");
        let half = levels(asset("test_av.mp4"), 0, (2., 4.))
            .expect("open")
            .expect("audio");
        // Two seconds of windows for two seconds asked, give or take the one a
        // packet boundary rounds -- not the five the file is.
        assert!(
            (half.len() as i64 - 100).abs() <= 2 && half.len() < whole.len(),
            "{} windows for [2, 4) of {} for the file",
            half.len(),
            whole.len()
        );
        let found = regions(&half, cfg);
        assert_eq!(found.len(), 2, "{found:?}");
        for (nth, &(from, to)) in found.iter().enumerate() {
            let dip = nth as f64 + 0.75;
            assert!(
                (from..to).contains(&dip),
                "dip {nth}: {from}..{to} does not cover {dip}"
            );
        }
        // ...and on the timeline they are where the clip plays them: a clip
        // reading from source frame 60 at 30 fps, laid at frame 500.
        let placed = clip(500, 60, 120, Speed::NORMAL);
        let marks = timeline_regions(&placed, 30., &found);
        assert_eq!(marks.len(), 2, "{marks:?}");
        for (nth, &(start, len)) in marks.iter().enumerate() {
            let dip = 500. + (nth as f64 + 0.75) * 30.;
            assert!(
                (f64::from(start)..f64::from(start + len)).contains(&dip),
                "dip {nth}: [{start}, {}) is not the dip at {dip}",
                start + len
            );
        }
    }

    /// The three settings, each one visible on its own: a quiet run shorter
    /// than `min_silence` is forgiven, `padding` shrinks what is left at both
    /// ends, and a kept sliver shorter than `min_keep` joins the silences
    /// around it rather than surviving as one-frame confetti.
    #[test]
    fn forgiveness_padding_and_min_keep_each_show_up() {
        // 10 s of speech with a 0.2 s dip, a 1 s silence and a 2 s silence.
        let levels = track(500, -12., &[(50, 60), (100, 150), (300, 400)]);
        let bare = Settings {
            threshold_db: -40.,
            min_silence: 0.5,
            padding: 0.,
            min_keep: 0.,
        };
        near(&regions(&levels, bare), &[(2., 3.), (6., 8.)]);
        // Padding takes 0.15 s off each end of each region, and off nothing
        // else: a region is 0.3 s shorter and starts 0.15 s later.
        let padded = regions(
            &levels,
            Settings {
                padding: 0.15,
                ..bare
            },
        );
        near(&padded, &[(2.15, 2.85), (6.15, 7.85)]);
        // Two silences with a two-window sliver of speech between them: with
        // nothing to keep it, the sliver joins them into one region.
        let sliver = track(500, -12., &[(100, 150), (152, 250)]);
        assert_eq!(
            regions(&sliver, bare).len(),
            2,
            "the sliver is kept while min_keep is zero"
        );
        let merged = regions(
            &sliver,
            Settings {
                min_keep: 0.15,
                ..bare
            },
        );
        near(&merged, &[(2., 5.)]);
    }

    /// Where a region lands on the timeline is the clip's business: seconds
    /// counted from its in point, its placement offsets them, its end trims
    /// them and its rate divides them. A 2x clip's silences mark half as many
    /// frames, at the right place.
    #[test]
    fn regions_map_through_the_clips_range_and_rate() {
        let secs = [(1., 2.), (3., 3.5)];
        // Whole file, real time, placed at the start: seconds read are timeline
        // frames at 30 fps.
        let whole = clip(0, 0, 300, Speed::NORMAL);
        assert_eq!(
            timeline_regions(&whole, 30., &secs),
            vec![(30, 30), (90, 15)]
        );
        // The same clip placed at frame 100 and reading from source frame 45:
        // the scan began at 45, so a region a second in is source frame 75 --
        // and it lands 30 frames after the clip's own start.
        let placed = clip(100, 45, 300, Speed::NORMAL);
        assert_eq!(
            timeline_regions(&placed, 30., &secs),
            vec![(130, 30), (190, 15)]
        );
        // At 2x every timeline frame is two source frames, so both regions are
        // half as wide and sit half as far along.
        let fast = clip(0, 0, 300, Speed::from_permille(2000));
        assert_eq!(timeline_regions(&fast, 30., &secs), vec![(15, 15), (45, 7)]);
        // A read that ran past the clip's end is trimmed to it, and a region
        // wholly past it is not on the timeline at all.
        let short = clip(0, 0, 45, Speed::NORMAL);
        assert_eq!(timeline_regions(&short, 30., &[(1., 2.)]), vec![(30, 15)]);
        assert!(timeline_regions(&short, 30., &[(2., 3.)]).is_empty());
    }

    /// Rounded inward at both ends: a region whose seconds fall between frames
    /// gives back the frames it *fully* covers, so no frame of speech is ever
    /// inside a cut. Half a frame of silence is dropped, never claimed.
    #[test]
    fn edges_round_inward_so_speech_keeps_its_frames() {
        let whole = clip(0, 0, 300, Speed::NORMAL);
        // [1.01, 1.99) s at 30 fps is source frames 30.3 .. 59.7: the region
        // may only claim 31..59.
        assert_eq!(
            timeline_regions(&whole, 30., &[(1.01, 1.99)]),
            vec![(31, 28)]
        );
        // Under a frame wide: nothing survives the rounding, and a zero-length
        // cut is never handed back.
        assert!(timeline_regions(&whole, 30., &[(1.01, 1.02)]).is_empty());
    }
}
