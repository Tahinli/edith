//! `AudioSession::open_at` must produce exactly the tail of a full run: same
//! samples, same absolute `start_sample` numbering, no gaps at the splice.

use std::path::PathBuf;

use engine::audio::{AudioChunk, AudioSession};

const RATE: u32 = 44100;
const CHANNELS: usize = 2;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Drains a session into one flat interleaved buffer, checking as it goes that
/// chunks are contiguous and that the first one lands on `want_start`.
fn drain(start_secs: f64, want_start: u64) -> Vec<f32> {
    let (meta, rx) = AudioSession::open_at(asset("test_av.mp4"), start_secs)
        .expect("open")
        .expect("test_av.mp4 has an audio track");
    assert_eq!((meta.sample_rate, meta.channels as usize), (RATE, CHANNELS));

    let mut samples = Vec::new();
    let mut next = want_start;
    for (
        i,
        AudioChunk {
            start_sample,
            samples: s,
        },
    ) in rx.into_iter().enumerate()
    {
        assert_eq!(start_sample, next, "chunk {i} start at {start_secs}s");
        assert!(!s.is_empty(), "empty chunk {i} at {start_secs}s");
        next += (s.len() / CHANNELS) as u64;
        samples.extend(s);
    }
    samples
}

/// Largest per-sample difference, and where it is.
fn max_diff(a: &[f32], b: &[f32]) -> (f32, usize) {
    a.iter()
        .zip(b)
        .enumerate()
        .map(|(i, (x, y))| ((x - y).abs(), i))
        .fold((0.0, 0), |m, d| if d.0 > m.0 { d } else { m })
}

#[test]
fn open_at_zero_matches_open() {
    let (mut full, mut at_zero) = (Vec::new(), Vec::new());
    for chunk in AudioSession::open(asset("test_av.mp4")).unwrap().unwrap().1 {
        full.extend(chunk.samples);
    }
    for chunk in AudioSession::open_at(asset("test_av.mp4"), 0.0)
        .unwrap()
        .unwrap()
        .1
    {
        at_zero.extend(chunk.samples);
    }
    assert_eq!(full.len(), at_zero.len());
    assert_eq!(full, at_zero, "open() must be open_at(0.0)");
}

#[test]
fn open_at_two_seconds_is_the_tail_of_a_full_run() {
    let full = drain(0.0, 0);
    let target = 2 * RATE as u64;
    let tail = drain(2.0, target);

    let want = &full[target as usize * CHANNELS..];
    assert_eq!(tail.len(), want.len(), "seeked run is a different length");
    if tail == want {
        eprintln!("bit-exact: {} samples", tail.len());
        return;
    }
    // Not bit-exact, and it never will be: perceptual noise substitution draws
    // from an LCG seeded once per decoder (symphonia-codec-aac cpe.rs:32,44), so
    // a run that skipped 86 packets draws different noise in PNS bands forever.
    // The MDCT overlap the pre-roll exists for *is* exact — the first packets
    // after the splice match sample for sample.
    let (diff, at) = max_diff(&tail, want);
    eprintln!("max abs diff {diff:e} at sample {at} of {}", tail.len());
    assert!(diff < 1e-3, "max abs diff {diff} at sample {at}");
}

#[test]
fn open_at_mid_first_packet_is_exact() {
    // 500 samples in: inside packet 1, before any pre-roll exists to walk back
    // to, and it forces the drain-inside-a-chunk trim path.
    let full = drain(0.0, 0);
    let start = 500u64;
    let tail = drain(start as f64 / RATE as f64, start);
    let want = &full[start as usize * CHANNELS..];
    assert_eq!(tail.len(), want.len());
    assert_eq!(tail, want, "no decoder state to diverge from at packet 1");
}

#[test]
fn open_at_past_duration_ends_clean() {
    let tail = drain(60.0, 60 * RATE as u64);
    assert!(
        tail.is_empty(),
        "{} samples past the end of a 5 s file",
        tail.len()
    );
}
