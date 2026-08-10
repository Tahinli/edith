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
use engine::project::{Edge, Lane, Speed};
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
    let path = std::env::temp_dir().join(format!("edith-mixed-{}.edith", std::process::id()));
    session.save_project(&path).expect("save");

    let reloaded = PlaybackSession::open_project(&path).expect("a mixed-rate project reopens");
    assert_eq!(reloaded.clip_spans(), spans, "the same clips, in seconds");
    assert!((reloaded.timeline_duration() - duration).abs() < 1e-9);
    assert_eq!(reloaded.file_frames(&pal), 60, "the length was recomputed");
    std::fs::remove_file(&path).ok();
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

    let out = std::env::temp_dir().join(format!("edith-mixed-{}.mp4", std::process::id()));
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
    std::fs::remove_file(&out).ok();
}
