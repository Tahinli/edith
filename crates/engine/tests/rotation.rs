//! The display matrix: a file that says "turn me" is turned -- in the preview
//! and in the export, from the same numbers.
//!
//! A phone records a portrait video as a landscape picture plus a `tkhd` matrix
//! saying which way to turn it. Before this the engine read neither: the
//! preview showed a portrait clip on its side and an export re-coded the
//! landscape pixels with a unity matrix, so the file played sideways anywhere
//! -- the one rendition nobody ever wanted.
//!
//! `test_rotation90/180/270.mp4` (`scripts/gen_fixtures.sh`) are one 320x180
//! source of four coloured quadrants written back out with a turn asked for, so
//! a single sampled pixel per corner names the layout and a *wrong direction*
//! cannot pass -- a 180-degree mix-up, a mirror, or "ignore it" each produce a
//! different set of four corners. The layouts asserted here are ffmpeg's own
//! autorotation of each file, measured rather than derived.
//!
//! `test_rotation45.mp4` is the other half of the contract: a matrix that is a
//! rotation but not a quarter turn has no correct rendition here, and it is
//! refused *by name* at the door rather than shown wrong.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::ExportSettings;
use engine::scratch::Scratch;
use engine::{DecodeSession, ExportHandle, PlaybackSession, Rotation};

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

fn out_path(name: &str) -> Scratch {
    Scratch::file(&format!("ve_rotation_{name}"), "mp4")
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
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

/// What colour a decoded BGRA pixel is, by the quadrant it came from. ffmpeg's
/// `red`/`green`/`blue`/`yellow` are far apart enough that the chroma of a
/// 4:2:0 edge never reaches a quadrant's centre, and the labels are what the
/// fixture's own comment says it drew.
fn tint(bgra: &[u8]) -> &'static str {
    let (b, g, r) = (bgra[0], bgra[1], bgra[2]);
    let (r, g, b) = (r > 110, g > 90, b > 110);
    match (r, g, b) {
        (true, true, false) => "yellow",
        (true, false, false) => "red",
        (false, true, false) => "green",
        (false, false, true) => "blue",
        other => panic!("a quadrant centre is not one of the four colours: {other:?}"),
    }
}

/// The four corners of one tightly packed BGRA picture, top-left first and
/// clockwise -- sampled deep enough inside each quadrant that a 4:2:0 boundary
/// cannot colour the sample.
fn corners(width: u32, height: u32, bgra: &[u8]) -> [&'static str; 4] {
    let at = |x: u32, y: u32| {
        let index = ((y * width + x) * 4) as usize;
        tint(&bgra[index..index + 4])
    };
    let (qx, qy) = (width / 4, height / 4);
    [at(qx, qy), at(3 * qx, qy), at(3 * qx, 3 * qy), at(qx, 3 * qy)]
}

/// What the coded picture holds, before anything turns it: this is the layout
/// every expectation below is *not*.
const CODED: [&str; 4] = ["red", "green", "yellow", "blue"];

/// One frame of `path` and the size it came out at: the preview path exactly
/// ([`DecodeSession`] is the same funnel [`PlaybackSession`] paints from).
fn first_frame(path: &Path) -> (u32, u32, Vec<u8>) {
    let (_, frames, _) = DecodeSession::open_at(path, 0).expect("open for a frame");
    let frame = frames.recv().expect("a first frame");
    (frame.width, frame.height, frame.bgra)
}

/// The same four corners ffmpeg's own autorotation of each file produces --
/// measured with `ffmpeg -i f.mp4 -f rawvideo -pix_fmt rgb24 -` on this
/// machine, which is the arbiter a display matrix is read against.
fn expected(file: &str) -> ([u32; 2], Rotation, [&'static str; 4]) {
    match file {
        "test_rotation90.mp4" => (
            [180, 320],
            Rotation::Cw270,
            ["green", "yellow", "blue", "red"],
        ),
        "test_rotation180.mp4" => (
            [320, 180],
            Rotation::Cw180,
            ["yellow", "blue", "red", "green"],
        ),
        "test_rotation270.mp4" => (
            [180, 320],
            Rotation::Cw90,
            ["blue", "red", "green", "yellow"],
        ),
        other => panic!("no expectation for {other}"),
    }
}

/// The header says which way the picture lies: a quarter turn swaps the
/// displayed size and names the turn, and a half turn does neither.
#[test]
fn a_display_matrix_is_read_as_a_quarter_turn() {
    for file in [
        "test_rotation90.mp4",
        "test_rotation180.mp4",
        "test_rotation270.mp4",
    ] {
        let (size, rotation, _) = expected(file);
        let (meta, _) = engine::demux::Demuxer::open(&asset(file)).expect("open a fixture");
        assert_eq!(
            [meta.width, meta.height],
            size,
            "{file}: the *displayed* size is what the engine reports"
        );
        assert_eq!(meta.rotation, rotation, "{file}: which turn the file asks for");
        assert_eq!(
            rotation.swaps_axes(),
            size == [180, 320],
            "{file}: a swapped size and a swapping turn go together"
        );
    }
    // ...and the fixture's own coded picture is the layout none of these are,
    // which is what makes the assertions above about the turn and not about the
    // source. `test_rotation180.mp4` keeps its coded size, so the picture is
    // where the turn is visible.
    let (width, height, bgra) = first_frame(&asset("test_rotation180.mp4"));
    assert_eq!((width, height), (320, 180));
    assert_ne!(corners(width, height, &bgra), CODED, "the turn was made");
}

/// The preview: the picture is turned before it is painted, at the size the
/// turn gives it.
#[test]
fn the_preview_shows_a_turned_file_upright() {
    let _seat = pin_software();
    for file in [
        "test_rotation90.mp4",
        "test_rotation180.mp4",
        "test_rotation270.mp4",
    ] {
        let (size, _, corners_expected) = expected(file);
        let (width, height, bgra) = first_frame(&asset(file));
        assert_eq!(
            [width, height],
            size,
            "{file}: the painted picture is the displayed size"
        );
        assert_eq!(
            corners(width, height, &bgra),
            corners_expected,
            "{file}: every quadrant landed where the turn puts it"
        );
    }
}

/// The export: the turn is baked into the pixels, so the file plays upright
/// everywhere -- which means the output is portrait *and* declares no matrix of
/// its own. A file that kept the source's matrix and the source's landscape
/// pixels would satisfy neither half.
#[test]
fn an_export_of_a_turned_file_is_upright_and_portrait() {
    let _seat = pin_software();
    let source = asset("test_rotation90.mp4");
    let mut session = PlaybackSession::open(&source).expect("open the fixture");
    session.pause();
    assert_eq!(
        [session.meta().width, session.meta().height],
        [180, 320],
        "the timeline is the displayed shape, not the coded one"
    );
    let out = out_path("upright");
    let handle = session.export_to_with(&out, &ExportSettings::default());
    wait(&handle, Duration::from_secs(60)).expect("export");

    let (meta, _) = engine::demux::Demuxer::open(&out).expect("reopen the export");
    assert_eq!(
        [meta.width, meta.height],
        [180, 320],
        "the written file is portrait"
    );
    assert_eq!(
        meta.rotation,
        Rotation::None,
        "and states no matrix: the turn is in the pixels"
    );
    assert_eq!(meta.frame_count, 30, "every timeline frame was written");

    let (width, height, bgra) = first_frame(&out);
    assert_eq!([width, height], [180, 320]);
    assert_eq!(
        corners(width, height, &bgra),
        ["green", "yellow", "blue", "red"],
        "the exported pixels are the ones the preview showed"
    );
}

/// A matrix that is not a quarter turn is refused by name at the door, with the
/// angle in the sentence -- the honest answer to a picture this engine has no
/// way to show right. The capability really is absent: nothing here turns by an
/// arbitrary angle ([`engine::scale::rotate_i420_90s`] takes whole quarters),
/// and the refusal is what says so instead of a sideways picture.
#[test]
fn a_matrix_that_is_not_a_quarter_turn_is_refused_by_name() {
    let error = match engine::demux::Demuxer::open(&asset("test_rotation45.mp4")) {
        Ok(_) => panic!("45 degrees is not a quarter turn, and opening it must not succeed"),
        Err(error) => error,
    };
    assert!(
        engine::UnsupportedRotation::is_it(&error),
        "the refusal is the named one, not a generic failure: {error}"
    );
    let said = error.to_string();
    assert!(
        said.contains("45.0°") && said.contains("quarter turn"),
        "the refusal names the angle it cannot make: {said}"
    );
}
