//! What the workers hand back: the progress lines, the file named on the
//! command line, the scans, the codecs offered, and the subtitles found.

use super::*;
use crate::player::library::{
    auto_proxies_pref_path, load_auto_proxies_pref, save_auto_proxies_pref,
};
use crate::subs::{
    SUB_SIZE_RANGE, load_subtitle_style, save_subtitle_style, sub_line_h_for, subtitle_style_path,
};
use crate::ui::preview::{bgra_to_rgba, screenshot_path};

/// The import line's own state machine, driven the way a repaint drives
/// it: the worker writes a stage, the poll notices it changed and restarts
/// the stall clock, and the line's words change only when a stage has
/// actually stood still. The whole point is that a stuck read reads as a
/// stuck read and never as a frozen window.
#[test]
fn an_import_line_says_a_stage_has_stopped_moving_and_only_then() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
    let stage = Arc::new(AtomicU8::new(ImportStage::Header as u8));
    let started = Instant::now() - Duration::from_secs(9);
    let mut import = Import {
        path: PathBuf::from("/films/A Film.mkv"),
        started,
        stage: Arc::clone(&stage),
        seen: ImportStage::Header,
        // As if the header had been running for those nine seconds.
        since: started,
        cancelled: Arc::default(),
    };
    // Nine seconds inside one stage: past the wait a person tolerates, so
    // the line stops pretending it is a progress line.
    let since = import.poll();
    assert!(since > IMPORT_STALL, "{since}");
    let stalled = import_line("A Film.mkv", import.seen, 9., since, 0, false);
    assert!(stalled.contains("still reading the header"), "{stalled}");
    assert!(stalled.contains("not frozen"), "{stalled}");
    assert!(stalled.contains("0:09 elapsed"), "{stalled}");
    // The worker moves on: the stall clock restarts even though the elapsed
    // one does not, which is the distinction the two clocks exist for.
    stage.store(ImportStage::Subtitles as u8, Relaxed);
    let since = import.poll();
    assert_eq!(import.seen, ImportStage::Subtitles);
    assert!(since < IMPORT_STALL, "{since}");
    let moving = import_line("A Film.mkv", import.seen, 9., since, 2, false);
    assert!(
        moving.starts_with("IMPORTING A Film.mkv · reading the subtitle tracks"),
        "{moving}"
    );
    assert!(!moving.contains("still"), "{moving}");
    // ...and what is behind it in the queue, which is what a drop of three
    // files or an argv of three files leaves waiting.
    assert!(moving.ends_with("· 2 more waiting"), "{moving}");
    assert!(!stalled.ends_with("waiting"), "an empty queue says nothing");
    // A stage that does not move is not the same fact as a stage that came
    // back to the same value: the poll only restarts on a change.
    let held = import.since;
    import.poll();
    assert_eq!(import.since, held, "an unchanged stage must not reset it");
}

/// The launch, from argv to the window being up: nothing named on the
/// command line is read before there is a window, so all a launch does is
/// sort argv into the file that becomes the timeline and the queue that is
/// read through -- and a run with no arguments has neither, exactly as it
/// had none before.
#[test]
fn a_launch_queues_the_file_it_names_instead_of_opening_it() {
    let film = PathBuf::from("/films/Dune.mkv");
    let extra = PathBuf::from("/films/Titles.mov");
    let (arg, queue) = launch_queue([film.clone(), extra.clone()].into_iter());
    assert_eq!(arg.as_deref(), Some(film.as_path()), "argv[1] is the open");
    // In the order they were named, the timeline's file first: six header
    // walks racing over one disk finish no sooner than six in a row, and
    // the one the person is waiting to watch goes first.
    assert_eq!(queue, [film, extra], "argv, in arrival order");
    // ...and no argument at all is still the empty window: nothing to open
    // and nothing to read.
    let (arg, queue) = launch_queue(std::iter::empty());
    assert_eq!(arg, None);
    assert!(queue.is_empty(), "no argv, nothing queued");
}

/// Which door a queued file goes through. One queue carries the file argv
/// named and every import behind it, so this fork -- made when the worker
/// starts and carried to the landing as a [`Landed`] -- is the whole state
/// machine: the named file becomes the timeline (a `.edith` restoring a
/// whole one), everything else joins the library, including the very same
/// film dropped again once the open has landed.
#[test]
fn the_file_argv_named_lands_as_the_timeline_and_everything_behind_it_as_imports() {
    let film = PathBuf::from("/films/Dune.mkv");
    let extra = PathBuf::from("/films/Titles.mov");
    let project = PathBuf::from("/films/Dune.edith");
    assert_eq!(arrival(Some(&film), &film), Landing::Open);
    assert_eq!(arrival(Some(&project), &project), Landing::Project);
    assert_eq!(arrival(Some(&film), &extra), Landing::Import);
    // Cleared as it lands: a drop of the film already on the timeline is an
    // import, which is what a drop has always been.
    assert_eq!(arrival(None, &film), Landing::Import);
    // ...except a `.edith`, which is a whole timeline and has nothing to be
    // imported into: dropped or picked, it lands where argv's does, and its
    // open is the worker's like every other.
    assert_eq!(arrival(None, &project), Landing::Project);
    assert_eq!(arrival(Some(&film), &project), Landing::Project);
}

/// What the window says while the file argv named is being read: the file's
/// name, the read that is running, and a clock proving the window is
/// answering -- in the *opening* wording, because the one person who typed
/// that name is not importing anything.
#[test]
fn the_named_file_is_read_under_an_opening_line_not_an_importing_one() {
    let opening = import_line("Dune.mkv", ImportStage::Header, 0.4, 0.4, 1, true);
    assert!(
        opening.starts_with("OPENING Dune.mkv · reading the header"),
        "{opening}"
    );
    assert!(opening.ends_with("· 1 more waiting"), "{opening}");
    // Twelve seconds into a cold 25 GB header walk, which is the whole
    // reason the window is up: it says the read is still moving through the
    // same stage and that this is not a freeze.
    let stalled = import_line("Dune.mkv", ImportStage::Header, 12., 12., 0, true);
    assert!(stalled.starts_with("OPENING Dune.mkv · still"), "{stalled}");
    assert!(stalled.contains("not frozen"), "{stalled}");
    assert!(stalled.contains("0:12 elapsed"), "{stalled}");
    // The files behind it are imports and still say so.
    let import = import_line("Titles.mov", ImportStage::Header, 0.4, 0.4, 0, false);
    assert!(import.starts_with("IMPORTING Titles.mov"), "{import}");
}

/// One gesture over the real gate, with the plumbing around it spelled out:
/// a write reseeks (the worker owes a frame again), a landed frame clears
/// that and flushes what is held, and the release flushes whatever the
/// worker is doing. Forty snapped steps and four frames delivered must cost
/// five writes -- the press's and one per frame -- and not forty, which is
/// the 22-30 s freeze this exists to remove.
#[test]
fn a_bar_wide_sweep_writes_once_per_frame_delivered() {
    let mut stash: Option<i32> = None;
    let mut written = Vec::new();
    let mut busy = false;
    for step in 0..40 {
        if let Some(value) = stash_or_write(&mut stash, step, step == 0, busy) {
            // What `write_color` does: the write supersedes the stash and
            // reseeks, so the worker owes a frame from here.
            stash = None;
            written.push(value);
            busy = true;
        }
        // A frame lands every tenth sample: `pump` clears the seek and the
        // render flushes what the drag held back.
        if step % 10 == 9 {
            busy = false;
            if let Some(value) = stash.take() {
                written.push(value);
                busy = true;
            }
        }
    }
    // The release, whatever the worker is doing.
    written.extend(stash.take());
    assert_eq!(
        written,
        vec![0, 9, 19, 29, 39],
        "one write per frame landed"
    );
}

/// The one value a gesture may never lose: where the hand let go. The
/// release samples into a busy worker -- so the sample is held -- and the
/// flush behind it is what writes it.
#[test]
fn a_release_lands_the_value_the_hand_let_go_on() {
    let mut stash = None;
    assert_eq!(stash_or_write(&mut stash, 7, false, true), None);
    assert_eq!(stash_or_write(&mut stash, 11, false, true), None);
    assert_eq!(stash.take(), Some(11), "the release writes the last sample");
    // The press is never held: it is the undo step the gesture rolls back
    // to, and one taken a frame late is a snapshot of the wrong grade.
    assert_eq!(stash_or_write(&mut stash, 3, true, true), Some(3));
    assert_eq!(stash, None);
    // Nothing to hold when the worker is idle: the write goes straight out.
    assert_eq!(stash_or_write(&mut stash, 5, false, false), Some(5));
    assert_eq!(stash, None);
}

/// A seek says nothing until it has stood: an ordinary one is a flicker and
/// a cold read of a big file is the case worth words.
#[test]
fn a_seek_says_so_only_once_it_has_stood() {
    assert_eq!(seek_line(None), None, "no seek, no line");
    assert_eq!(seek_line(Some(Duration::from_millis(300))), None);
    let line = seek_line(Some(SEEK_STALL + Duration::from_secs(7))).expect("past the stall");
    assert!(line.contains("still opening the picture"), "{line}");
    assert!(line.contains("not frozen"), "{line}");
    assert!(line.contains("0:09 elapsed"), "{line}");
}

/// The silence card's own state machine, driven the way a repaint drives
/// it: a worker moves its mark, the poll notices and restarts the stall
/// clock, and the line says a read has stopped only when it actually has.
/// The card is up through all of it -- that is the whole change, since the
/// same decode used to run on the render thread and hold the frame for
/// fifty-one seconds on a 25 GB film.
#[test]
fn a_silence_card_is_up_while_its_scan_runs_and_says_where_it_has_got_to() {
    use std::sync::Arc;
    use std::sync::atomic::Ordering::Relaxed;
    let progress = Arc::new(engine::silence::Progress::default());
    // Two hours and eight minutes, as the header claims it.
    progress.total.store(7680, Relaxed);
    let started = Instant::now() - Duration::from_secs(9);
    let mut scan = SilenceScan {
        key: (PathBuf::from("/films/A Film.mkv"), 0, 0, 192_000),
        started,
        progress: Arc::clone(&progress),
        seen: 0,
        since: started,
    };
    // Nine seconds and the mark has not moved: past the wait a person
    // tolerates, so the line stops pretending it is a progress line.
    let since = scan.poll();
    assert!(since > IMPORT_STALL, "{since}");
    let stalled = silence_line(0., 768., 9., since);
    assert!(stalled.contains("still reading the sound"), "{stalled}");
    assert!(stalled.contains("not frozen"), "{stalled}");
    assert!(stalled.contains("0:00 of ~12:48 scanned"), "{stalled}");
    assert!(stalled.contains("0:09 elapsed"), "{stalled}");
    // The worker reports, and the stall clock restarts even though the
    // elapsed one does not -- the two clocks' whole distinction.
    let mut last = 0;
    for deci in [83, 1_140, 4_002] {
        progress.scanned.store(deci, Relaxed);
        let since = scan.poll();
        assert!(since < IMPORT_STALL, "{since}");
        assert!(scan.seen > last, "{} after {last}", scan.seen);
        last = scan.seen;
    }
    let moving = silence_line(scan.seen as f32 / 10., 768., 9., 0.2);
    assert_eq!(moving, "SCANNING · 6:40 of ~12:48 scanned · 0:09 elapsed");
    assert!(!moving.contains("still"), "{moving}");
    // A header that does not say how long the track is says nothing rather
    // than guessing at it.
    let unknown = silence_line(60., 0., 61., 0.2);
    assert_eq!(unknown, "SCANNING · 1:00 scanned · 1:01 elapsed");
    // A mark that comes back the same is not the same fact as one that
    // moved: the poll only restarts on a change.
    let held = scan.since;
    scan.poll();
    assert_eq!(scan.since, held, "an unchanged mark must not reset it");
}

/// The cache is per scanned stretch, which is what stops two films
/// thrashing each other's fifty seconds: A, then B, then A again is *one*
/// decode of A. And a stretch already being read is waited for rather than
/// read twice -- both halves of an A/V take name the same file and the same
/// frames. A *trimmed* A is not A: it is a shorter read of its own, which
/// is the whole point of scanning the clip instead of the file.
#[test]
fn a_second_film_does_not_cost_the_first_one_its_levels() {
    let (a, b) = (
        (PathBuf::from("/films/a.mkv"), 0, 0, 3000),
        (PathBuf::from("/films/b.mkv"), 0, 0, 3000),
    );
    let half = (PathBuf::from("/films/a.mkv"), 0, 0, 1500);
    let mut cache: std::collections::HashMap<ScanKey, ()> = std::collections::HashMap::new();
    let mut started = Vec::new();
    // What a card open does, three times over, with the worker landing
    // between each: plan, and start what the plan says to start.
    for key in [&a, &b, &a] {
        match scan_plan(cache.contains_key(key), None, key) {
            ScanPlan::Start => {
                started.push(key.clone());
                cache.insert(key.clone(), ());
            }
            ScanPlan::Marks => {}
            ScanPlan::Wait => unreachable!("nothing is running"),
        }
    }
    assert_eq!(started, vec![a.clone(), b.clone()], "A was decoded twice");
    // The single-slot cache this replaced would have evicted A when B
    // landed; both are held.
    assert!(cache.contains_key(&a) && cache.contains_key(&b));
    // A scan in flight on the same source is joined, not restarted -- and
    // one on another source is not waited for.
    assert_eq!(scan_plan(false, Some(&a), &a), ScanPlan::Wait);
    assert_eq!(scan_plan(false, Some(&b), &a), ScanPlan::Start);
    // Levels in hand beat a worker either way: the marks are arithmetic.
    assert_eq!(scan_plan(true, Some(&a), &a), ScanPlan::Marks);
    // The same file cut in half is a read of its own: neither A's levels
    // nor A's worker answers for it.
    assert_eq!(
        scan_plan(cache.contains_key(&half), None, &half),
        ScanPlan::Start
    );
    assert_eq!(scan_plan(false, Some(&a), &half), ScanPlan::Start);
    // ...and the seconds asked for are the clip's, at the project's rate.
    assert_eq!(source_secs(&half, 30.), (0., 50.));
    assert_eq!(
        source_secs(&(half.0.clone(), 0, 900, 1500), 30.),
        (30., 50.)
    );
    // A rate that is not one reads the file rather than nothing.
    assert_eq!(source_secs(&half, 0.), (0., f64::INFINITY));
}

/// The background scan started at import ([`Player::cache_media`]) lands its
/// whole-file levels under [`full_scan_key`], and that entry alone is what
/// makes the card's own cache check answer "already read" for *every* clip
/// cut from that source -- without the per-clip decode `start_silence_scan`
/// used to be the only way to get one. The whole point of moving the scan to
/// import: the card must find it warm.
#[test]
fn a_warm_whole_source_scan_answers_the_card_for_every_clip_cut_from_it() {
    use std::sync::Arc;
    let mut levels: std::collections::HashMap<ScanKey, Arc<Vec<f32>>> =
        std::collections::HashMap::new();
    let clip_a = (PathBuf::from("/films/a.mkv"), 0, 300, 900);
    let clip_b = (PathBuf::from("/films/a.mkv"), 0, 1_200, 1_800);
    // Nothing yet -- neither clip's own read nor a background one.
    assert!(!silence_cached(&levels, &clip_a));
    assert!(!silence_cached(&levels, &clip_b));
    // The background scan lands, once, before either half of the take was
    // ever opened on the card.
    levels.insert(full_scan_key(&clip_a.0, clip_a.1), Arc::new(vec![0.; 100]));
    assert!(
        silence_cached(&levels, &clip_a) && silence_cached(&levels, &clip_b),
        "one whole-source entry answers for both halves of the take"
    );
    // A clip cut from a *different* file is not answered by it -- the
    // sentinel is per (path, stream), exactly like every other scan key.
    let other_file = (PathBuf::from("/films/b.mkv"), 0, 300, 900);
    assert!(!silence_cached(&levels, &other_file));
    // A clip's own exact read still counts too, same as before this existed.
    levels.insert(clip_a.clone(), Arc::new(vec![-40.; 10]));
    assert!(silence_cached(&levels, &clip_a));
}

/// What the card actually draws from a warm background scan: the same
/// dBFS-per-window numbers a direct read of the clip's own stretch would
/// have found, because window `k` of the whole file and window `k` of a read
/// that started at second 0 name the same slice of the same source
/// ([`slice_whole_levels`]'s whole contract). No decode in this test at
/// all -- the numbers are synthetic and distinct per window so a wrong
/// offset shows up as a wrong value, not a coincidentally-right one.
#[test]
fn a_clips_slice_of_the_whole_scan_is_the_windows_its_own_read_would_have_found() {
    // 50 fps: one timeline frame is exactly one silence window
    // (`engine::silence::WINDOWS_PER_SEC` is 50), so the arithmetic is exact
    // and the test does not have to fight rounding to prove the offset.
    let fps = 50.;
    let whole: Vec<f32> = (0..40).map(|i| i as f32).collect();
    let slice = slice_whole_levels(&whole, fps, 10, 20);
    assert_eq!(slice, (10..20).map(|i| i as f32).collect::<Vec<f32>>());
    // The clip's own in point is where its slice starts, not the file's:
    // a take an hour into the film reads the same as one at its head.
    assert_eq!(
        slice_whole_levels(&whole, fps, 0, 5),
        vec![0., 1., 2., 3., 4.]
    );
    // Past what the background scan has read so far (still running, or
    // cancelled) is not padded with zeroes -- the caller is told nothing is
    // there yet, same as [`SilenceScan`]'s own cancel leaves a prefix.
    assert_eq!(
        slice_whole_levels(&whole, fps, 35, 60),
        vec![35., 36., 37., 38., 39.]
    );
    assert_eq!(slice_whole_levels(&whole, fps, 45, 60), Vec::<f32>::new());
    // A rate that is not a rate reads nothing, the same refusal
    // `source_secs` makes for a background scan's own range.
    assert_eq!(slice_whole_levels(&whole, 0., 0, 10), Vec::<f32>::new());
}

/// The import door's split, which is what keeps the render thread out of a
/// header walk: what the worker probed and what the landing registers add up
/// to exactly the rows, lengths and refusals the in-place import lands.
#[test]
fn probing_ahead_lands_exactly_what_importing_in_place_lands() {
    use std::sync::atomic::AtomicU8;
    let stage = AtomicU8::new(ImportStage::Header as u8);
    let plain = {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
        session.set_gain(0.0);
        session.import(&asset("test_av2.mp4")).expect("av2 matches");
        (
            session.sources().to_vec(),
            session.file_frames(&asset("test_av2.mp4")),
            session.timeline_duration(),
        )
    };
    let split = {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
        session.set_gain(0.0);
        let (subs, probe) = read_parts(&asset("test_av2.mp4"), &stage, Some(session.import_gate()));
        // This mp4 is walked now (an mp4 can carry `tx3g`) and has no
        // subtitle track in it, so the worker hands over an empty list --
        // the same thing the import lands with.
        assert!(subs.expect("an mp4 is readable").is_empty());
        let probe = probe
            .expect("a container is probed on the worker")
            .expect("av2 matches");
        session
            .import_probed(&asset("test_av2.mp4"), probe)
            .expect("av2 matches");
        (
            session.sources().to_vec(),
            session.file_frames(&asset("test_av2.mp4")),
            session.timeline_duration(),
        )
    };
    assert_eq!(plain, split);
    // ...and it leaves the stage where the line can read it: a worker that
    // never announced its second read would show one that never ends.
    assert_eq!(
        ImportStage::from_u8(stage.load(std::sync::atomic::Ordering::Relaxed)),
        ImportStage::Subtitles
    );
    // A refusal is the engine's refusal, and now it is *the* refusal: the
    // walk happens once, on the worker, so what the probe says is what the
    // notice shows -- worded exactly as importing in place worded it.
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    session.set_gain(0.0);
    let (_, probe) = read_parts(
        &asset("test_25fps.mp4"),
        &stage,
        Some(session.import_gate()),
    );
    let on_the_worker = probe
        .expect("a container is probed")
        .map(|_| ())
        .map_err(|e| e.to_string());
    assert_eq!(
        on_the_worker,
        session
            .import(&asset("test_25fps.mp4"))
            .map(|_| ())
            .map_err(|e| e.to_string())
    );
    // A timeline that moved while the worker read is not one the probe was
    // decided against, so the probe is not trusted -- the import is simply
    // taken the slow way and lands the same row.
    let mut moved = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    moved.set_gain(0.0);
    let (_, probe) = read_parts(&asset("test_av2.mp4"), &stage, Some(moved.import_gate()));
    moved
        .import(&asset("test_still.png"))
        .expect("a still joins");
    let stale = probe.expect("a container is probed").expect("av2 matches");
    assert_eq!(
        moved.import_probed(&asset("test_av2.mp4"), stale).ok(),
        Some(2),
        "a stale probe still lands the file, by the door that re-reads it"
    );
    // A path nothing can read is refused by the worker now, in the engine's
    // own words: nothing walks it a second time to re-word it, and neither
    // half is a panic.
    let (subs, probe) = read_parts(
        std::path::Path::new("/no/such/film.mkv"),
        &stage,
        Some(session.import_gate()),
    );
    assert!(subs.is_err(), "the cue walk comes back as a refusal");
    assert!(
        probe.expect("a container is probed").is_err(),
        "and so does the container walk"
    );
}

/// The import door's two halves, split across the worker hop: what
/// [`read_ahead`] walks is exactly what the render thread would have walked,
/// and pushing it says what walking it in place used to say.
#[test]
fn an_imports_subtitles_are_read_by_the_worker_and_only_pushed_here() {
    use std::sync::atomic::AtomicU8;
    let stage = AtomicU8::new(ImportStage::Header as u8);
    let film = asset("test_subs.mkv");
    // The in-place walk (`subtitle_notice`) and the split one land the same
    // tracks and the same tail on the same timeline.
    let mut in_place = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    in_place.set_gain(0.0);
    let said = subtitle_notice(&mut in_place, &film);
    let mut split = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    split.set_gain(0.0);
    let (walked, _) = read_parts(&film, &stage, Some(split.import_gate()));
    let walked = walked.expect("the mkv is readable");
    assert_eq!(subtitle_tail(&mut split, Ok(walked)), said);
    assert_eq!(split.subtitles().len(), in_place.subtitles().len());
    // The same file twice is still one row, and still says so: the dedupe
    // lives in the push, which is the half that stayed here.
    let (again, _) = read_parts(&film, &stage, Some(split.import_gate()));
    let again = again.expect("the mkv is readable");
    assert_eq!(subtitle_tail(&mut split, Ok(again)), None);
    // A standalone `.srt` is walked by the same worker: an import of one is
    // not a door that reads on the render thread either.
    let srt = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/data/test_subs.srt")
        .canonicalize()
        .expect("the subtitle fixture");
    assert_eq!(
        read_parts(&srt, &stage, Some(split.import_gate()))
            .0
            .expect("the srt is readable")
            .len(),
        1
    );
    // ...and a refusal is worded as a tail, never as a failed import.
    let unread: Subs = Err("nothing to read".into());
    assert_eq!(
        subtitle_tail(&mut split, unread),
        Some(" — SUBTITLES UNREAD: nothing to read".to_string())
    );
}

#[test]
fn every_codec_row_is_offered_or_says_why_not() {
    // One row per codec, and the boxes it can go in are the container row's
    // business: seven rows that pick, one that says why it cannot. A codec
    // twice over (AV1 · MKV beside AV1 · MP4) was two rows asking the same
    // question, and five picture rows above the fold is what the card was
    // called unfriendly for.
    let offered: Vec<&[Format]> = FORMATS
        .iter()
        .map(|&(row, ..)| row)
        .filter(|row| !row.is_empty())
        .collect();
    assert_eq!(
        offered,
        vec![
            &[Format::Mp4][..],
            &[Format::Av1, Format::Av1Mp4][..],
            &[Format::Hevc, Format::HevcMp4][..],
            &[Format::Wav][..],
            &[Format::Flac][..],
            &[Format::Mp3][..],
            &[Format::Ogg][..],
        ]
    );
    // Every format the engine writes is on exactly one row: one the card
    // cannot reach is one nobody can pick.
    for format in [
        Format::Mp4,
        Format::Av1,
        Format::Av1Mp4,
        Format::Hevc,
        Format::HevcMp4,
        Format::Wav,
        Format::Flac,
        Format::Mp3,
        Format::Ogg,
    ] {
        assert_eq!(
            FORMATS
                .iter()
                .filter(|(row, ..)| row.contains(&format))
                .count(),
            1,
            "{format:?}"
        );
    }
    // The boxes the container row offers, and what the extension of each is
    // -- the destination follows the format, so these are what a file gets
    // named with.
    assert_eq!(containers(Format::Av1), [Format::Av1, Format::Av1Mp4]);
    assert_eq!(containers(Format::HevcMp4), [Format::Hevc, Format::HevcMp4]);
    assert_eq!(containers(Format::Mp3), [Format::Mp3]);
    assert_eq!(Format::Av1.ext(), "mkv");
    assert_eq!(Format::Av1Mp4.ext(), "mp4");
    assert_eq!(Format::Hevc.ext(), "mkv");
    assert_eq!(Format::HevcMp4.ext(), "mp4");
    // The container key walks the row and wraps, and does nothing at all
    // where there is only one box -- a stroke must not invent a choice the
    // card is not offering.
    assert_eq!(next_container(Format::Av1), Format::Av1Mp4);
    assert_eq!(next_container(Format::Av1Mp4), Format::Av1);
    assert_eq!(next_container(Format::Hevc), Format::HevcMp4);
    assert_eq!(next_container(Format::Mp4), Format::Mp4);
    assert_eq!(next_container(Format::Wav), Format::Wav);
    for (row, stroke, label, detail) in FORMATS {
        assert!(!label.is_empty(), "a row with no name");
        // A refused row is a row with a reason, never a hidden one: an
        // empty detail column would read as an oversight.
        assert!(!detail.is_empty(), "{label} says nothing");
        // Every row that can be picked has a key of its own, and no row
        // that cannot has one: the card is drivable without a pointer.
        assert_eq!(!row.is_empty(), !stroke.is_empty(), "{label}");
        if let Some(&first) = row.first() {
            assert_eq!(
                format_key(stroke, first),
                Some(first),
                "{label} keys to itself"
            );
            assert!(
                stroke.parse::<u32>().is_err(),
                "{label} takes a digit the bitrate needs"
            );
            // Every box on one row is the same codec, so the quality rows
            // do not change meaning when the container does.
            assert!(row.iter().all(|f| f.has_video() == first.has_video()));
            assert!(
                row.iter()
                    .all(|f| bitrate_refusal(*f) == bitrate_refusal(first))
            );
        }
    }
    // No two rows share a key, and none of them is a stroke the card already
    // answers to itself -- an ambiguous key is a key that picks the wrong
    // thing on a card that has no other input.
    let keys: Vec<&str> = FORMATS
        .iter()
        .map(|&(_, stroke, ..)| stroke)
        .filter(|stroke| !stroke.is_empty())
        .collect();
    for (i, key) in keys.iter().enumerate() {
        assert!(!keys[i + 1..].contains(key), "{key} picks two rows");
        assert!(
            !["c", "q", "d", "g", "r", "enter", "backspace", ESCAPE].contains(key),
            "{key} is already the card's own"
        );
    }
    assert_eq!(
        format_key("a", Format::Mp4),
        Some(Format::Av1Mp4),
        "the box already chosen is kept"
    );
    assert_eq!(
        format_key("a", Format::Wav),
        Some(Format::Av1),
        "and a codec with no such box takes its first"
    );
    assert_eq!(format_key("h", Format::Av1Mp4), Some(Format::HevcMp4));
    assert_eq!(format_key("p", Format::Mp4), Some(Format::Mp3));
    assert_eq!(
        format_key("m", Format::Mp3),
        Some(Format::Mp4),
        "not MP3, which is p"
    );
    assert_eq!(
        format_key("x", Format::Mp4),
        None,
        "a stroke no row carries"
    );
    assert_eq!(format_key("o", Format::Mp4), Some(Format::Ogg));
    // The one codec left that this program reads and cannot write is a row
    // of its own, refused by name rather than absent: VP9, because AV1 is
    // the row that replaced it. Its reason travels with it, in the row or in
    // the footer line that collects them -- either way it is on screen
    // without a click. OGG was the other one until `rusty_vorbis` gave this
    // project an encoder, and the row that says so is the row above.
    let (row, _, _, detail) = FORMATS
        .into_iter()
        .find(|(_, _, name, _)| *name == "VP9")
        .expect("VP9 has a row");
    assert!(row.is_empty(), "VP9 is not offered");
    assert!(detail.contains("replaces it"), "VP9: {detail}");
    let (row, _, _, detail) = FORMATS
        .into_iter()
        .find(|(_, _, name, _)| *name == "OGG")
        .expect("OGG has a row");
    assert_eq!(row, [Format::Ogg], "OGG is a row that picks now");
    assert!(
        detail.contains("rusty_vorbis"),
        "the row names the encoder like every other live one: {detail}"
    );
    // Both AV1 boxes say they carry sound: the file used to be picture only,
    // and a line that still said so would be the lie a user plays the file
    // to find out about. HEVC says intra-only before anyone waits on one --
    // a file several times the size, which the disk would otherwise say.
    for format in [Format::Av1, Format::Av1Mp4] {
        assert!(format_line(format).starts_with("AV1 · "));
    }
    for format in [Format::Hevc, Format::HevcMp4] {
        assert!(format_line(format).starts_with("HEVC intra · "));
    }
    // The head names the box every format goes in, which is what the
    // destination is then named after.
    for format in [
        Format::Mp4,
        Format::Av1,
        Format::Av1Mp4,
        Format::Hevc,
        Format::HevcMp4,
    ] {
        assert!(
            format_line(format).contains(&format.ext().to_uppercase()),
            "{format:?}: {}",
            format_line(format)
        );
    }
    // ...and it stays inside the one line the card budgets for it: the
    // longest of them, with every field after it, against the 76 characters
    // that fit at `EXPORT_W`.
    let longest = summary_head(
        Format::Hevc,
        Some(((1920, 1080), 23.976)),
        "AAC · SW encode (rusty_aac)",
    );
    assert!(
        longest.chars().count() <= 76,
        "{longest} is {} long",
        longest.chars().count()
    );
    assert!(
        FORMATS
            .into_iter()
            .any(|(row, _, _, detail)| row.contains(&Format::Hevc) && detail.contains("intra"))
    );
    // Only a picture encoder is given a bitrate, and the quality rows dim
    // with the reason for every format that is not one.
    for format in [
        Format::Mp4,
        Format::Av1,
        Format::Av1Mp4,
        Format::Hevc,
        Format::HevcMp4,
    ] {
        assert!(
            format.has_video() && bitrate_refusal(format).is_none(),
            "{format:?}"
        );
    }
    for format in [Format::Wav, Format::Flac, Format::Mp3] {
        assert!(!format.has_video());
        assert!(
            bitrate_refusal(format).is_some(),
            "{format:?} dims silently"
        );
    }
    assert!(bitrate_refusal(Format::Wav).unwrap().contains("lossless"));
    // MP3 has a rate and it is the *Sound* row's: the quality rows are the
    // picture's, and this refusal used to claim a fixed 256 kbps that the
    // Sound row can now change under it.
    assert!(bitrate_refusal(Format::Mp3).unwrap().contains("Sound row"));
    assert!(
        !format_line(Format::Mp3).contains("256"),
        "the summary states a rate the Sound row can change under it"
    );
    // The destination follows the format and keeps the stem, mp4 included.
    assert_eq!(
        retarget(std::path::Path::new("/a/take.export.mp4"), Format::Wav),
        std::path::Path::new("/a/take.export.wav")
    );
    assert_eq!(
        retarget(std::path::Path::new("/a/take.export.wav"), Format::Mp4),
        std::path::Path::new("/a/take.export.mp4")
    );
    assert!(format_line(Format::Flac).contains("lossless"));
}

/// The one line the card is answerable for: what it says is on screen before
/// the button is pressed, and every field of it is one `ffprobe` reads back
/// off the file that comes out.
#[test]
fn the_summary_states_the_file_before_it_is_written() {
    let head = summary_head(Format::Mp4, Some(((1920, 1080), 30.)), "AAC copy");
    for field in ["H.264", "MP4", "1920x1080", "30 fps", "AAC copy"] {
        assert!(head.contains(field), "{field} missing from {head}");
    }
    // The rate as a person writes it, and the ratio one spelled out rather
    // than rounded to a rate nothing is written at.
    assert_eq!(fps_label(30.), "30");
    assert_eq!(fps_label(24000. / 1001.), "23.976");
    assert_eq!(fps_label(29.97002997), "29.97");
    // A format with no picture states no size and no rate it does not write.
    let audio = summary_head(Format::Wav, Some(((1920, 1080), 30.)), "PCM · SW (hound)");
    assert!(
        !audio.contains("1920x1080") && !audio.contains("fps"),
        "{audio}"
    );
    assert!(audio.contains("PCM · SW (hound)"));
    // ...and one with no sound on the timeline says that, rather than
    // leaving the field out and reading as a file with sound in it.
    assert!(
        summary_head(Format::Mp4, Some(((640, 360), 25.)), "no sound to write")
            .contains("no sound to write")
    );
    // The tail: where it lands, about how big, and what will encode it --
    // never a guessed seat, and no seat at all for a format with no picture.
    let tail = summary_tail(
        Path::new("/a/take.export.mp4"),
        Some(45_000_000),
        Some("VA-API"),
        true,
    );
    assert!(tail.starts_with("take.export.mp4"), "{tail}");
    assert!(
        tail.contains("≈ 45 MB") && tail.contains("VA-API"),
        "{tail}"
    );
    assert!(summary_tail(Path::new("/a/x.mp4"), None, None, true).contains("encoder …"));
    assert!(!summary_tail(Path::new("/a/x.wav"), None, None, false).contains("encoder"));
    assert!(!summary_tail(Path::new("/a/x.wav"), None, None, false).contains("MB"));
    // 6 Mbps over a minute is 45 MB. `Auto` has no figure to estimate from
    // and an empty timeline no length: neither invents one.
    assert_eq!(estimated_bytes(Some(6_000_000), 60.), Some(45_000_000));
    assert_eq!(estimated_bytes(Some(2_000_000), 90.), Some(22_500_000));
    assert_eq!(estimated_bytes(None, 60.), None);
    assert_eq!(estimated_bytes(Some(6_000_000), 0.), None);
    assert_eq!(estimated_bytes(Some(0), 60.), None);
    // ...and a short one is not nothing. Three seconds at the floor
    // bitrate is 375 kB, which used to round to the "≈ 0 MB" this line
    // exists to never say -- the shortest export a frame can make is
    // still a real file with a real size.
    let short = estimated_bytes(Some(1_000_000), 3.).expect("a rate and a length");
    assert_eq!(size_label(short), "375 kB");
    let frame = summary_tail(
        Path::new("/a/x.mp4"),
        estimated_bytes(Some(1_000_000), 1. / 60.),
        None,
        true,
    );
    assert!(
        frame.contains("≈ 2 kB") && !frame.contains("0 MB") && !frame.contains("0 kB"),
        "{frame}"
    );
    // The boundary reads in the unit that can state it, either side.
    assert_eq!(size_label(999_600), "1 MB");
    assert_eq!(size_label(499_000), "499 kB");
    assert_eq!(size_label(1), "1 kB");
}

/// Which cue is on screen when: the whole of what the overlay decides, and
/// the one piece of it that is arithmetic rather than layout.
#[test]
fn a_cue_is_on_screen_from_its_start_until_the_moment_it_ends() {
    use engine::subtitle::Cue;
    let cue = |start_us, end_us, text: &str| Cue {
        start_us,
        end_us,
        text: text.to_string(),
        image: None,
    };
    // Two cues that hand over exactly, and one that overlaps the second --
    // a sign over a line of dialogue, which is two plates at one moment.
    let cues = [
        cue(500_000, 1_500_000, "first line"),
        cue(1_500_000, 2_500_000, "second line"),
        cue(2_000_000, 2_200_000, "a sign"),
    ];
    let at = |t: f64| -> Vec<&str> {
        cues_at(&cues, t)
            .into_iter()
            .map(|c| c.text.as_str())
            .collect()
    };
    // Before the first, between none of them: nothing, which is what makes
    // the overlay disappear rather than sit there empty.
    assert!(at(0.).is_empty());
    assert!(at(0.4).is_empty());
    assert!(at(3.).is_empty());
    assert_eq!(at(0.5), ["first line"], "on at its own start");
    assert_eq!(at(1.4), ["first line"]);
    // Half-open: the frame the first ends on is the second's, never both.
    assert_eq!(at(1.5), ["second line"]);
    assert_eq!(at(2.1), ["second line", "a sign"], "two at once stack");
    assert_eq!(at(2.2), ["second line"], "the sign is over");
    // ...and the end of the last one is the end of it.
    assert!(at(2.5).is_empty());
    // A negative time is before everything rather than a panic: the
    // playhead is clamped, but nothing here depends on that.
    assert!(at(-1.).is_empty());
}

/// The cue plate and the transient bars share the bottom edge of the picture,
/// and the plate is the one that moves: a notice drawn across the line being
/// read costs the reader both of them.
#[test]
fn a_cue_steps_over_whatever_bar_is_hanging_off_the_picture() {
    // Nothing hanging there, nothing moved: people know where the line sits,
    // and the no-notice position is the one they know, to the pixel.
    assert_eq!(sub_bottom(0.), SUB_BOTTOM);
    // A bar of any height is a bar the plate is clear of: its bottom edge is
    // above the bar's top edge, with the plate's own gap still under it.
    for bars in [1., 26., 48., 96., 300.] {
        assert!(
            sub_bottom(bars) >= bars + SUB_BOTTOM,
            "a {bars} px bar reaches the plate"
        );
    }
    // A taller bar never lowers the plate, and a box that was never painted
    // (or measured as nothing) reads as no bar at all rather than as a plate
    // pulled off the bottom of the window.
    assert!(sub_bottom(48.) > sub_bottom(26.));
    assert_eq!(sub_bottom(-10.), SUB_BOTTOM);
}

/// The two places a subtitle is drawn -- the plate over the picture and the
/// lanes under the tracks -- inside the 640x360 floor the rest of this window
/// is sized for, and the plate readable on whatever the film is showing.
#[test]
fn the_subtitle_plate_and_lanes_fit_the_smallest_window() {
    // A subtitle lane is a lane: it costs the panel exactly what a third
    // track costs it, and a timeline with none costs nothing at all -- the
    // strip that used to hang under the lanes is gone, and with it the one
    // row nothing could be dropped on.
    assert_eq!(lanes_h(3) - lanes_h(2), LANE_H + 8.);
    // What is left for the picture at the floor, with a project's two tracks
    // and a subtitle lane under them.
    let picture = 360. - HEADER_H - panel_h(3);
    assert!(
        picture > 0.,
        "a subtitle lane pushed the picture off the window"
    );
    // A two-line cue and the gap under it fit inside that, which is the
    // whole claim: the plate sits *over* the picture and must not need more
    // of it than there is.
    assert!(
        SUB_BOTTOM + 2. * SUB_LINE_H <= picture,
        "a two-line cue does not fit the smallest picture"
    );
    // The text sits on its line rather than being clipped by it.
    assert!(SUB_LINE_H >= SUB_TEXT);
    // White on the plate, not chrome on chrome: a cue is read against the
    // film, and this is the one pair here that has to survive any picture.
    for id in crate::ui::theme::PaletteId::ALL {
        assert!(contrast(id.palette().SUB_FG, 0x000000) >= 7., "{id:?}");
    }
    // A subtitle lane is dragged onto, trimmed on and lifted from, so its row
    // is a target and binds `HIT_MIN` like every other lane's -- the whole
    // reason it is a lane and no longer a 16 px strip.
    assert!(LANE_H >= HIT_MIN);
    // The library's own list of tracks scrolls past three rather than
    // taking the media list's room.
    assert_eq!(SUB_ROWS_H / ROW_H, 3.);
    assert!(ROW_H >= HIT_MIN, "a subtitle row is clicked to pick it");
    // A header is always drawn with more than one file, floor included: the
    // list scrolls past `SUB_ROWS_H` instead of a short window losing the
    // name saying whose tracks these are.
    assert!(SUB_HEAD_H + 2. * ROW_H <= SUB_ROWS_H);
    // A header folds its group on a click now, so it binds `HIT_MIN` like
    // every other target rather than being let off it as a bare label.
    assert!(SUB_HEAD_H >= HIT_MIN);
}

/// The subtitle style file, round-tripped through a scratch config directory
/// -- the same corner-cut persistence as the theme and the keybindings, kept
/// to the same test the load/save pair earns: a save followed by a load reads
/// back what was written, a file that never existed leaves the defaults in
/// force, and a file that exists but says nothing readable does too rather
/// than crashing a startup on someone else's edit of the file.
#[test]
fn the_subtitle_style_file_round_trips_or_falls_back_to_defaults() {
    let _config_env = config_env_lock();

    let dir = std::env::temp_dir().join(format!(
        "edith-subtitle-style-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // The shared process environment is serialized across every config
    // round-trip test by `config_env_lock`.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

    // Nothing written yet: the defaults, not a crash on a missing file.
    assert_eq!(load_subtitle_style(), (None, SUB_TEXT));

    // A save, read back whole: family and size both.
    save_subtitle_style(Some("Iosevka"), 22.).unwrap();
    assert_eq!(load_subtitle_style(), (Some("Iosevka".to_string()), 22.));

    // The platform default (`None`) round-trips too, and is not a family
    // called "".
    save_subtitle_style(None, 14.).unwrap();
    assert_eq!(load_subtitle_style(), (None, 14.));

    // A file present but unreadable as this format -- a stray edit, or bytes
    // from a future version -- leaves the defaults in force rather than
    // failing the window that opens over it.
    std::fs::write(subtitle_style_path(), "not a number\n9999\n").unwrap();
    assert_eq!(load_subtitle_style(), (None, SUB_TEXT));

    unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    std::fs::remove_dir_all(&dir).ok();
}

/// The auto-proxies default, round-tripped through a scratch config
/// directory the same way the subtitle style is above -- the door D1 wired
/// it through, so this is what the settings page header can now honestly
/// call the same claim it makes about Proxies: a project's own line always
/// beats this one once one is open ([`engine::edith::Document::auto_proxy`]),
/// but with nothing open (or nothing saved since the flip) this is what a
/// relaunch reads instead of the field's hardcoded `true`.
#[test]
fn the_auto_proxies_default_round_trips_through_its_own_config_file_or_falls_back_to_on() {
    let _config_env = config_env_lock();

    let dir = std::env::temp_dir().join(format!(
        "edith-auto-proxies-pref-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // `config_env_lock` serializes every test that changes this process-wide
    // path.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

    // Nothing written yet: On, the field's own long-standing default.
    assert!(load_auto_proxies_pref(), "a missing file must read as On");

    save_auto_proxies_pref(false).unwrap();
    assert!(!load_auto_proxies_pref(), "off must round-trip");

    save_auto_proxies_pref(true).unwrap();
    assert!(
        load_auto_proxies_pref(),
        "on must round-trip too, not just off"
    );

    // Garbled bytes -- a stray edit, or a future dialect -- read as On rather
    // than failing a startup on someone else's edit of the file.
    std::fs::write(auto_proxies_pref_path(), "not on or off\n").unwrap();
    assert!(
        load_auto_proxies_pref(),
        "unreadable content must fall back to On"
    );

    unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    std::fs::remove_dir_all(&dir).ok();
}

/// The cue's line height scales with its size, on the same ratio the defaults
/// draw at -- a bigger plate wants more room per line, not letters growing
/// inside an unchanged one.
#[test]
fn the_cue_line_height_scales_with_its_size() {
    assert_eq!(sub_line_h_for(SUB_TEXT), SUB_LINE_H);
    // Twice the size is twice the line height, on the same ratio.
    assert_eq!(sub_line_h_for(SUB_TEXT * 2.), SUB_LINE_H * 2.);
    // Always at least the text itself, across the whole size range: a line
    // shorter than its own text would clip it.
    for size in [SUB_SIZE_RANGE.0, SUB_TEXT, SUB_SIZE_RANGE.1] {
        assert!(sub_line_h_for(size) >= size);
    }
}

/// What the rows of both lists do with a name too long for the column: two
/// episodes off one release differ in their last two characters, and a name
/// cut from the right is the same row twice.
#[test]
fn a_long_name_is_cut_out_of_its_middle_so_two_episodes_stay_two_rows() {
    let (a, b) = (
        "A Long Release Name Of A Series 01",
        "A Long Release Name Of A Series 02",
    );
    // The two call sites at the narrowest the column ever is: a media row's
    // name gets the row's whole width, a subtitle row's file prefix gets a
    // share of it because the language has to fit beside it.
    let media = row_text_w(LIBRARY_MIN_W);
    let prefix = media * SUB_STEM_SHARE;
    for width in [media, prefix] {
        assert!(width > 0., "the floor leaves a row no words at all");
        assert_ne!(
            clip_middle(a, width),
            clip_middle(b, width),
            "{width}px: two episodes read as one row"
        );
        // Both ends survive: the release's name at the front, the number
        // that tells them apart at the back.
        assert!(clip_middle(a, width).starts_with(&a[..2]));
        assert!(clip_middle(a, width).ends_with("01"));
        assert!(clip_middle(a, width).contains('…'));
        // Never wider than the column can hold, gap included.
        assert!(clip_middle(a, width).chars().count() <= (width / 6.) as usize + 1);
    }
    // A name the width holds is left exactly as it is -- no gap, nothing
    // dropped -- and so is a name at any width once the column is wide.
    assert_eq!(clip_middle("eng.srt", 400.), "eng.srt");
    assert_eq!(clip_middle(a, 4000.), a);
    // Nothing panics at a width no column ever has, and something of both
    // ends is still there.
    assert_eq!(clip_middle(a, 0.).chars().count(), 5);
    assert!(clip_middle(a, 0.).ends_with('1'));
    // Counted in characters and not bytes: a name in another script is cut
    // between its letters, not through one.
    assert_eq!(clip_middle("ααααααααααααααα", 24.), "αα…αα");
}

/// The two doors subtitles arrive through, end to end on the fixtures: a
/// file beside the media and the tracks inside an mkv, what the overlay then
/// says at a given moment, and where the strip draws it.
#[test]
fn subtitles_arrive_beside_the_media_and_inside_it() {
    // Which door a path goes through is decided before anything is opened.
    // ...and a `.mks` is one, the subtitles of a Matroska file alone: it has
    // no source in it to import, so the drop door has to send it where `+ S`
    // sends it. Its two siblings are media and must not come here -- a
    // `.mka` is a song this would stop importing.
    for name in [
        "subs.srt", "SUBS.SRT", "a.vtt", "a.ass", "a.ssa", "s.mks", "S.MKS",
    ] {
        assert!(is_subtitle(Path::new(name)), "{name}");
    }
    for name in ["a.mp4", "a.mkv", "song.mka", "film.mk3d", "notes.txt", "a"] {
        assert!(!is_subtitle(Path::new(name)), "{name}");
    }
    // Every container the engine can walk for tracks inside it, Matroska
    // and ISO-BMFF alike -- an mp4 carries `tx3g`, so the app has to ask it.
    // The Matroska half is the engine's own closed set, extension for
    // extension, so no file is walked here that it would refuse there.
    for name in [
        "film.MKV",
        "clip.webm",
        "clip.mp4",
        "a.m4v",
        "a.MOV",
        "song.mka",
        "s.mks",
        "f.MK3D",
    ] {
        assert!(carries_subtitles(Path::new(name)), "{name}");
        assert!(
            engine::demux::is_matroska(Path::new(name))
                || matches!(
                    Path::new(name)
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(str::to_ascii_lowercase)
                        .as_deref(),
                    Some("mp4" | "m4v" | "mov")
                ),
            "{name} is walked by the app but not by the engine"
        );
    }
    for name in ["song.wav", "still.png", "notes.txt", "a"] {
        assert!(!carries_subtitles(Path::new(name)), "{name}");
    }
    // The three doors, on the one file that used to split them: what the
    // drop/argv door does with it ([`Player::take_import`] routes on
    // `is_subtitle`), what the worker reads for it ([`walk_subtitles`]), and
    // what `+ S` reads ([`Player::add_subtitles`] -> `parse_subtitles`) are
    // the same walk of the same bytes.
    let mks = asset("test_subs.mks");
    assert!(is_subtitle(&mks), "the drop door takes it as subtitles");
    let dropped = walk_subtitles(&mks).expect("the drop door's worker reads it");
    let plus_s = PlaybackSession::parse_subtitles(&mks).expect("`+ S` reads it");
    assert!(!dropped.is_empty(), "and there are tracks in it");
    assert_eq!(
        dropped.iter().map(|t| &t.label).collect::<Vec<_>>(),
        plus_s.iter().map(|t| &t.label).collect::<Vec<_>>(),
    );

    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    // Beside the media: the drop door's own call, and the row it makes.
    let srt = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/data/test_subs.srt")
        .canonicalize()
        .expect("the subtitle fixture");
    assert_eq!(session.import_subtitles(&srt).expect("the .srt imports"), 1);
    let track = &session.subtitles()[0];
    assert_eq!(track.label, "test_subs.srt");
    assert_eq!(subtitle_detail(track), "3 cues");
    // Arriving is landing in the *list* and nowhere else: no lane is made for
    // it, so there is nothing over the picture yet -- the rule every other
    // medium follows here, and what `Player::subtitle_overlay` reads.
    assert!(
        session.subtitle_lanes().is_empty(),
        "an import places nothing: the palette is where a track lands"
    );
    // What the track itself says at a moment inside the first cue, and between
    // two of them -- the fixture's own timings (0.5-1.5 s, 2.0-3.25 s).
    let text = |t: f64| -> Vec<String> {
        cues_at(&track.cues, t)
            .into_iter()
            .map(|c| c.text.clone())
            .collect()
    };
    assert_eq!(text(0.7), ["first line"]);
    assert!(text(1.8).is_empty(), "between two cues the plate goes");
    assert_eq!(text(2.5), ["second line\nwith a break"]);
    // Where the strip puts that first cue: 0.5 s in at 40 px to the second
    // is 20 px along, and a second of cue is 40 px wide.
    let scale = Scale::default();
    assert_eq!(scale.pps, PPS_DEFAULT);
    assert_eq!(cue_box(scale, &track.cues[0]), (20., 40.));
    // Zoomed right out, a cue worth a fraction of a pixel is still a mark.
    let far = Scale {
        pps: 0.01,
        start: 0.,
    };
    assert_eq!(cue_box(far, &track.cues[0]).1, SUB_CUE_MIN_W);
    // ...and scrolled past, its left edge is negative, exactly as a clip
    // box's is: the bed clips it.
    let scrolled = Scale {
        pps: PPS_DEFAULT,
        start: 1.,
    };
    assert_eq!(cue_box(scrolled, &track.cues[0]).0, -20.);

    // Inside the media: the tracks of an mkv, taken by the same call every
    // open and import door makes. Two of them in the fixture, named by what
    // the file says rather than by number.
    let notice =
        subtitle_notice(&mut session, &asset("test_subs.mkv")).expect("the mkv carries two");
    assert!(notice.contains('2'), "{notice}");
    assert_eq!(session.subtitles().len(), 3);
    assert_eq!(session.subtitles()[1].label, "eng");
    assert_eq!(session.subtitles()[2].label, "fra — Signs");
    assert_eq!(subtitle_detail(&session.subtitles()[1]), "3 cues");
    // The same file twice adds nothing and says nothing.
    assert_eq!(subtitle_notice(&mut session, &asset("test_subs.mkv")), None);
    // An mp4 is walked too (it can carry `tx3g`); this one holds none, so
    // the notice grows no tail.
    assert_eq!(subtitle_notice(&mut session, &asset("test_av.mp4")), None);

    // A track that could not be read says why, where its cue count would
    // be: what the greyed library row prints, and the whole difference
    // between "this film has no subtitles" and "these four are pictures".
    let refused = engine::subtitle::SubtitleTrack {
        path: PathBuf::from("/a/remux.mkv"),
        track: Some(1),
        // Neither field, like `SubtitleTrack::refused` leaves them: what is
        // refused is never written, and the row keeps the label it was
        // refused under.
        language: String::new(),
        name: String::new(),
        label: "eng".into(),
        cues: Vec::new(),
        bitmap: false,
        refused: Some("S_HDMV/PGS subtitles are pictures, not text".into()),
    };
    assert!(subtitle_detail(&refused).contains("pictures"));
    assert!(cues_at(&refused.cues, 1.).is_empty());

    // ...and what an embedded track carried through a *cut* maps to: the timeline's
    // own clock, asked of the engine through the very map an export writes
    // the file with (`PlaybackSession::timeline_cues`), so the preview and
    // the file cannot drift apart. The numbers are the export's own
    // (`export::a_subtitle_file_beside_the_media_keeps_the_timelines_own_
    // clock`): 0.5s..2.5s rippled out of a five-second timeline leaves
    // three seconds, which clips the second cue and takes the third away
    // altogether -- while the track itself still holds all three, which is
    // exactly why the drawing may not read them straight.
    let lanes = session.lanes();
    session
        .cut_regions(&[(15, 60)], &lanes)
        .expect("cut 0.5s..2.5s out");
    assert_eq!(session.timeline_duration(), 3.0);
    assert_eq!(
        session.subtitles()[0].cues.len(),
        3,
        "the track is untouched"
    );
    let mapped = session.timeline_cues(0);
    assert_eq!(
        mapped
            .iter()
            .map(|c| (c.start_us, c.end_us))
            .collect::<Vec<_>>(),
        vec![(500_000, 1_500_000), (2_000_000, 3_000_000)]
    );
    // The export's own two lines, at the same moments as above.
    let drawn = |t: f64| -> Vec<String> {
        cues_at(&mapped, t)
            .into_iter()
            .map(|c| c.text.clone())
            .collect()
    };
    assert_eq!(drawn(0.7), ["first line"]);
    assert_eq!(drawn(2.5), ["second line\nwith a break"]);
    assert!(
        drawn(4.2).is_empty(),
        "a cue past the cut end is drawn where the file writes none"
    );
    // ...and the strip's: the second cue is one second wide now, not 1.25.
    assert_eq!(cue_box(scale, &mapped[1]), (80., 40.));

    // The last mile, what the plate over the picture actually reads
    // (`Player::subtitle_overlay` -> `PlaybackSession::sub_lane_cues`): a lane
    // added and still empty draws nothing, and the palette row reaches the
    // screen the moment it is *placed* on it -- shifted by where it sits, the
    // way a clip is.
    let lane = session.add_lane(LaneKind::Subtitle);
    assert!(
        session.sub_lane_cues(lane).is_empty(),
        "an empty subtitle lane draws nothing"
    );
    session
        .place_sub(
            lane,
            0,
            SubClip {
                start: 0,
                frames: 90,
                track: 0,
                in_us: 0,
                out_us: 5_000_000,
                link: None,
            },
        )
        .expect("the palette row goes down on the lane");
    let placed = session.sub_lane_cues(lane);
    let shown = |t: f64| -> Vec<String> {
        cues_at(&placed, t)
            .into_iter()
            .map(|c| c.text.clone())
            .collect()
    };
    // The placement holds five seconds of the track in a three-second box,
    // so the words cross the plate by the placement's own proportion: the
    // cue times compress with it (first line [0.5, 1.5) -> [0.3, 0.9),
    // second [2, 3.25) -> [1.2, 1.95)) -- a unity placement would keep the
    // file's own times, to the microsecond.
    assert_eq!(shown(0.7), ["first line"]);
    assert!(shown(0.95).is_empty(), "between two cues the plate goes");
    assert_eq!(shown(1.5), ["second line\nwith a break"]);
}

/// One lane over the picture and never two ([`Player::active_sub_lane`]): which
/// one, at every moment a stack of subtitle lanes goes through -- none, one,
/// three, a pick, and the pick's lane going away under it.
#[test]
fn exactly_one_subtitle_lane_is_shown_whatever_the_pick_and_whatever_is_left() {
    let sub = |ord| Lane::new(LaneKind::Subtitle, ord);
    let (s1, s2, s3) = (sub(0), sub(1), sub(2));
    // Nowhere for words to be: no lane, nothing shown -- and a pick left over
    // from a timeline that has been closed does not resurrect one.
    assert_eq!(active_lane(None, &[]), None);
    assert_eq!(active_lane(Some(s2), &[]), None);
    // One lane is shown by being the only one: nobody has to pick anything,
    // which is what the single-lane timeline always did.
    assert_eq!(active_lane(None, &[s1]), Some(s1));
    // Three, unpicked: the first, so adding lanes never blanks the picture.
    assert_eq!(active_lane(None, &[s1, s2, s3]), Some(s1));
    // ...and picked: that one alone, however many are there.
    assert_eq!(active_lane(Some(s3), &[s1, s2, s3]), Some(s3));
    // The picked lane taken off the timeline -- a removal, or the undo of the
    // add that made it: the first lane left is shown, never nothing.
    assert_eq!(active_lane(Some(s3), &[s1, s2]), Some(s1));
    // A pick that is not a subtitle lane at all cannot name one either.
    assert_eq!(active_lane(Some(Lane::V1), &[s1, s2]), Some(s1));
}

/// A library preview's whole point ([`Player::open_preview`],
/// [`Player::close_preview`]): the timeline's own session is untouched by
/// one showing over it. `Player` cannot be built here -- it takes a gpui
/// `Context`, which this test binary has no `TestAppContext` to hand it
/// (see the crate's other tests for the same limit) -- so this exercises
/// the two sessions `open_preview`/`close_preview` juggle directly: seeking
/// and playing one, exactly as a preview session's own life does, leaves
/// the other's position and clock exactly where they were.
#[test]
fn a_second_session_playing_never_moves_the_first() {
    let mut timeline = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    timeline.seek(0.5);
    let before = timeline.now();
    let before_playing = timeline.is_playing();

    // Stands in for the preview session `open_preview` opens over it.
    let mut preview = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    preview.seek(1.0);
    preview.play();

    assert_eq!(
        timeline.now(),
        before,
        "the preview's seek moved the wrong session"
    );
    assert_eq!(
        timeline.is_playing(),
        before_playing,
        "the preview's play moved the wrong session"
    );
}

/// The frozen-preview regression: `render` used to call `tick` on
/// `self.session` unconditionally, so a preview session's clock -- the one
/// [`Player::pump`] actually reads while previewing -- was never advanced
/// and the picture sat on frame 0 forever. The fix routes `tick` through
/// [`Player::active_session_mut`], the same preview-first precedence
/// `pump`/`transport` already use. This proves `tick` is what a session
/// needs to keep its own clock honest, and that ticking one session leaves
/// an untouched sibling exactly where it was -- so routing it through
/// whichever session is active cannot disturb the other one.
#[test]
fn ticking_the_preview_session_never_moves_the_timeline() {
    let mut timeline = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    timeline.seek(0.5);
    let before = timeline.now();

    let mut preview = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    preview.play();
    // Stands in for what `render` now does every frame while a preview is
    // showing: only the active session -- the preview -- gets ticked.
    preview.tick();

    assert_eq!(
        timeline.now(),
        before,
        "ticking the preview moved the timeline's clock"
    );
}

/// [`bgra_to_rgba`] and [`save_screenshot`]'s own PNG write, round-tripped: a
/// synthetic 2x2 frame -- one pixel per corner, alpha and channel order all
/// distinct -- decodes back to the same pixels a viewer would see, in RGBA.
#[test]
fn a_screenshot_frame_round_trips_through_its_png() {
    // BGRA, gpui's own layout: red top-left, green top-right, blue
    // bottom-left, and a half-alpha white bottom-right.
    #[rustfmt::skip]
    let bgra: Vec<u8> = vec![
        0, 0, 255, 255,   0, 255, 0, 255,
        255, 0, 0, 255,   255, 255, 255, 128,
    ];
    let rgba = bgra_to_rgba(&bgra);
    assert_eq!(
        rgba[0..4],
        [255, 0, 0, 255],
        "red survives the channel swap"
    );
    assert_eq!(rgba[4..8], [0, 255, 0, 255], "green is unmoved by the swap");
    assert_eq!(rgba[8..12], [0, 0, 255, 255], "blue swaps into place");
    assert_eq!(rgba[12..16], [255, 255, 255, 128], "alpha is untouched");

    let dir = std::env::temp_dir().join(format!("edith-screenshot-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("roundtrip.png");
    let buf = image::RgbaImage::from_raw(2, 2, rgba.clone()).expect("2x2 rgba buffer");
    buf.save(&path).expect("png write");
    let decoded = image::open(&path).expect("png read").to_rgba8();
    assert_eq!(
        decoded.into_raw(),
        rgba,
        "the PNG round-trips the pixels exactly"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`screenshot_path`]'s no-clobber rule: the same stem and timecode taken
/// twice in a row lands on `-2`, and a third time on `-3` -- a screenshot
/// never overwrites the one before it, unlike an export's own path.
#[test]
fn a_repeated_screenshot_name_gets_a_numeric_suffix() {
    let dir = std::env::temp_dir().join(format!("edith-screenshot-suffix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let first = screenshot_path(&dir, "A Film", "00-01-02");
    assert_eq!(first, dir.join("A Film-00-01-02.png"));
    std::fs::write(&first, b"stand-in for a PNG").expect("write the first");

    let second = screenshot_path(&dir, "A Film", "00-01-02");
    assert_eq!(second, dir.join("A Film-00-01-02-2.png"));
    std::fs::write(&second, b"stand-in for a PNG").expect("write the second");

    let third = screenshot_path(&dir, "A Film", "00-01-02");
    assert_eq!(third, dir.join("A Film-00-01-02-3.png"));

    // A different timecode never collides with either of the above.
    let elsewhere = screenshot_path(&dir, "A Film", "00-01-03");
    assert_eq!(elsewhere, dir.join("A Film-00-01-03.png"));

    let _ = std::fs::remove_dir_all(&dir);
}
