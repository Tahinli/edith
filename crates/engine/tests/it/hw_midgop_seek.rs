//! Mid-GOP hardware seeks, exact. The VA-API plugin is opened at non-keyframe
//! targets (fresh with `HwSession::open_at`, and on a live session with
//! `HwSession::seek`) and every picture it hands back from there must be the
//! same I420 picture a sequential decode from frame 0 produced for that index.
//!
//! Both fixtures are 60 frames with keyframes at 0 and 30, so every target below
//! is a mid-GOP landing. These run the plugin only (decode, never encode), so
//! they need a built `libengine_hw.so` and a VA-API decoder:
//!
//! ```text
//! cargo build --release --workspace
//! cargo test --release -p engine --test hw_midgop_seek -- --nocapture
//! ```

use crate::hwchild;

use std::path::PathBuf;

use engine::hw::HwSession;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// One picture as its three planes, back to back.
type Picture = Vec<u8>;

/// Every picture of `name` in display order, from one sequential decode.
fn sequential(name: &str) -> Vec<Picture> {
    let mut hw = HwSession::open(&asset(name)).expect("hardware decode plugin and driver");
    let mut out = Vec::new();
    while let Some((y, u, v, _, _)) = hw.next_frame().expect("sequential hardware decode") {
        out.push([y, u, v].concat());
    }
    out
}

/// The `count` pictures from `first` on, in the order a session hands them out.
fn take(hw: &mut HwSession, count: usize) -> Vec<Picture> {
    (0..count)
        .map(|_| {
            let (y, u, v, _, _) = hw
                .next_frame()
                .expect("hardware decode")
                .expect("a picture from the seek target");
            [y, u, v].concat()
        })
        .collect()
}

/// Asserts that a fresh open at each target, and a seek on a live session to
/// each target, hands back the sequential decode's pictures exactly.
fn check_midgop(name: &str, targets: &[usize]) {
    // `require()` has already panicked if a render node exists without
    // `hw-child`, so a session that does not open here means no usable VA-API
    // driver or profile: a skip, as on a machine with no GPU.
    if HwSession::open(&asset(name)).is_none() {
        println!("skipped: no VA-API driver");
        return;
    }
    let reference = sequential(name);
    assert_eq!(reference.len(), 60, "{name}: the fixture is 60 frames");
    let path = asset(name);
    const RUN: usize = 4;

    for &t in targets {
        let mut hw = HwSession::open_at(&path, t as u32)
            .unwrap_or_else(|| panic!("{name}: no hardware session at frame {t}"));
        let got = take(&mut hw, RUN.min(reference.len() - t));
        for (i, pic) in got.iter().enumerate() {
            assert!(
                *pic == reference[t + i],
                "{name}: open_at({t}) picture {} is not sequential frame {}",
                i,
                t + i
            );
        }
    }

    let mut hw = HwSession::open(&path).expect("hardware session");
    take(&mut hw, 5);
    for &t in targets {
        assert!(hw.seek(t as u32), "{name}: seek to {t} refused");
        let got = take(&mut hw, RUN.min(reference.len() - t));
        for (i, pic) in got.iter().enumerate() {
            assert!(
                *pic == reference[t + i],
                "{name}: seek({t}) picture {} is not sequential frame {} (it is frame {:?})",
                i,
                t + i,
                reference.iter().position(|r| r == pic)
            );
        }
    }
}

/// Non-keyframe targets, before and after the keyframe at 30, and the last
/// GOP's leading and trailing pictures.
const TARGETS: &[usize] = &[1, 7, 15, 29, 31, 45, 59];

#[test]
fn h264_mid_gop_seeks_match_a_sequential_decode() {
    hwchild::require();
    check_midgop("test_h264.mkv", TARGETS);
}

#[test]
fn hevc_mid_gop_seeks_match_a_sequential_decode() {
    hwchild::require();
    check_midgop("test_hevc.mkv", TARGETS);
}
