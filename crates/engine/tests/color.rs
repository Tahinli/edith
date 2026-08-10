//! A per-clip colour grade, end to end: what the renderer shows and what the
//! export writes have to be the same picture, and a clip nobody graded has to
//! come out of both untouched.
//!
//! `test_baseline.mp4` is colour bars (mean channel spread 249 of 255), so
//! saturation 0 is not a subtle claim: every pixel of a graded span is grey to
//! within the converter's own rounding, and every pixel outside it is not.
//!
//! ```text
//! cargo test -p engine --release --test color -- --nocapture --test-threads=1
//! ```

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use engine::color::ColorParams;
use engine::export::ExportSettings;
use engine::project::Lane;
use engine::{DecodeSession, ExportHandle, PlaybackSession};

const FPS: f64 = 30.0;
/// Where the first clip ends and the ungraded one begins.
const CUT: u32 = 30;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Silent, like every other suite that plays a real file: the grade is a
/// picture setting and nothing here listens.
fn open(path: &Path) -> PlaybackSession {
    let session = PlaybackSession::open(path).expect("open");
    session.set_gain(0.0);
    session
}

/// Two clips on `V1`, the first of them drained of all colour.
fn graded() -> PlaybackSession {
    let mut session = open(&asset("test_baseline.mp4"));
    assert!(session.cut_at(f64::from(CUT) / FPS), "cut at frame 30");
    assert!(
        session.set_color(
            Lane::V1,
            0,
            Some(ColorParams {
                saturation: 0.0,
                ..Default::default()
            })
        ),
        "grade the first clip"
    );
    session
}

/// How far a BGRA frame is from grey: the mean spread between its channels,
/// which is 0 for a greyscale picture and ~250 for these colour bars.
fn spread(bgra: &[u8]) -> f64 {
    let total: u64 = bgra
        .chunks_exact(4)
        .map(|px| u64::from(px[..3].iter().max().unwrap() - px[..3].iter().min().unwrap()))
        .sum();
    total as f64 / (bgra.len() / 4) as f64
}

/// The next frame off a freshly seeked channel.
fn next_frame(session: &mut PlaybackSession, what: &str) -> engine::Frame {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = session.try_frame() {
            return frame;
        }
        assert!(Instant::now() < deadline, "no frame after {what}");
        sleep(Duration::from_millis(4));
    }
}

fn seek_and_show(session: &mut PlaybackSession, frame: u32) -> Vec<u8> {
    session.seek(f64::from(frame) / FPS);
    let shown = next_frame(session, "a seek");
    assert_eq!(shown.index, frame, "the seek landed elsewhere");
    shown.bgra
}

/// The render half: the grade reaches the picture, and it stops at the clip's
/// own edge rather than colouring the whole timeline.
#[test]
fn a_graded_clip_renders_grey_and_the_next_one_does_not() {
    let mut session = graded();
    for frame in [0, CUT - 1] {
        let inside = spread(&seek_and_show(&mut session, frame));
        assert!(
            inside <= 2.0,
            "frame {frame} is graded and not grey: {inside}"
        );
    }
    for frame in [CUT, CUT + 15] {
        let outside = spread(&seek_and_show(&mut session, frame));
        assert!(
            outside > 100.0,
            "frame {frame} is outside the graded clip and lost its colour: {outside}"
        );
    }
}

/// Live: a grade set while the session is up repaints what is on screen. The
/// engine reseeks on the edit, so the very next frame is the graded one -- no
/// scrub, no reopen by the caller.
#[test]
fn setting_a_grade_repaints_the_frame_that_is_already_up() {
    let mut session = open(&asset("test_baseline.mp4"));
    let before = spread(&seek_and_show(&mut session, 10));
    assert!(before > 100.0, "the fixture is not colour bars: {before}");

    assert!(session.set_color(
        Lane::V1,
        0,
        Some(ColorParams {
            saturation: 0.0,
            ..Default::default()
        })
    ));
    let after = spread(&next_frame(&mut session, "a grade").bgra);
    assert!(after <= 2.0, "the frame on screen kept its colour: {after}");

    // And taking it off puts the colour back, on the same terms.
    assert!(session.set_color(Lane::V1, 0, None));
    let back = spread(&next_frame(&mut session, "an ungrade").bgra);
    assert!(back > 100.0, "ungrading left the picture grey: {back}");
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < limit,
            "export did not finish in {limit:?}"
        );
        sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// The export half, decoded back off disk: the same picture the renderer
/// showed, graded span for graded span. Software encoder, so it needs nothing
/// installed -- the same pin `tests/export.rs`'s default suite uses.
#[test]
fn an_exported_grade_is_the_one_that_was_watched() {
    unsafe {
        std::env::set_var("VE_SW", "1");
        std::env::set_var("VE_SW_ENC", "1");
    }
    let mut session = graded();
    session.pause();
    let out = std::env::temp_dir().join(format!("ve_color_{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let handle = session.export_to_with(&out, &ExportSettings::default());
    wait(&handle, Duration::from_secs(180)).expect("export");

    let (_, frames) = DecodeSession::open(&out).expect("decode the export");
    let mut seen = 0;
    for frame in frames {
        let spread = spread(&frame.bgra);
        if frame.index < CUT {
            assert!(
                spread <= 4.0,
                "exported frame {} is inside the graded clip and kept its colour: {spread}",
                frame.index
            );
        } else {
            assert!(
                spread > 100.0,
                "exported frame {} is outside the graded clip and lost its colour: {spread}",
                frame.index
            );
        }
        seen += 1;
    }
    assert!(seen > CUT, "the export is shorter than the graded clip");
    let _ = std::fs::remove_file(&out);
}
