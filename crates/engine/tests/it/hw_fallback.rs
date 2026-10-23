//! A hardware decode error part-way through a span hands the rest of the span
//! to the software seat, at the picture it failed on.
//!
//! The error is injected (`engine::hw::inject_failure_after`) because a real
//! one needs a hung GPU ring. The test compares the pictures delivered after
//! the failure with a pure software decode of the same range (the hook set to
//! 0 makes the hardware refuse its first picture, so the whole span runs in
//! software), and checks the stream is complete: before the fallback existed it
//! stopped at the failure. One test, because the hook is process-wide.
//!
//! Needs a built `libengine_hw.so` and a VA-API decoder; without them it skips.

use crate::hwchild;

use std::path::PathBuf;

use engine::decode::DecodeSession;
use engine::hw::{HwSession, inject_failure_after};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn span(path: &PathBuf, start: u32, end: u32) -> Vec<(u32, Vec<u8>)> {
    let (_, rx, _) = DecodeSession::open_range(path, start, end).expect("open");
    rx.iter().map(|f| (f.index, f.bgra)).collect()
}

#[test]
fn a_hardware_error_mid_span_continues_in_software_with_the_same_frames() {
    hwchild::require();
    for (name, start, end, fail_after) in [
        ("test_25fps.mp4", 0, 50, 17),
        ("test_25fps.mp4", 10, 40, 5),
        ("test_25fps.mp4", 0, 50, 49),
        ("test_h264.mkv", 0, u32::MAX, 20),
        ("test_hevc.mp4", 0, u32::MAX, 12),
    ] {
        let path = asset(name);
        inject_failure_after(None);
        if HwSession::open(&path).is_none() {
            eprintln!("skipped: no hardware decode for {name}");
            return;
        }
        // Reference: software for the whole span.
        inject_failure_after(Some(0));
        let software = span(&path, start, end);
        // Hardware that dies after `fail_after` pictures.
        inject_failure_after(Some(fail_after));
        let fell_back = span(&path, start, end);
        inject_failure_after(None);

        assert!(!software.is_empty(), "{name}: reference decoded nothing");
        assert_eq!(
            fell_back.len(),
            software.len(),
            "{name}: the stream ended at the failure instead of continuing"
        );
        let first = software[0].0;
        let at = first + fail_after as u32;
        for (got, want) in fell_back.iter().zip(&software) {
            assert_eq!(got.0, want.0, "{name}: frame order");
            if got.0 >= at {
                assert!(
                    got.1 == want.1,
                    "{name}: frame {} differs from a software decode",
                    got.0
                );
            }
        }
    }
}
