//! The stream's matrix reaching the picture a player shows, through the door
//! the window itself opens ([`PlaybackSession`]) rather than through the
//! converter alone.
//!
//! Two fixtures, one picture: `test_bt601.mkv` and `test_h264.mkv` are the same
//! `testsrc2` at 1280x720 through the same encoder, and their first frames were
//! byte-identical while every file was converted as BT.601 -- the FNV constant
//! below is that shared frame, measured before this change. What separates them
//! now is only what they *say*: the first tags SMPTE 170M in its container, the
//! second tags nothing and is 720 lines, so the heuristic calls it BT.709.
//!
//! ```text
//! cargo test -p engine --release --test matrix_render -- --test-threads=1
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::colorspace::Matrix;

/// Frame 0 of either fixture as the engine converted it before it read a single
/// colour tag: BT.601 limited, both files.
const LEGACY_601_FRAME: u64 = 0x872e_4f9c_4165_0c9e;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x100_0000_01b3)
    })
}

/// The first frame of `name`, and the matrix the engine resolved for it. Pinned
/// to the software decoder so the bytes are the same on a machine with a VA-API
/// plugin and one without -- hence `--test-threads=1`.
fn frame0(name: &str) -> (Matrix, Vec<u8>) {
    unsafe { std::env::set_var("VE_SW", "1") };
    let mut session = PlaybackSession::open(asset(name)).expect("open");
    session.set_gain(0.0);
    let matrix = session.meta().color.matrix;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(frame) = session.try_frame() {
            assert_eq!(frame.index, 0, "{name}: not the first frame");
            assert_eq!((frame.width, frame.height), (1280, 720), "{name}");
            return (matrix, frame.bgra);
        }
        assert!(Instant::now() < deadline, "no frame for {name} in 60 s");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_streams_matrix_reaches_the_rendered_frame() {
    let (sd_matrix, sd) = frame0("test_bt601.mkv");
    let (hd_matrix, hd) = frame0("test_h264.mkv");
    assert_eq!(sd_matrix, Matrix::Bt601, "the tagged fixture");
    assert_eq!(hd_matrix, Matrix::Bt709, "the untagged 720-line fixture");

    // The half that must not move: BT.601 content is the picture it always was,
    // to the byte (`convert::bt601_limited_is_the_legacy_matrix` is why).
    assert_eq!(fnv(&sd), LEGACY_601_FRAME, "the BT.601 fixture shifted");

    // ...and the half that must: the same picture, read as HD, is not it.
    assert_ne!(
        fnv(&hd),
        LEGACY_601_FRAME,
        "the BT.709 stream is still being converted as BT.601"
    );

    // A matrix, not a mangling: every pixel moves a little and none of it moves
    // far. (BT.709's red weight is 0.2126 against BT.601's 0.299, so the
    // saturated bars of `testsrc2` are where the shift lands.)
    let moved = sd.iter().zip(&hd).filter(|(a, b)| a != b).count();
    let worst = sd
        .iter()
        .zip(&hd)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .expect("a frame");
    assert!(
        (0.05 * sd.len() as f64) < moved as f64 && worst <= 60,
        "{moved} of {} bytes moved, worst by {worst}",
        sd.len()
    );
}
