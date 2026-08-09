//! Hardware path checks. These need a built `libengine_hw.so` plus a working
//! VA-API driver, so they are `#[ignore]`d by default. Run them with:
//!
//! ```text
//! cargo build --workspace --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test hw_decode -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The comparison test pins [`DecodeSession`] to the software decoder itself
//! (it sets `VE_SW=1` in-process, hence the required `--test-threads=1`); the
//! hardware side goes through [`HwSession`] directly and can never silently be
//! a fallback.

use std::path::{Path, PathBuf};
use std::time::Instant;

use engine::DecodeSession;
use engine::convert::i420_to_bgra;
use engine::hw::HwSession;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn open_hw(path: &Path) -> HwSession {
    HwSession::open(path).expect("no hardware decode plugin/driver available")
}

/// Decodes the whole file through the plugin, returning (frame count, frame 30 as BGRA).
fn decode_all_hw(name: &str) -> (usize, Vec<u8>) {
    let path = asset(name);
    let mut hw = open_hw(&path);
    let meta = hw.meta().expect("meta");
    assert_eq!(
        (meta.width, meta.height),
        (1280, 720),
        "{name} container dims"
    );

    let mut count = 0usize;
    let mut frame_30 = Vec::new();
    let start = Instant::now();
    while let Some((y, u, v, w, h)) = hw.next_frame().expect("hardware decode") {
        assert_eq!((w, h), (1280, 720), "{name} frame {count} dims");
        if count == 30 {
            frame_30 = i420_to_bgra(y, u, v, w as usize, h as usize);
        }
        count += 1;
    }
    eprintln!(
        "{name}: {count} hardware frames in {:?} ({:?}/frame)",
        start.elapsed(),
        start.elapsed() / count.max(1) as u32
    );
    (count, frame_30)
}

/// The plugin must refuse junk by returning null, never by unwinding into us.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn rejects_unopenable_files() {
    assert!(HwSession::open(Path::new("/definitely/not/here.mp4")).is_none());
    assert!(
        HwSession::open(Path::new("/etc/hostname")).is_none(),
        "not an mp4"
    );
}

#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn hardware_decodes_baseline() {
    let (count, _) = decode_all_hw("test_baseline.mp4");
    assert_eq!(count, 150);
}

#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn hardware_decodes_high_profile() {
    // B-frames: the plugin emits display order (DPB bumping happens inside
    // cros-codecs), so a clean run to EOF is the check we can make headlessly.
    let (count, _) = decode_all_hw("test_high.mp4");
    assert_eq!(count, 150);
}

/// Seeking may only change *when* a picture arrives, never the picture: both
/// backends must land on bytes identical to a linear decode of the same index.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn seek_matches_linear() {
    const TARGET: u32 = 60;
    let path = asset("test_baseline.mp4");

    // Software side first, pinned like the test above. SAFETY: --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let (_, rx) = DecodeSession::open(&path).expect("software open");
    let linear = rx
        .into_iter()
        .find(|f| f.index == TARGET)
        .expect("software frame 60")
        .bgra;

    let start = Instant::now();
    let (_, rx, _cancel) = DecodeSession::open_at(&path, TARGET).expect("software open_at");
    let seeked = rx.into_iter().next().expect("no frame after software seek");
    eprintln!("software seek to {TARGET} took {:?}", start.elapsed());
    assert_eq!(seeked.index, TARGET, "first frame after seek is the target");
    assert_eq!(seeked.bgra, linear, "software seek != software linear");

    let start = Instant::now();
    let mut hw = HwSession::open_at(&path, TARGET).expect("no plugin/driver available");
    let (y, u, v, w, h) = hw
        .next_frame()
        .expect("hardware decode")
        .expect("no frame after hardware seek");
    eprintln!("hardware seek to {TARGET} took {:?}", start.elapsed());
    assert_eq!(
        i420_to_bgra(y, u, v, w as usize, h as usize),
        linear,
        "hardware seek != software linear"
    );
}

/// Drains a bounded session, returning the indices it delivered. The loop ends
/// only on `RecvError`, i.e. the worker closed the channel by itself.
fn range_indices(path: &Path, start: u32, end: u32) -> Vec<u32> {
    let (_, rx, _cancel) = DecodeSession::open_range(path, start, end).expect("open_range");
    let mut got = Vec::new();
    while let Ok(frame) = rx.recv() {
        got.push(frame.index);
    }
    got
}

/// A bounded range must stop on its own — both backends — with `Frame::index`
/// still the absolute source index, and then disconnect rather than hang.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn range_stops_at_end() {
    const START: u32 = 30;
    const END: u32 = 60;
    let path = asset("test_baseline.mp4");
    let want: Vec<u32> = (START..END).collect();

    // Another test in this binary may already have pinned VE_SW; clear it so
    // the hardware half really is hardware. SAFETY: --test-threads=1.
    unsafe { std::env::remove_var("VE_SW") };
    assert_eq!(range_indices(&path, START, END), want, "hardware range");

    unsafe { std::env::set_var("VE_SW", "1") };
    assert_eq!(range_indices(&path, START, END), want, "software range");

    // Degenerate ranges close cleanly instead of decoding to EOF.
    assert!(range_indices(&path, 30, 30).is_empty(), "empty range");
    assert!(range_indices(&path, 60, 30).is_empty(), "inverted range");
    // Past the end is clamped to the frame count.
    assert_eq!(range_indices(&path, 148, 999).len(), 2, "clamped range");
}

/// H.264 8-bit decode is bit-exact by conformance, so the two backends must
/// agree. A mismatch means a driver deviation and is reported, not swallowed.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn hardware_matches_software_on_frame_30() {
    let (_, hw) = decode_all_hw("test_baseline.mp4");

    // Pin the comparison side to software ourselves: without this, running the
    // suite without VE_SW=1 would compare hardware against hardware and pass
    // vacuously. SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };

    let start = Instant::now();
    let (_, rx) = DecodeSession::open(asset("test_baseline.mp4")).expect("software open");
    let sw = rx
        .into_iter()
        .find(|f| f.index == 30)
        .expect("software frame 30")
        .bgra;
    eprintln!("software reached frame 30 in {:?}", start.elapsed());

    assert_eq!(hw.len(), sw.len(), "frame size");
    let diff = hw.iter().zip(&sw).filter(|(a, b)| a != b).count();
    eprintln!("frame 30 differing bytes: {diff} / {}", hw.len());
    assert_eq!(diff, 0, "hardware and software decode disagree");
}
