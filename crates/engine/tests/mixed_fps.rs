//! Clips shot at other frame rates on one timeline: what a 25 fps and a 23.976
//! fps file do on the 30 fps timeline `test_av.mp4` scaffolds.
//!
//! Everything goes through the front door -- `import`, `place_stream_at`,
//! `seek`, `export_to_with`, `open_project` -- because the whole claim is that a
//! *user* can put two rates on one timeline and watch both at the speed they
//! were shot at. `project.rs`'s own `rate_composes_with_speed` owns the
//! arithmetic; what is measured here is that the pictures and the file agree
//! with it.
//!
//! ```text
//! cargo test -p engine --test mixed_fps
//! ```

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::{Edge, Lane, Rate, Speed};
use engine::scratch::Scratch;
use engine::{DecodeSession, Frame, PlaybackSession};

/// The timeline's rate, which `test_av.mp4` (30 fps, 5 s) defines.
const FPS: f64 = 30.0;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Silent, for `tests/playback.rs`'s reason: the suite plays real files.
fn open(path: impl AsRef<Path>) -> PlaybackSession {
    let session = PlaybackSession::open(path).expect("open");
    session.set_gain(0.0);
    session
}

fn next_frame(session: &mut PlaybackSession, what: &str) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = session.try_frame() {
            return frame;
        }
        assert!(Instant::now() < deadline, "no frame after {what}");
        sleep(Duration::from_millis(4));
    }
}

/// One frame of a file, decoded straight from it: the picture a mapping is
/// checked against, in the file's *own* numbering.
fn source_frame(path: &Path, index: u32) -> Vec<u8> {
    let (_, rx, _cancel) = DecodeSession::open_range(path, index, index + 1).expect("open_range");
    let frame = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("reference frame");
    assert_eq!(frame.index, index, "open_range starts where it is told");
    frame.bgra
}

/// Imports `path` and drags it onto the end of the timeline, the way a library
/// row reaches a lane.
fn import_and_place(session: &mut PlaybackSession, path: &Path) {
    session.import(path).expect("the file joins this timeline");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, path, 0, None)
            .expect("a file just imported is on this timeline"),
        "the drag onto the end of the timeline"
    );
}

fn wait(handle: &engine::ExportHandle) -> engine::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(180);
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "export did not finish");
        sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// How different two pictures are, sampled: a re-encode moves every pixel a
/// little and a *different* picture of `testsrc2` moves them a lot, so the two
/// are never close to the threshold below.
fn difference(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "two pictures of one size");
    let (mut sum, mut n) = (0f64, 0u32);
    for i in (0..a.len()).step_by(997) {
        sum += f64::from(a[i].abs_diff(b[i]));
        n += 1;
    }
    sum / f64::from(n.max(1))
}

/// A file's length is the seconds it lasts, in the timeline's frames -- not the
/// frames it holds. That is the whole of what placing another rate means: 50
/// frames of 25 fps and 48 of 23.976 fps are both two seconds, and two seconds
/// of a 30 fps timeline is 60 frames.
#[test]
fn a_file_at_another_rate_is_placed_for_the_seconds_it_lasts() {
    let mut session = open(asset("test_av.mp4"));
    assert_eq!(session.file_frames(&asset("test_av.mp4")), 150, "5 s at 30");

    let pal = asset("test_25fps.mp4");
    session
        .import(&pal)
        .expect("25 fps joins a 30 fps timeline");
    assert_eq!(session.file_frames(&pal), 60, "50 frames at 25 fps is 2 s");

    // 24000/1001: 48 frames is 2.002 s, which is 60.06 frames of 30 fps --
    // rounded *up*, so the last picture is still reachable by a trim.
    let ntsc = asset("test_23976fps.mp4");
    session.import(&ntsc).expect("23.976 joins it too");
    assert_eq!(session.file_frames(&ntsc), 61);

    // ...and placed, both are as long on the timeline as they are in the ear.
    let end = session.timeline_duration();
    session
        .place_stream_at(end, &pal, 0, None)
        .expect("the drag");
    assert!(
        (session.timeline_duration() - (5.0 + 2.0)).abs() < 1e-9,
        "a 2 s clip made the timeline {} s",
        session.timeline_duration()
    );
    let end = session.timeline_duration();
    session
        .place_stream_at(end, &ntsc, 0, None)
        .expect("the drag");
    assert!(
        (session.timeline_duration() - (5.0 + 2.0 + 61. / FPS)).abs() < 1e-9,
        "{} s",
        session.timeline_duration()
    );
    // ...and the sound that came down with each take is the same length as its
    // picture: a placed take is one linked group, so a rate that stretched only
    // the picture would slide the two apart on the very first drag.
    let (video, audio) = (session.lane_clips(Lane::V1), session.lane_clips(Lane::A1));
    assert_eq!(video.len(), 3, "the 30 fps take and the two others");
    assert_eq!(audio.len(), video.len(), "one audio clip per take");
    for (v, a) in video.iter().zip(audio) {
        assert_eq!((v.start, v.len()), (a.start, a.len()), "{v:?} vs {a:?}");
    }
}

/// The NTSC ratio, which the 23.976 fps fixture is shot at: 24000/1001 over 30
/// is exactly 800/1001, and it stays exact however far the timeline runs. A rate
/// rounded to a float -- or to milli-fps -- would leave a fraction of a frame
/// per clip to pile up into a visible drift over an hour, which is why the
/// mapping is built out of the muxer's own timescales and not a division.
#[test]
fn a_23_976_rate_is_exact_and_never_drifts() {
    let ntsc = Rate::from_fps(24_000.0 / 1001.0, FPS).expect("23.976 over 30");
    assert!(
        (ntsc.as_f64() - 800.0 / 1001.0).abs() < 1e-15,
        "{}",
        ntsc.as_f64()
    );
    // Independent integer arithmetic, out to an hour of source and past it: 800
    // source frames per 1001 timeline ones, and a file's length is that ceiled
    // (the last picture must still be reachable by a trim).
    for source in [1u64, 2, 24, 1000, 86_486, 500_000, 1_000_000] {
        assert_eq!(
            u64::from(ntsc.timeline_at(source as u32)),
            (source * 1001).div_ceil(800),
            "{source} source frames"
        );
    }
    // ...and the two directions stay each other's inverse at every one of them:
    // the frame an export writes at timeline frame `d` is the frame playback is
    // holding there, which is what `timeline_at`'s ceil buys.
    for d in [0u32, 1, 2, 3, 1000, 100_000, 1_000_000] {
        let source = ntsc.source_at(d);
        assert!(ntsc.timeline_at(source) <= d, "frame {d} is not yet due");
        assert!(
            ntsc.timeline_at(source + 1) > d,
            "the next frame is due too early at {d}"
        );
    }
    // A timeline's own rate conforms by nothing at all, exactly -- whichever of
    // the two rates it is written at.
    assert!(Rate::from_fps(FPS, FPS).expect("30 over 30").is_real_time());
    assert!(
        Rate::from_fps(24_000.0 / 1001.0, 24_000.0 / 1001.0)
            .expect("ntsc over ntsc")
            .is_real_time()
    );
    // A rate no timescale can name is refused rather than read 1:1 in silence:
    // the one thing `matches_timeline` still turns a file away for.
    assert!(Rate::from_fps(0.0, FPS).is_err(), "not a rate at all");
}

/// The picture on screen at a timeline frame is the picture the *file* holds
/// there: frame `floor(offset * 25/30)` of a 25 fps clip, which is what playing
/// at the speed it was shot at means, frame for frame.
#[test]
fn playback_shows_the_frame_the_file_holds_there() {
    let pal = asset("test_25fps.mp4");
    let mut session = open(asset("test_av.mp4"));
    import_and_place(&mut session, &pal);
    session.pause();
    // The clip starts at timeline frame 150 (5 s) and runs 60 frames.
    let start = 150u32;
    for offset in [0u32, 1, 5, 6, 29, 30, 47, 59] {
        let at = start + offset;
        session.seek(f64::from(at) / FPS);
        let landed = next_frame(&mut session, "a seek into the 25 fps clip");
        assert_eq!(landed.index, at, "a seek lands on the frame it asked for");
        let want = (u64::from(offset) * 25_000 / 30_000) as u32;
        assert!(
            landed.bgra == source_frame(&pal, want),
            "timeline frame {at} (offset {offset}) is not source frame {want}"
        );
    }
    // Past the last picture the file holds, the clip is still on the timeline:
    // its last frame is shown, not black and not a panic.
    session.seek(f64::from(start + 59) / FPS);
    let last = next_frame(&mut session, "the last frame of the clip");
    assert!(last.bgra == source_frame(&pal, 49), "source frame 49 last");
    // ...and the frame in front of it is the 30 fps take, frame for frame: a
    // rate belongs to the source that has one, so the clip beside it is read
    // exactly as it was before any of this existed.
    let av = asset("test_av.mp4");
    session.seek(f64::from(start - 1) / FPS);
    let before = next_frame(&mut session, "the last frame of the 30 fps take");
    assert_eq!(before.index, start - 1);
    assert!(
        before.bgra == source_frame(&av, start - 1),
        "the 30 fps clip is still frame for frame"
    );
}

/// A trim of a clip at another rate is dragged in *timeline* frames and commits
/// the source range those frames are worth -- so the box is exactly the room it
/// was dragged to, and the picture at its new edge is the file's own frame
/// there.
#[test]
fn a_clip_at_another_rate_trims_to_the_room_it_was_dragged_to() {
    let pal = asset("test_25fps.mp4");
    let mut session = open(asset("test_av.mp4"));
    import_and_place(&mut session, &pal);
    session.pause();
    let start = 150u32;
    // Drag its tail back to 30 timeline frames of room: one second, which at 25
    // fps is the file's first 25 pictures.
    assert!(
        session.trim_clip(Lane::V1, 1, Edge::End, start + 30),
        "trim the tail of the 25 fps clip"
    );
    let clip = session.lane_clips(Lane::V1)[1];
    assert_eq!((clip.start, clip.len()), (start, 30), "{clip:?}");
    session.seek(f64::from(start + 29) / FPS);
    let last = next_frame(&mut session, "the trimmed tail");
    assert!(
        last.bgra == source_frame(&pal, 24),
        "one second of a 25 fps file ends on its frame 24"
    );
    // ...and its head, dragged forward into source it keeps: 6 timeline frames
    // in is 5 source frames in, and the tail did not move.
    assert!(session.trim_clip(Lane::V1, 1, Edge::Start, start + 6));
    let clip = session.lane_clips(Lane::V1)[1];
    assert_eq!((clip.start, clip.len()), (start + 6, 24), "{clip:?}");
    session.seek(f64::from(start + 6) / FPS);
    let head = next_frame(&mut session, "the trimmed head");
    assert!(
        head.bgra == source_frame(&pal, 5),
        "6 frames of a 30 fps timeline is 5 frames of a 25 fps file"
    );
}

/// A speed and a frame rate compose: a 2x 25 fps clip on a 30 fps timeline is
/// half as many timeline frames and reads twice as far into the file for each
/// of them.
#[test]
fn a_speeded_clip_at_another_rate_is_exactly_both() {
    let pal = asset("test_25fps.mp4");
    let mut session = open(asset("test_av.mp4"));
    import_and_place(&mut session, &pal);
    session
        .set_speed(Lane::V1, 1, Speed::from_permille(2000))
        .expect("nothing is in the way of a clip that got shorter");
    session.pause();
    let start = 150u32;
    assert!(
        (session.timeline_duration() - (5.0 + 1.0)).abs() < 1e-9,
        "2 s at 2x is 1 s: {}",
        session.timeline_duration()
    );
    for offset in [0u32, 3, 14, 29] {
        let at = start + offset;
        session.seek(f64::from(at) / FPS);
        let landed = next_frame(&mut session, "a seek into the speeded clip");
        assert_eq!(landed.index, at);
        // The speed picks the clip's (timeline-rate) frame, the rate picks the
        // file's -- in that order, which is the order the decoder's door
        // applies them.
        let want = (u64::from(offset) * 2 * 25_000 / 30_000) as u32;
        assert!(
            landed.bgra == source_frame(&pal, want),
            "2x at offset {offset} is not source frame {want}"
        );
    }
}

/// A saved project reloads at the same lengths: `open_project` recomputes every
/// source's rate from the file, so a mixed-rate timeline is not one that only
/// the session that built it could hold.
#[test]
fn a_mixed_rate_project_reloads_unchanged() {
    let pal = asset("test_25fps.mp4");
    let mut session = open(asset("test_av.mp4"));
    import_and_place(&mut session, &pal);
    let duration = session.timeline_duration();
    let spans = session.clip_spans();
    let path = Scratch::file("edith-mixed", "edith");
    session.save_project(&path).expect("save");

    let reloaded = PlaybackSession::open_project(&path).expect("a mixed-rate project reopens");
    assert_eq!(reloaded.clip_spans(), spans, "the same clips, in seconds");
    assert!((reloaded.timeline_duration() - duration).abs() < 1e-9);
    assert_eq!(reloaded.file_frames(&pal), 60, "the length was recomputed");
}

/// The project's rate is the user's to pick, exactly as its resolution is. The
/// timeline is *conformed* to it -- the same seconds of the same footage,
/// counted on a new grid -- every source is read through a [`Rate`] against it,
/// the media's own rate stays reachable, and a save comes back at the rate it
/// was cut at rather than at the media's.
#[test]
fn the_project_rate_is_picked_and_the_timeline_is_conformed_to_it() {
    let pal = asset("test_25fps.mp4");
    let mut session = open(asset("test_av.mp4"));
    import_and_place(&mut session, &pal);
    let duration = session.timeline_duration();
    assert_eq!(session.native_frame_rate(), FPS, "the media's own rate");
    assert!(!session.set_frame_rate(FPS), "the rate already in force");
    assert!(!session.set_frame_rate(0.0), "not a rate at all");

    assert!(
        session.set_frame_rate(24.0),
        "24 is a rate a timescale can name"
    );
    assert_eq!(session.meta().frame_rate, 24.0);
    assert!(
        (session.timeline_duration() - duration).abs() < 1.0 / 24.0,
        "the timeline is {} s where it was {duration} s",
        session.timeline_duration()
    );
    // Both files are read through a rate against the new grid now: 5 s and 2 s,
    // in 24ths.
    assert_eq!(session.file_frames(&asset("test_av.mp4")), 120, "5 s at 24");
    assert_eq!(session.file_frames(&pal), 48, "2 s at 24");
    // ...and each take is still one take on both lanes: every frame number goes
    // through one map, so a picture cannot drift off its sound.
    let (video, audio) = (session.lane_clips(Lane::V1), session.lane_clips(Lane::A1));
    assert_eq!(video.len(), 2, "the 30 fps take and the 25 fps one");
    for (v, a) in video.iter().zip(audio) {
        assert_eq!((v.start, v.len()), (a.start, a.len()), "{v:?} vs {a:?}");
    }

    let path = Scratch::file("edith-rate", "edith");
    session.save_project(&path).expect("save");
    let reloaded = PlaybackSession::open_project(&path).expect("a retimed project reopens");
    assert_eq!(reloaded.meta().frame_rate, 24.0, "the rate it was cut at");
    assert_eq!(
        reloaded.clip_spans(),
        session.clip_spans(),
        "the same clips"
    );
    assert_eq!(reloaded.file_frames(&pal), 48, "recomputed against 24");
    assert_eq!(
        reloaded.native_frame_rate(),
        FPS,
        "the way back is still there"
    );
    std::fs::remove_file(&path).ok();

    // ...and that way back is a pick like any other: the media's own rate.
    let native = session.native_frame_rate();
    assert!(session.set_frame_rate(native), "back to the media's own");
    assert_eq!(session.file_frames(&pal), 60, "2 s at 30 again");
    assert!(
        (session.timeline_duration() - duration).abs() < 1.0 / 24.0,
        "{} s after the round trip, {duration} s before",
        session.timeline_duration()
    );
}

/// ...and an export of it comes out at that rate: the file is written at 24 fps,
/// as many frames as the conformed timeline has, with its sound as long as its
/// picture.
#[test]
fn an_export_runs_at_the_rate_the_project_was_cut_at() {
    let mut session = open(asset("test_av.mp4"));
    assert!(
        session.trim_clip(Lane::V1, 0, Edge::End, 30),
        "one second of the 30 fps take"
    );
    assert!(session.set_frame_rate(24.0), "cut it at 24 instead");
    let total = (session.timeline_duration() * 24.0).round() as u32;
    assert_eq!(total, 24, "one second, in 24ths");

    let out = Scratch::file("edith-rate", "mp4");
    wait(&session.export_to_with(
        &out,
        &ExportSettings {
            format: Format::Mp4,
            ..ExportSettings::default()
        },
    ))
    .expect("the export");

    let (meta, _rx, _cancel) = DecodeSession::open_range(&out, 0, u32::MAX).expect("reopen");
    assert!(
        (meta.frame_rate - 24.0).abs() < 0.01,
        "the export runs at the picked rate: {}",
        meta.frame_rate
    );
    assert_eq!(
        meta.frame_count, total,
        "the export is the timeline's length"
    );
    let secs = engine::AudioSession::duration_secs(&out)
        .expect("probe the export")
        .expect("the export has sound");
    assert!(
        (secs - f64::from(total) / 24.0).abs() < 0.05,
        "sound is {secs} s under a second of picture"
    );
    std::fs::remove_file(&out).ok();
}

/// The export is what was watched: as many frames as the timeline has, and the
/// 25 fps clip walked through its file at 25 fps -- 25 pictures over 30 frames,
/// not 30 of them and not 25 held to the end.
#[test]
fn an_export_of_two_rates_runs_at_the_speed_it_was_shot() {
    let pal = asset("test_25fps.mp4");
    let mut session = open(asset("test_av.mp4"));
    // A short head of the 30 fps take, then the whole of the 25 fps one --
    // enough of each to be a mixed timeline, short enough to encode in a test.
    // The sound comes with it: a placed take is one linked group.
    assert!(
        session.trim_clip(Lane::V1, 0, Edge::End, 15),
        "trim the 30 fps take to half a second"
    );
    import_and_place(&mut session, &pal);
    assert!(
        session.trim_clip(Lane::V1, 1, Edge::End, 45),
        "one second of the 25 fps clip"
    );
    let total = (session.timeline_duration() * FPS).round() as u32;
    assert_eq!(total, 45, "15 frames of 30 fps and 30 of the 25 fps clip");

    let out = Scratch::file("edith-mixed", "mp4");
    wait(&session.export_to_with(
        &out,
        &ExportSettings {
            format: Format::Mp4,
            ..ExportSettings::default()
        },
    ))
    .expect("the export");

    // Duration first: the file is as long as the timeline, so the mixed rates
    // did not stretch or shorten it.
    let (meta, rx, _cancel) = DecodeSession::open_range(&out, 0, u32::MAX).expect("reopen");
    assert!(
        (meta.frame_rate - FPS).abs() < 0.01,
        "the export runs at the timeline's rate: {}",
        meta.frame_rate
    );
    assert_eq!(
        meta.frame_count, total,
        "the export is the timeline's length"
    );
    let written: Vec<Vec<u8>> = rx.into_iter().map(|frame| frame.bgra).collect();
    assert_eq!(written.len() as u32, total, "every frame came back");

    // ...and motion: inside the 25 fps clip, 30 exported frames show 25
    // pictures, so 5 of them are a picture held from the frame before. A clip
    // played at the wrong rate would hold none (too fast, and it would run out
    // of file) or hold half of them (too slow).
    let held = (16..total as usize)
        .filter(|&i| difference(&written[i - 1], &written[i]) < 2.0)
        .count();
    assert_eq!(
        held, 5,
        "30 timeline frames of a 25 fps clip hold 5 pictures over"
    );

    // ...and *which* picture each of those frames is: the very one playback
    // holds there, which is the whole reason the two conversions are one
    // floor/ceil pair. Nearer its own source frame than either neighbour rather
    // than equal to it -- the export is re-encoded, so no pixel survives whole.
    for offset in [0u32, 1, 6, 12, 24, 29] {
        let want = (u64::from(offset) * 25_000 / 30_000) as u32;
        let mine = difference(&written[(15 + offset) as usize], &source_frame(&pal, want));
        for other in [want.saturating_sub(1), want + 1] {
            if other == want {
                continue;
            }
            assert!(
                mine < difference(&written[(15 + offset) as usize], &source_frame(&pal, other)),
                "exported frame {offset} of the 25 fps clip is nearer source \
                 frame {other} than its own {want}"
            );
        }
    }

    // The sound came out as long as the picture, from a *packet copy*: a clip
    // counts timeline frames whatever its file was shot at, so the window this
    // export trimmed the 25 fps file's AAC to is whole timeline frames and the
    // copy is in sync. This is the assertion behind the note in
    // `export::copy_audio` that says not to turn a conformed lane into a
    // re-encode.
    let secs = engine::AudioSession::duration_secs(&out)
        .expect("probe the export")
        .expect("the export has sound");
    let want = f64::from(total) / FPS;
    assert!(
        (secs - want).abs() < 0.05,
        "sound is {secs} s under {want} s of picture"
    );
    std::fs::remove_file(&out).ok();
}
