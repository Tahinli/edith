//! The baseline every later decode/encode change is measured against: open,
//! seek, seek storm, play loop, scrub and export, timed on the real library
//! rather than on a fixture.
//!
//! `cargo run --release -p engine --example bench -- <metric> <file> [args]`
//!
//! One metric per process, on purpose: a decoder that panics (the vendored
//! HEVC parser does, on a 2160p remux) must cost one row and not the run, so
//! `scripts/bench.sh` reads the exit status of each of these and writes the
//! failure down as data.
//!
//! Every row is one TSV line -- `file, metric, unit, n, median, min, max, note`
//! -- so the whole baseline is `cat`ted together and diffed later. The note is
//! where the tail percentile and the **decode backend** live: a row that does
//! not say `backend hw` was measured in software, and a baseline that does not
//! say which is not a baseline (see `scripts/bench.sh` on `LD_LIBRARY_PATH`).
//!
//! **Cold and warm.** Cold means the file's pages are dropped from the page
//! cache immediately before the measurement, with `posix_fadvise(DONTNEED)` on
//! a fd of our own: no `drop_caches`, no root, and nothing else on the machine
//! is evicted. It is advisory -- a page another process still holds mapped
//! stays -- which is why the cold numbers are a *floor* on the cold cost and
//! the run log records that. Warm means the same open repeated with the cache
//! left as the previous one filled it.
//!
//! **Media files are read only here.** Nothing in this file opens anything for
//! writing except the export destination it is handed.
//!
//! Env: `BENCH_TTFF_TIMEOUT` (s, default 180) how long a seek may wait for its
//! first frame, `BENCH_EXPORT_SPAN` (s, default 60) how much timeline one
//! export measurement covers, `BENCH_EXPORT_AUDIO` (unset) whether that
//! timeline carries its source's sound as well -- the row is named `_av` when
//! it does, because an export's wall clock is the audio pass plus the picture
//! and a picture-only number is not the thing a person waits for --
//! `BENCH_EXPORT_ASTREAM` (default 0) which of the file's audio streams that
//! is, a film's second track often being the one no sample table can copy and
//! therefore the one that measures a re-encode,
//! `BENCH_EXPORT_CAP` (s, default 300) the wall clock any single
//! export measurement may take, `BENCH_EXPORT_REPS` (default 5) how many times
//! an export is repeated *if the cap leaves room* -- a 4K encode gets one
//! capped rep, the small control gets five -- and `BENCH_KEEP` (unset) whether
//! the exported file survives the run instead of being deleted with the number.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::export::{EncoderSeat, ExportSettings, Format};
use engine::project::{Clip, Lane, LaneKind, Project, Source, Speed};

// `posix_fadvise(2)`, straight from libc, which std already links: dropping
// this file's pages needs no crate and no privilege.
unsafe extern "C" {
    fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32;
}
const POSIX_FADV_DONTNEED: i32 = 4;

fn main() {
    let mut args = std::env::args().skip(1);
    let metric = args.next().unwrap_or_else(|| usage());
    let path = PathBuf::from(args.next().unwrap_or_else(|| usage()));
    match metric.as_str() {
        "open" => open_bench(&path),
        "seek" => seek_bench(&path, arg_f64(args.next(), "seconds")),
        "seekstorm" => seekstorm_bench(&path, arg_seed(args.next())),
        "playloop" => playloop_bench(&path, arg_f64(args.next(), "seconds"), arg_seed(args.next())),
        "cutloop" => cutloop_bench(
            &path,
            arg_or(args.next(), 20.0) as u32,
            arg_or(args.next(), 0.5),
            arg_or(args.next(), 0.3),
        ),
        "scrub" => scrub_bench(&path),
        "waveform" => waveform_bench(&path),
        "export" => {
            let seat = args.next().unwrap_or_else(|| usage());
            let out_dir = PathBuf::from(args.next().unwrap_or_else(|| usage()));
            export_bench(&path, &seat, &out_dir);
        }
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: bench open <file>\n       bench seek <file> <secs>\n       \
         bench seekstorm <file> [seed]\n       \
         bench playloop <file> <secs> [seed]\n       \
         bench cutloop <file> [regions=20] [keep_s=0.5] [cut_s=0.3]\n       \
         bench scrub <file>\n       bench waveform <file>\n       \
         bench export <file> <h264sw|h264hw|av1|hevc|hevchw> <out_dir>"
    );
    std::process::exit(2)
}

fn arg_f64(arg: Option<String>, what: &str) -> f64 {
    arg.and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("missing {what}"))
}

/// An optional measurement parameter with a default, so a rerun that names
/// nothing runs the same shape of timeline the last one did.
fn arg_or(arg: Option<String>, default: f64) -> f64 {
    arg.and_then(|s| s.parse().ok()).unwrap_or(default)
}

/// The seed the random metrics draw their positions from: same seed, same
/// seeks, so a before and an after are the same measurement.
fn arg_seed(arg: Option<String>) -> u64 {
    arg.and_then(|s| s.parse().ok()).unwrap_or(1)
}

// ---------------------------------------------------------------- reporting

/// One TSV row: the median is what a comparison uses, the min and max are what
/// says whether the median means anything.
fn row(file: &Path, metric: &str, unit: &str, samples: &[f64], note: &str) {
    let name = file.file_name().unwrap_or(file.as_os_str()).to_string_lossy();
    if samples.is_empty() {
        println!("{name}\t{metric}\t{unit}\t0\t\t\t\t{note}");
        return;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = if sorted.len() % 2 == 1 {
        sorted[sorted.len() / 2]
    } else {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    };
    println!(
        "{name}\t{metric}\t{unit}\t{}\t{median:.2}\t{:.2}\t{:.2}\t{note}",
        sorted.len(),
        sorted[0],
        sorted[sorted.len() - 1],
    );
}

// ------------------------------------------------------------------- helpers

/// Drops this file's page cache. Advisory: pages someone else holds survive,
/// so a "cold" number here is the optimistic end of cold.
fn evict(path: &Path) {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::File::open(path).expect("open for evict");
    let len = file.metadata().map_or(0, |m| m.len());
    // SAFETY: a valid fd of ours, a length from its own metadata, and an
    // advice value that changes nothing but the kernel's cache bookkeeping.
    let rc = unsafe { posix_fadvise(file.as_raw_fd(), 0, len as i64, POSIX_FADV_DONTNEED) };
    if rc != 0 {
        eprintln!("posix_fadvise: rc {rc} (measurement is warm, not cold)");
    }
}

/// Threads this process holds right now -- the scrub metric that matters as
/// much as the latency, a drag storm having peaked in the hundreds.
fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").map_or(0, Iterator::count)
}

fn ttff_timeout() -> Duration {
    Duration::from_secs_f64(env_f64("BENCH_TTFF_TIMEOUT", 180.0))
}

/// The p'th percentile of `samples`, nearest rank on the sorted copy. The
/// median is [`row`]'s business; this is for the tail, which is the number a
/// dropped frame and a starved ring both live in.
fn pct(samples: &[f64], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// A seeded LCG, so "random" seeks are the same random seeks tomorrow. Sixteen
/// lines of a crate would do the same thing and none of the quality a real
/// generator has matters to a list of positions in a film.
struct Lcg(u64);

impl Lcg {
    /// The next position, in `0.0..1.0`.
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Seeks and waits for the picture that seek asked for: the number a user
/// feels. The error side is the two ways no picture arrives, and both are a
/// row rather than a crash: `worker died`, which is what a decoder panicking
/// on its own thread looks like from here (the channel drops, the session runs
/// out of clips, and that is end-of-stream a millisecond after a seek), and
/// `timeout`, which is a decode still grinding when the clock ran out.
///
/// `peak` collects the thread count seen while waiting, so the scrub's storm
/// is measured by the same poll loop that measures its latency.
fn seek_ttff(
    session: &mut engine::PlaybackSession,
    secs: f64,
    peak: &mut usize,
) -> Result<f64, &'static str> {
    let deadline = Instant::now() + ttff_timeout();
    let t = Instant::now();
    session.seek(secs);
    loop {
        *peak = (*peak).max(threads());
        if session.try_frame().is_some() {
            return Ok(t.elapsed().as_secs_f64() * 1000.0);
        }
        if session.is_eos() {
            return Err("worker died");
        }
        if Instant::now() >= deadline {
            return Err("timeout");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

// -------------------------------------------------------------------- open

/// `Demuxer::open`, five times cold and five times warm. Matroska has no index
/// here and this walks every cluster header of the whole file, which is why it
/// is the first thing measured.
fn open_bench(path: &Path) {
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    let mut note = String::new();
    for _ in 0..5 {
        evict(path);
        let t = Instant::now();
        match engine::demux::Demuxer::open(path) {
            Ok((meta, _)) => {
                cold.push(t.elapsed().as_secs_f64() * 1000.0);
                if note.is_empty() {
                    note = format!(
                        "{}x{} {:.3}fps {} frames",
                        meta.width, meta.height, meta.frame_rate, meta.frame_count
                    );
                }
            }
            Err(e) => {
                row(path, "open_cold", "ms", &[], &format!("FAIL({e})"));
                return;
            }
        }
        let t = Instant::now();
        let _ = engine::demux::Demuxer::open(path).expect("warm open");
        warm.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    row(path, "open_cold", "ms", &cold, &note);
    row(path, "open_warm", "ms", &warm, &note);
}

// -------------------------------------------------------------------- seek

/// First-frame latency for a seek to `secs`, five times, on a session that is
/// already open -- the `open_worker_deferred` path (`decode.rs`), which is
/// what every seek, clip boundary and export span goes through.
///
/// The file is evicted once before the session is opened, so rep 1 is cold and
/// the rest are as warm as the previous seek left them: the spread between min
/// and max is that difference and is meant to be read.
fn seek_bench(path: &Path, secs: f64) {
    evict(path);
    let mut session = match engine::PlaybackSession::open(path) {
        Ok(session) => session,
        Err(e) => {
            row(path, &format!("seek_ttff_{secs:.0}s"), "ms", &[], &format!("FAIL({e})"));
            return;
        }
    };
    let duration = session.timeline_duration();
    if secs > duration {
        row(
            path,
            &format!("seek_ttff_{secs:.0}s"),
            "ms",
            &[],
            &format!("SKIP(file is {duration:.0}s)"),
        );
        return;
    }
    let mut samples = Vec::new();
    let mut missed = Vec::new();
    let mut peak = 0;
    for _ in 0..5 {
        match seek_ttff(&mut session, secs, &mut peak) {
            Ok(ms) => samples.push(ms),
            Err(why) => missed.push(why),
        }
    }
    let backend = session.decode_backend().label();
    let note = if missed.is_empty() {
        format!("backend {backend}")
    } else {
        format!(
            "backend {backend}, {}/5 NO-FRAME({})",
            missed.len(),
            missed[0]
        )
    };
    row(path, &format!("seek_ttff_{secs:.0}s"), "ms", &samples, &note);
}

// -------------------------------------------------------------- seek storm

/// Seeks a film opens with none of: fifty positions nobody chose, drawn from a
/// seed so the same fifty come back tomorrow. `seek` measures one point five
/// times over; this measures the spread across the whole file, which is where
/// an index lookup, a keyframe distance and a decoder reopen all vary.
///
/// Cold is the first pass over a freshly evicted file, warm the same fifty
/// again straight after -- the same two questions the other metrics ask, and
/// the audio underruns each pass cost are counted with it
/// ([`engine::PlaybackSession::audio_underruns`]), because a seek that lands
/// fast and starves the ring is not a seek that worked.
const STORM_SEEKS: usize = 50;

fn seekstorm_bench(path: &Path, seed: u64) {
    evict(path);
    let mut session = match engine::PlaybackSession::open(path) {
        Ok(session) => session,
        Err(e) => {
            row(path, "seekstorm_ttff_cold", "ms", &[], &format!("FAIL({e})"));
            return;
        }
    };
    // Playing throughout, because the underruns are the point: a paused device
    // runs no callbacks and can never starve, so a storm measured paused would
    // report zero however badly the seeks fed it. This is the complaint's own
    // shape -- a film that is playing, jumped around in.
    session.play();
    let duration = session.timeline_duration();
    let mut rng = Lcg(seed);
    // Short of the very end: a landing with no picture after it is a different
    // measurement (end of stream), and every seek here is meant to be one.
    let targets: Vec<f64> = (0..STORM_SEEKS)
        .map(|_| rng.next() * duration * 0.95)
        .collect();

    for pass in ["cold", "warm"] {
        let before = session.audio_underruns().map_or(0, |(n, _)| n);
        let mut samples = Vec::new();
        let mut missed = Vec::new();
        let mut peak = threads();
        for &secs in &targets {
            match seek_ttff(&mut session, secs, &mut peak) {
                Ok(ms) => samples.push(ms),
                Err(why) => missed.push(why),
            }
        }
        let audio = session.audio_underruns();
        let backend = session.decode_backend().label();
        let mut note = format!(
            "backend {backend}, p95 {:.2}ms, peak {peak} threads, seed {seed}",
            pct(&samples, 0.95)
        );
        if !missed.is_empty() {
            note += &format!(", {}/{STORM_SEEKS} NO-FRAME({})", missed.len(), missed[0]);
        }
        row(path, &format!("seekstorm_ttff_{pass}"), "ms", &samples, &note);
        row(
            path,
            &format!("seekstorm_underruns_{pass}"),
            "count",
            &[audio.map_or(0, |(n, _)| n).saturating_sub(before) as f64],
            &match audio {
                None => "no audio device".into(),
                Some((total, last)) => format!(
                    "backend {backend}, {total} since open, {}",
                    last.map_or_else(|| "none yet".into(), |t| format!("last at {t:.1}s device"))
                ),
            },
        );
    }
}

// --------------------------------------------------------------- play loop

/// How long a picture waits between frames while the sound runs: the app's own
/// pump (`crates/app/src/player/transport.rs`), driven here for `secs` with a
/// seek to a random point every five seconds -- the shape of the complaint,
/// which is a film played and jumped around in rather than a seek measured on
/// its own.
///
/// Three numbers come out: the gap between frames (p50 and p99, the tail being
/// the jump a person sees), the audio underruns over the whole run, and how
/// many times the picture worker was restarted -- by a seek, by a clip
/// boundary, or by the late-picture resync, which is counted separately in the
/// note.
///
/// The resync rule is the app's, repeated here (`LATE_RESYNC`, `RESYNC_GAP`
/// and the never-during-a-prime gate in `crates/app/src/player/transport.rs`)
/// because an engine example cannot depend on the binary. Repeated only:
/// nothing in this file changes how playback runs.
const PLAYLOOP_SEEK_GAP: Duration = Duration::from_secs(5);
const LATE_RESYNC: f64 = 0.4;
const RESYNC_GAP: Duration = Duration::from_secs(2);

fn playloop_bench(path: &Path, secs: f64, seed: u64) {
    evict(path);
    let mut session = match engine::PlaybackSession::open(path) {
        Ok(session) => session,
        Err(e) => {
            row(path, "playloop_frame_gap", "ms", &[], &format!("FAIL({e})"));
            return;
        }
    };
    let fps = session.meta().frame_rate;
    let duration = session.timeline_duration();
    let mut rng = Lcg(seed);

    let mut gaps = Vec::new();
    let mut held: Option<engine::Frame> = None;
    let (mut shown, mut dropped, mut seeks, mut resyncs) = (0u64, 0u64, 0u64, 0u64);
    let mut last_resync: Option<Instant> = None;
    // The first frame of the run is owed exactly as a seek's landing is.
    let mut owed = true;

    session.play();
    let start = Instant::now();
    let deadline = start + Duration::from_secs_f64(secs);
    let mut last_frame = start;
    let mut next_seek = start + PLAYLOOP_SEEK_GAP;
    while Instant::now() < deadline {
        session.tick();
        // Read before the drain, as the app does: the frame the drain takes
        // is the one that ends the prime, and its lateness is the reopen's.
        let priming = session.picture_priming();
        let target = session.now() * fps;
        let mut newest = None;
        while session.is_playing() || owed {
            let frame = match held.take() {
                Some(frame) => frame,
                None => match session.try_frame() {
                    // Timed here and only here: a frame taken back out of
                    // `held` cost no decode, and counting it would report a
                    // zero gap for a frame nobody waited for.
                    Some(frame) => {
                        let now = Instant::now();
                        gaps.push((now - last_frame).as_secs_f64() * 1000.0);
                        last_frame = now;
                        frame
                    }
                    None => break,
                },
            };
            if f64::from(frame.index) <= target {
                dropped += u64::from(newest.is_some());
                newest = Some(frame);
            } else {
                held = Some(frame);
                break;
            }
        }
        let late = newest
            .as_ref()
            .map_or(0.0, |f| (target - f64::from(f.index)) / fps);
        if newest.is_some() {
            shown += 1;
            owed = false;
        }
        if !owed
            && !priming
            && session.is_playing()
            && late > LATE_RESYNC
            && last_resync.is_none_or(|t| t.elapsed() >= RESYNC_GAP)
        {
            session.resync_picture();
            held = None;
            last_resync = Some(Instant::now());
            resyncs += 1;
        }
        // End of stream inside a five-second stretch is the same jump early:
        // the loop is here to keep playing, not to stop at whatever it landed
        // near.
        if Instant::now() >= next_seek || session.is_eos() {
            session.seek(rng.next() * duration * 0.95);
            held = None;
            owed = true;
            seeks += 1;
            next_seek = Instant::now() + PLAYLOOP_SEEK_GAP;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    session.pause();

    let elapsed = start.elapsed().as_secs_f64();
    let backend = session.decode_backend().label();
    let audio = session.audio_underruns();
    row(
        path,
        "playloop_frame_gap",
        "ms",
        &gaps,
        &format!(
            "backend {backend}, p99 {:.2}ms, {shown} shown, {dropped} dropped, \
             {seeks} seeks in {elapsed:.0}s, seed {seed}",
            pct(&gaps, 0.99)
        ),
    );
    row(
        path,
        "playloop_underruns",
        "count",
        &[audio.map_or(0, |(n, _)| n) as f64],
        &match audio {
            None => "no audio device".into(),
            Some((_, last)) => format!(
                "backend {backend}, {}",
                last.map_or_else(|| "none".into(), |t| format!("last at {t:.1}s device"))
            ),
        },
    );
    row(
        path,
        "playloop_restarts",
        "count",
        &[session.restarts() as f64],
        &format!("backend {backend}, {resyncs} late-picture resyncs, {seeks} seeks"),
    );
}

// ----------------------------------------------------------------- cutloop

/// The freeze metric: a **silence-cut** timeline -- `regions` holes of
/// `cut_s` punched out of one file, `keep_s` of picture left between them --
/// played end to end with the app's own pump, drop policy and resync rule
/// (`playloop_bench`'s, without the random seeks). Every boundary between two
/// kept clips is a same-file reseek of the picture worker while the audio
/// feeder runs straight through -- exactly the timeline a silence removal
/// leaves, and the complaint it brought: clips shorter than a decoder's own
/// re-prime, a picture that starves while the sound plays on, and a
/// late-picture resync every `RESYNC_GAP` buying the same reopen again.
///
/// What comes out: displayed and dropped pictures per wall-clock second (a
/// freeze is a second that shows nothing), on stderr one line each, and in
/// the TSV the per-second displayed counts beside the totals -- restarts,
/// resyncs and underruns -- that say which mechanism spent the time.
///
/// ```text
/// bench cutloop <4k file> 20 0.5 0.3
/// ```
fn cutloop_bench(path: &Path, regions: u32, keep_s: f64, cut_s: f64) {
    let mut session = match engine::PlaybackSession::open(path) {
        Ok(session) => session,
        Err(e) => {
            row(path, "cutloop_displayed", "count", &[], &format!("FAIL({e})"));
            return;
        }
    };
    // Muted like the tests: the device still schedules the stream and the
    // clock still counts its samples, which is the whole of what this
    // measures -- the sound is the master the picture is starving against.
    session.set_gain(0.0);
    session.drop_late_pictures(true);
    let fps = session.meta().frame_rate;

    // Cut every hole out before a single picture decodes: keep, cut, keep,
    // cut -- `regions` pairs of boundaries, so the timeline that plays is
    // `regions + 1` clips of one file with the holes joined shut.
    let stride = keep_s + cut_s;
    for i in 0..regions {
        let at = keep_s + f64::from(i) * stride;
        if !session.cut_at(at) || !session.cut_at(at + cut_s) {
            row(
                path,
                "cutloop_displayed",
                "count",
                &[],
                &format!("FAIL(file shorter than region {i} of {regions})"),
            );
            return;
        }
    }
    // ...and the end of the last keep, so the timeline that plays out is the
    // keeps alone and not the rest of the film (a cut past the end of the
    // file is no cut, and the shape check below says so if it mattered).
    session.cut_at(f64::from(regions) * stride + keep_s);
    eprintln!(
        "cutloop: spans before the deletes: {:?}",
        session.clip_spans()
    );
    // The holes are the odd clips and the tail is the last: deleted from the
    // far end down, so no delete moves the index of a clip still to be
    // deleted. V1 names each hole and the group takes the A1 half of the
    // take with it; the shape check below guards the lot.
    for idx in (1..=2 * regions as usize + 1).rev().step_by(2) {
        if !session.delete_clip(Lane::V1, idx) {
            row(
                path,
                "cutloop_displayed",
                "count",
                &[],
                &format!("FAIL(clip {idx} of the cut timeline would not delete)"),
            );
            return;
        }
    }
    eprintln!(
        "cutloop: spans after the deletes: {:?}",
        session.clip_spans()
    );
    let spans = session.clip_spans();
    let timeline = session.timeline_duration();
    let expect = (f64::from(regions) + 1.0) * keep_s;
    if (timeline - expect).abs() > keep_s {
        row(
            path,
            "cutloop_displayed",
            "count",
            &[],
            &format!("FAIL(timeline {timeline:.2}s, expected ~{expect:.2}s of kept clips)"),
        );
        return;
    }
    eprintln!(
        "cutloop: {regions} regions of {cut_s}s cut, {} clips left on the \
         video lane, {timeline:.1}s of timeline at {fps:.3} fps",
        spans.len()
    );

    session.seek(0.0);
    session.play();
    let start = Instant::now();
    let mut held: Option<engine::Frame> = None;
    let (mut shown, mut dropped, mut resyncs) = (0u64, 0u64, 0u64);
    let mut last_resync: Option<Instant> = None;
    // The first frame of the run is owed exactly as a seek's landing is.
    let mut owed = true;
    let mut per_second: Vec<(u64, u64)> = vec![(0, 0)];
    loop {
        session.tick();
        // Read before the drain, as the app does: the frame the drain takes
        // is the one that ends the prime, and its lateness is the reopen's.
        let priming = session.picture_priming();
        let target = session.now() * fps;
        let mut newest = None;
        let mut dropped_now = 0u64;
        while session.is_playing() || owed {
            let frame = match held.take() {
                Some(frame) => frame,
                None => match session.try_frame() {
                    Some(frame) => frame,
                    None => break,
                },
            };
            if f64::from(frame.index) <= target {
                dropped_now += u64::from(newest.is_some());
                newest = Some(frame);
            } else {
                held = Some(frame);
                break;
            }
        }
        dropped += dropped_now;
        let late = newest
            .as_ref()
            .map_or(0.0, |f| (target - f64::from(f.index)) / fps);
        if newest.is_some() {
            shown += 1;
            owed = false;
        }
        let slot = start.elapsed().as_secs() as usize;
        if per_second.len() <= slot {
            per_second.resize(slot + 1, (0, 0));
        }
        per_second[slot].0 += u64::from(newest.is_some());
        per_second[slot].1 += dropped_now;
        if !owed
            && !priming
            && session.is_playing()
            && late > LATE_RESYNC
            && last_resync.is_none_or(|t| t.elapsed() >= RESYNC_GAP)
        {
            eprintln!("cutloop: picture {late:.3}s behind the clock: restarting it there");
            session.resync_picture();
            held = None;
            last_resync = Some(Instant::now());
            resyncs += 1;
        }
        // End of stream is the end of the run: the clock is halted on the out
        // point, as the app's transport does, so the last second's bucket is
        // closed rather than walked off the end of the timeline.
        if session.is_eos() {
            session.halt_at_end();
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    session.pause();

    let elapsed = start.elapsed().as_secs_f64();
    let rates: Vec<f64> = per_second.iter().map(|(s, _)| *s as f64).collect();
    let backend = session.decode_backend().label();
    let underruns = session.audio_underruns().map_or(0, |(n, _)| n);
    for (i, (shown, dropped)) in per_second.iter().enumerate() {
        eprintln!("cutloop[{i:2}s]: {shown:3} displayed, {dropped:3} dropped");
    }
    row(
        path,
        "cutloop_displayed",
        "count",
        &rates,
        &format!(
            "per second; backend {backend}, {shown} displayed, {dropped} dropped, \
             {resyncs} resyncs in {elapsed:.1}s, {regions} x {cut_s}s cuts of {keep_s}s clips"
        ),
    );
    row(
        path,
        "cutloop_restarts",
        "count",
        &[session.restarts() as f64],
        &format!(
            "backend {backend}, {resyncs} late-picture resyncs, {underruns} underruns"
        ),
    );
}

// ---------------------------------------------------------------- waveform

/// What a lane waits for before it has an envelope: one whole
/// [`engine::waveform::peaks`] of the file's first audio stream, at the app's
/// own ten buckets a second. Cold once, then warm -- the mkv sidecar index is
/// written on the first open, so the two are different questions.
fn waveform_bench(path: &Path) {
    const BPS: u32 = 10;
    evict(path);
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    let mut note = String::new();
    for i in 0..3 {
        let t = Instant::now();
        match engine::waveform::peaks(path, 0, BPS) {
            Ok(peaks) => {
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                if i == 0 { cold.push(ms) } else { warm.push(ms) }
                if note.is_empty() {
                    note = format!("{} buckets", peaks.map_or(0, |p| p.len()));
                }
            }
            Err(e) => {
                row(path, "waveform_cold", "ms", &[], &format!("FAIL({e})"));
                return;
            }
        }
    }
    row(path, "waveform_cold", "ms", &cold, &note);
    row(path, "waveform_warm", "ms", &warm, &note);
}

// ------------------------------------------------------------------- scrub

/// Twenty scrub steps: the playhead dragged across a quarter of the film, one
/// frame waited for at each stop. Two numbers come out -- the per-step
/// first-frame latency, and the peak thread count, because every abandoned
/// step leaves a decode worker inside an uncancellable open.
///
/// Then the same twenty steps *without* waiting, which is the drag a user
/// really makes: no latency to report (no frame is waited for), only the
/// thread peak, which is where the hundreds came from.
fn scrub_bench(path: &Path) {
    evict(path);
    let mut session = match engine::PlaybackSession::open(path) {
        Ok(session) => session,
        Err(e) => {
            row(path, "scrub_ttff", "ms", &[], &format!("FAIL({e})"));
            return;
        }
    };
    let duration = session.timeline_duration();
    let start = duration * 0.25;
    let step = (duration * 0.25) / 20.0;
    let mut samples = Vec::new();
    let mut missed = Vec::new();
    let mut peak = threads();
    for i in 0..20 {
        match seek_ttff(&mut session, start + step * f64::from(i), &mut peak) {
            Ok(ms) => samples.push(ms),
            Err(why) => missed.push(why),
        }
    }
    let note = if missed.is_empty() {
        format!("backend {}", session.decode_backend().label())
    } else {
        format!(
            "backend {}, {}/20 NO-FRAME({})",
            session.decode_backend().label(),
            missed.len(),
            missed[0]
        )
    };
    row(path, "scrub_ttff", "ms", &samples, &note);
    row(path, "scrub_threads_peak", "threads", &[peak as f64], "waited per step");

    // The storm: seek, do not wait, seek again.
    let mut storm_peak = threads();
    for i in 0..20 {
        session.seek(start + step * f64::from(i));
        storm_peak = storm_peak.max(threads());
    }
    row(
        path,
        "scrub_storm_threads_peak",
        "threads",
        &[storm_peak as f64],
        "20 seeks, no wait",
    );
}

// ------------------------------------------------------------------ export

/// Export throughput over a fixed 60 s span, one encoder seat per run.
///
/// The span is video only: what is being compared is the picture encoder, and
/// a sound track that refuses (a 7.1 Opus film does, at the app's door) would
/// otherwise take the row with it. The span starts a tenth of the way in --
/// not on the black of a title card, which encodes at a rate no film sustains.
///
/// The rate is frames the worker reported written over wall seconds, so a run
/// stopped at the cap still reports a rate; the row says `CAPPED` when it was.
fn export_bench(path: &Path, seat: &str, out_dir: &Path) {
    let (format, pick) = match seat {
        "h264sw" => (Format::Mp4, EncoderSeat::Software),
        "h264hw" => (Format::Mp4, EncoderSeat::Auto),
        "av1" => (Format::Av1, EncoderSeat::Software),
        // `hevc` keeps meaning the software intra seat, so a row measured
        // against an older baseline still compares with the one beside it; the
        // GPU seat is the new name.
        "hevc" => (Format::Hevc, EncoderSeat::Software),
        "hevchw" => (Format::Hevc, EncoderSeat::Auto),
        _ => usage(),
    };
    // The sound is opt-in and named in the row, so an `_av` number is never
    // compared against a picture-only one: what it measures is the *export* --
    // audio pass, mux and all -- which is the only shape of the number a
    // "twice real time" claim may be made from.
    let with_audio = std::env::var_os("BENCH_EXPORT_AUDIO").is_some();
    let metric = format!("export_fps_{seat}{}", if with_audio { "_av" } else { "" });
    let (meta, _) = match engine::demux::Demuxer::open(path) {
        Ok(opened) => opened,
        Err(e) => {
            row(path, &metric, "fps", &[], &format!("FAIL(open: {e})"));
            return;
        }
    };
    let span = ((meta.frame_rate * env_f64("BENCH_EXPORT_SPAN", 60.0)) as u32)
        .min(meta.frame_count.max(1));
    let in_frame = (meta.frame_count / 10).min(meta.frame_count.saturating_sub(span));
    let clip = Clip {
        fade_in: 0,
        fade_out: 0,
        start: 0,
        in_frame,
        out_frame: in_frame + span,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: engine::scale::FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    // The sound, where it was asked for, is the same clip on an audio lane:
    // the very window the picture covers, so the two passes measure one export.
    let mut lanes = vec![(LaneKind::Video, vec![clip.clone()])];
    if with_audio {
        lanes.push((LaneKind::Audio, vec![clip]));
    }
    let stream = env_f64("BENCH_EXPORT_ASTREAM", 0.0) as usize;
    let project = match Project::from_parts(
        vec![Source::new(path, stream)],
        lanes,
        Vec::new(),
        Vec::new(),
    ) {
        Ok(project) => project,
        Err(e) => {
            row(path, &metric, "fps", &[], &format!("FAIL(project: {e})"));
            return;
        }
    };
    let settings = ExportSettings {
        seat: pick,
        format,
        ..ExportSettings::default()
    };
    let out = out_dir.join(format!("bench_{seat}.{}", format.ext()));
    let cap = Duration::from_secs_f64(env_f64("BENCH_EXPORT_CAP", 300.0));
    let reps = env_f64("BENCH_EXPORT_REPS", 5.0) as usize;

    let mut samples = Vec::new();
    let mut notes = Vec::new();
    let budget = Instant::now();
    for _ in 0..reps.max(1) {
        let handle = engine::export::start(project.clone(), meta, &out, &settings);
        let t = Instant::now();
        let mut capped = false;
        // The highest the bar was ever seen at, not wherever it happens to
        // stand when the clock stops: an export publishes one bar from two
        // stages, and a capped run's rate is derived from this number. Taking
        // the maximum over the poll loop means a single unlucky read cannot
        // under-report the work that was really done.
        let mut peak = 0.0f64;
        while !handle.is_finished() {
            peak = peak.max(f64::from(handle.progress()));
            if t.elapsed() >= cap {
                capped = true;
                handle.cancel();
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // A cancel is answered at the worker's next checkpoint; the rate is
        // read from the progress at the moment the clock stopped either way.
        let progress = peak.max(f64::from(handle.progress()));
        let elapsed = t.elapsed().as_secs_f64();
        let done = Instant::now() + Duration::from_secs(60);
        while !handle.is_finished() && Instant::now() < done {
            std::thread::sleep(Duration::from_millis(100));
        }
        let seats = handle.encoders().unwrap_or_else(|| "?".into());
        let outcome = handle.result();
        // A throughput run leaves nothing behind; `BENCH_KEEP=1` keeps the last
        // one, which is how a seat is checked for *decodability* rather than
        // speed (`ffprobe -count_frames` on the file it wrote).
        if std::env::var_os("BENCH_KEEP").is_none() {
            let _ = std::fs::remove_file(&out);
        }
        if capped {
            samples.push(progress * f64::from(span) / elapsed);
            notes.push(format!("CAPPED at {:.0}s, {:.0}% done, {seats}", elapsed, progress * 100.0));
            break;
        }
        match outcome {
            Some(Ok(())) => {
                samples.push(f64::from(span) / elapsed);
                notes.push(seats);
            }
            Some(Err(e)) => {
                notes.push(format!("FAIL({e})"));
                break;
            }
            // Finished without an outcome cannot happen (the worker settles
            // before it flips the flag); a wait that ran out of its own minute
            // does, and it is the same "no number" either way.
            None => {
                notes.push("FAIL(no outcome inside 60 s of the cap)".into());
                break;
            }
        }
        if budget.elapsed() >= cap {
            notes.push(format!("{} rep(s) inside the {:.0}s budget", samples.len(), cap.as_secs_f64()));
            break;
        }
    }
    notes.dedup();
    row(path, &metric, "fps", &samples, &notes.join("; "));
}
