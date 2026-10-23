//! The chroma and depth edith does not decode are refused at probe, by name, before
//! anything is exported: `Demuxer::open` (what the probe and import read) and
//! `decode::probe` (what a front-end asks for the decoder) say the same words.
//!
//! One case per format, each with the conversion that makes the file usable.
//! AV1 4:2:2, 4:4:4 and 12-bit, and HEVC 4:2:2 at 10-bit, are carried now, so
//! they probe clean.
//!
//! ```text
//! cargo test -p engine --release --test probe_refusal
//! ```

use std::path::PathBuf;
use std::process::Command;

use engine::decode::probe;
use engine::demux::Demuxer;
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

/// A 10-bit RExt 4:2:2 HEVC clip (`hvcC` chroma 2, depth 10), encoded with ffmpeg's
/// libx265 into a scratch directory. `assets/` is gitignored, so the fixture the
/// gen script writes is not on every checkout; the test must not depend on it.
fn hevc_422_10_clip() -> Scratch {
    let clip = Scratch::file("probe_hevc422_10", "mp4");
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=64x64:rate=30:duration=0.1")
        .args(["-frames:v", "3", "-c:v", "libx265", "-pix_fmt", "yuv422p10le"])
        .args(["-x265-params", "log-level=error"])
        .arg(&*clip)
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    clip
}

#[test]
fn av1_422_444_and_12_bit_open_at_probe() {
    // Carried since the software seat averages their chroma to 4:2:0 and
    // narrows 12 bits, so the probe finds nothing to refuse.
    for name in ["test_av1_422.mkv", "test_av1_444.mkv", "test_av1_12bit.mkv"] {
        probe(&asset(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn hevc_422_10_bit_opens_at_probe() {
    // Carried since the seat takes 4:2:2 (chroma averaged to 4:2:0 on the way in),
    // so the probe finds nothing to refuse and names the software seat.
    let clip = hevc_422_10_clip();
    Demuxer::open(&clip).expect("a 10-bit 4:2:2 HEVC file opens");
    probe(&clip).expect("and probes");
}
