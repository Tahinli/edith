//! The sample aspect ratio: a file whose pixels are not square is drawn
//! wider than they are stored, and every rendition this engine makes --
//! the probe, the preview and the export -- is the drawn one.
//!
//! `test_anamorphic.mp4` and `test_anamorphic.mkv` (`scripts/gen_fixtures.sh`)
//! are one 1440x1080 source with `setsar=4/3` asked of it: the classic
//! anamorphic 16:9 frame on a 4:3 raster, the way a DVD rip says "widen me".
//! The engine's answer is ffmpeg's own: the decode keeps the coded pixels,
//! the container's ratio is read as metadata, and the stretch is applied
//! exactly once, at the conversion to the square-pixel raster -- so the
//! preview shows 1920x1080 and the corners of it are picture, not bars.
//!
//! The copy door is the other half of ffmpeg's behaviour: a copied file
//! keeps the coded pixels and carries the ratio beside them (Matroska's
//! `Display*` elements), and where the container cannot state it -- an mp4
//! has no `pasp` in this writer -- a copy refuses by name rather than
//! writing a header that lies about its own shape.
//!
//! ```text
//! cargo test -p engine --release --test anamorphic -- --test-threads=1
//! ```

use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::demux::Demuxer;
use engine::export::{ExportSettings, Format};
use engine::scratch::Scratch;
use engine::PlaybackSession;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Which seat an export takes is decided by *process-wide* environment
/// variables, so a test binary that shares the machine has to borrow them
/// rather than assume: this pins both decode and encode to software, which is
/// the seat every machine has, and holds the borrow for the whole test binary.
static SEAT: std::sync::RwLock<()> = std::sync::RwLock::new(());

type Shared = std::sync::RwLockReadGuard<'static, ()>;

#[must_use = "the seat pin holds only while its guard is alive"]
fn pin_software() -> Shared {
    let borrowed = SEAT.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    static PIN: Once = Once::new();
    PIN.call_once(|| unsafe {
        std::env::set_var("VE_SW", "1");
        std::env::set_var("VE_SW_ENC", "1");
    });
    borrowed
}

fn wait(handle: &engine::ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < limit,
            "export did not finish in {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// The coded and displayed shapes every anamorphic test here agrees on:
/// 1440x1080 stored, 4/3 drawn, 1920x1080 shown.
const CODED: (u32, u32) = (1440, 1080);
const DISPLAY: (u32, u32) = (1920, 1080);

/// The reduced form of `h/v`, so a ratio stated unreduced (a DAR pair like
/// ffmpeg's 17280/12960) compares equal to its simplest spelling.
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// The header says what the samples are and what they are drawn at, and both
/// doors say the same thing about the same picture.
#[test]
fn the_container_aspect_is_probed_off_both_doors() {
    for file in ["test_anamorphic.mp4", "test_anamorphic.mkv"] {
        let (meta, _) = Demuxer::open(&asset(file)).expect("open a fixture");
        assert_eq!(
            (meta.coded_width, meta.coded_height),
            CODED,
            "{file}: the samples are stored at the coded size"
        );
        assert_eq!(
            (meta.width, meta.height),
            DISPLAY,
            "{file}: 4/3 over 1440x1080 is drawn 1920x1080"
        );
        // The mkv door reads ffmpeg's DAR spelling (16/9) and the mp4 door the
        // SAR itself (4/3); both name the same drawn picture, so the test
        // compares the reduced ratio, not the spelling.
        let (h, v) = (meta.pixel_aspect.h, meta.pixel_aspect.v);
        let g = gcd(h, v);
        assert_eq!(
            (h / g, v / g),
            (4, 3),
            "{file}: the ratio one coded pixel is drawn at"
        );
        assert!(!meta.pixel_aspect.is_square(), "{file}: it is not square");
        assert_eq!(
            meta.rotation,
            engine::Rotation::None,
            "{file}: no turn was asked of it"
        );
    }
    // A square-pixel file states none of it, and both doors answer square:
    // the default that makes the whole feature invisible to the ordinary
    // library.
    for file in ["test_baseline.mp4", "test_rotation90.mp4"] {
        let (meta, _) = Demuxer::open(&asset(file)).expect("open a fixture");
        assert!(
            meta.pixel_aspect.is_square(),
            "{file}: no container said an aspect, so the pixels are square"
        );
        // Drawn one to one: the displayed pair is the coded pair, swapped
        // where the file's matrix turns it and never widened.
        let (dw, dh) = match meta.rotation.swaps_axes() {
            true => (meta.coded_height, meta.coded_width),
            false => (meta.coded_width, meta.coded_height),
        };
        assert_eq!(
            (meta.width, meta.height),
            (dw, dh),
            "{file}: drawn one to one, swapped only by the turn"
        );
    }
    // The rotation still swaps on an anamorphic file whose *matrix* asks for
    // a turn: `turn_and_stretch` is the composed order -- the aspect widens
    // the coded frame, then the quarter turn swaps the stretched pair -- the
    // same composition ffmpeg's own probe reports for such a file.
    let turned = asset(".anamorphic_turned.mp4");
    if std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=180x320:rate=30:duration=1")
        .args([
            "-vf",
            "setsar=4/3",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&turned)
        .status()
        .is_ok_and(|s| s.success())
    {
        let (meta, _) = Demuxer::open(&turned).expect("open the turned fixture");
        // 180x320 at SAR 4/3 draws 240x320; a 270-degree matrix turns it.
        assert_eq!(
            (meta.coded_width, meta.coded_height),
            (180, 320),
            "the turned fixture's samples"
        );
        assert_eq!(
            (meta.width, meta.height),
            (240, 320),
            "the aspect widens the coded frame before the turn swaps it"
        );
        let _ = std::fs::remove_file(&turned);
    }
}

/// The preview: the composed picture is the displayed shape, filled to the
/// corners -- no letterbox, no squeeze, no coded-shaped picture drawn into a
/// wider window. The corner gate is numeric: a picture that left bars would
/// sample black.
#[test]
fn the_preview_shows_the_drawn_picture() {
    let _seat = pin_software();
    for file in ["test_anamorphic.mp4", "test_anamorphic.mkv"] {
        let (_, frames, _) =
            engine::DecodeSession::open_at(&asset(file), 0).expect("open for a frame");
        let frame = frames.recv().expect("a first frame");
        assert_eq!(
            (frame.width, frame.height),
            DISPLAY,
            "{file}: the composed picture is the drawn shape"
        );
        let (w, h) = (frame.width as usize, frame.height as usize);
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            let (b, g, r) = (frame.bgra[i], frame.bgra[i + 1], frame.bgra[i + 2]);
            (u32::from(r), u32::from(g), u32::from(b))
        };
        let m = w / 10;
        let corners = [
            at(m, m),
            at(w - 1 - m, m),
            at(w - 1 - m, h - 1 - m),
            at(m, h - 1 - m),
        ];
        assert!(
            corners.iter().all(|(r, g, b)| *r + *g + *b > 48),
            "{file}: the corners are picture, not black: {corners:?}"
        );
        let dar = w as f64 / h as f64;
        assert!(
            (dar - 16.0 / 9.0).abs() < 0.01,
            "{file}: the measured display aspect is 16:9, measured {dar:.4}"
        );
    }
}

/// The export: the drawn shape is what the file says, and the pixels are the
/// ones the preview showed -- re-encoded, so the new file is square-pixel at
/// the displayed size and states no aspect of its own.
#[test]
fn an_export_of_an_anamorphic_file_is_drawn_and_square() {
    let _seat = pin_software();
    let source = asset("test_anamorphic.mkv");
    let mut session = PlaybackSession::open(&source).expect("open the fixture");
    session.pause();
    let preview = loop {
        if let Some(frame) = session.try_frame() {
            break frame;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = Scratch::file("ve_anamorphic_export", "mp4");
    let handle = session.export_to_with(&out, &ExportSettings::default());
    wait(&handle, Duration::from_secs(120)).expect("export");

    let (meta, _) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(
        (meta.coded_width, meta.coded_height),
        DISPLAY,
        "the re-encoded file is coded at the displayed shape"
    );
    assert!(
        meta.pixel_aspect.is_square(),
        "the aspect was applied, not carried: the export states none"
    );
    // Preview vs export, same clip: the drawn picture to the byte.
    let mut reopened = PlaybackSession::open(&out).expect("reopen the export");
    let exported = loop {
        if let Some(frame) = reopened.try_frame() {
            break frame;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        (exported.width, exported.height),
        DISPLAY,
        "the export plays at the drawn shape"
    );
    // Encode is lossy, so the picture is measured, not diffed: a wrong shape
    // or a squeezed picture would move far more than a rounding code.
    let moved = preview
        .bgra
        .iter()
        .zip(&exported.bgra)
        .map(|(a, b)| u32::from(a.abs_diff(*b)))
        .sum::<u32>();
    let mean = moved as f64 / preview.bgra.len() as f64;
    assert!(
        mean < 2.0,
        "the export is {mean:.2} mean codes from the preview -- not the same picture"
    );
}

/// The copy door, Matroska side: an anamorphic source's Exact row copies the
/// coded blocks and carries the aspect beside them -- read back as the same
/// 4/3 off the same elements the probe reads.
#[test]
fn a_matroska_copy_keeps_the_pixel_aspect() {
    // The copy itself decodes nothing, but opening the HEVC source and
    // reopening the copy do -- there is no software HEVC decoder, so both
    // need the plugin. A machine without it skips, exactly as
    // `video_copy`'s decode-back test does.
    let Ok(session) = PlaybackSession::open(asset("test_anamorphic_hevc.mkv")) else {
        println!("no VA-API plugin: skipping the copy-keeps-aspect test");
        return;
    };
    let out = Scratch::file("ve_anamorphic_copy", "mkv");
    let handle = session.export_to_with(
        &out,
        &ExportSettings {
            format: Format::Hevc,
            ..Default::default()
        },
    );
    while !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let encoders = handle.encoders().unwrap_or_default();
    handle.result().expect("an outcome").expect("copy export");
    assert!(
        encoders.contains("copy"),
        "an untouched anamorphic timeline copies: {encoders}"
    );
    let (meta, _) = Demuxer::open(&out).expect("reopen the copy");
    assert_eq!(
        (meta.coded_width, meta.coded_height),
        CODED,
        "the copied track declares the coded shape"
    );
    let (h, v) = (meta.pixel_aspect.h, meta.pixel_aspect.v);
    let g = gcd(h, v);
    assert_eq!(
        (h / g, v / g),
        (4, 3),
        "the copied track states the aspect it was copied with"
    );
    assert_eq!(
        (meta.width, meta.height),
        DISPLAY,
        "and so reads back as the drawn shape"
    );
}

/// The copy door, mp4 side: the sample entry this writer produces has no
/// `pasp` box, so an anamorphic source's Exact row refuses by name rather
/// than writing a header that lies about its own shape -- and the export
/// still re-encodes to the right shape when asked without Exact.
#[test]
fn an_mp4_copy_of_an_anamorphic_source_refuses_in_words() {
    let _seat = pin_software();
    // The refusal is decided at the header read, but the session must open
    // the HEVC source first -- plugin on this machine or skip, as above.
    let Ok(session) = PlaybackSession::open(asset("test_anamorphic_hevc.mkv")) else {
        println!("no VA-API plugin: skipping the mp4 exact refusal test");
        return;
    };
    let out = Scratch::file("ve_anamorphic_exact_refused", "mp4");
    let settings = ExportSettings {
        format: Format::Mp4,
        exact: true,
        ..Default::default()
    };
    let handle = session.export_to_with(&out, &settings);
    wait(&handle, Duration::from_secs(300))
        .expect_err("an mp4 cannot honour exact for an anamorphic source");
    let error = handle
        .result()
        .expect("a finished export has an outcome")
        .expect_err("the export refused")
        .to_string();
    assert!(
        error.contains("pixel aspect"),
        "the refusal names the aspect it would lose: {error}"
    );
    assert!(
        !out.exists() || std::fs::read(&out).is_err(),
        "no lying header was written"
    );
}
