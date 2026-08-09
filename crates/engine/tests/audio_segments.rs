//! `AudioSession::open_segments` must pour source windows back to back: each
//! segment equal to the matching window of a plain `open_at`, `start_sample`
//! continuous across the join, and the trims exact down to a sub-packet clip.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use engine::audio::{AudioChunk, AudioSession};

const RATE: u64 = 44100;
const CHANNELS: usize = 2;
/// One AAC-LC packet, in frames per channel.
const PACKET: u64 = 1024;

fn asset() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4")
}

/// Drains a session flat, asserting chunks are contiguous, and reports the
/// first chunk's `start_sample` plus how many chunks arrived.
fn drain(rx: Receiver<AudioChunk>) -> (Vec<f32>, u64, usize) {
    let (mut samples, mut first, mut chunks) = (Vec::new(), None, 0usize);
    for AudioChunk {
        start_sample,
        samples: s,
    } in rx
    {
        assert!(!s.is_empty(), "empty chunk {chunks}");
        let want = first.unwrap_or(start_sample) + (samples.len() / CHANNELS) as u64;
        assert_eq!(start_sample, want, "gap or overlap before chunk {chunks}");
        first.get_or_insert(start_sample);
        samples.extend(s);
        chunks += 1;
    }
    (samples, first.unwrap_or(0), chunks)
}

fn segments(segs: &[(f64, f64)]) -> (Vec<f32>, u64, usize) {
    let (_, rx) = AudioSession::open_segments(asset(), segs)
        .expect("open")
        .expect("test_av.mp4 has an audio track");
    drain(rx)
}

/// The first `frames` per channel of a plain `open_at(start)`.
fn window(start: f64, frames: u64) -> Vec<f32> {
    let (_, rx) = AudioSession::open_at(asset(), start).unwrap().unwrap();
    let (mut samples, _, _) = drain(rx);
    assert!(samples.len() >= frames as usize * CHANNELS, "short window");
    samples.truncate(frames as usize * CHANNELS);
    samples
}

/// Largest per-sample difference, and where it is.
fn max_diff(a: &[f32], b: &[f32]) -> (f32, usize) {
    assert_eq!(a.len(), b.len(), "length mismatch");
    a.iter()
        .zip(b)
        .enumerate()
        .map(|(i, (x, y))| ((x - y).abs(), i))
        .fold((0.0, 0), |m, d| if d.0 > m.0 { d } else { m })
}

/// Perceptual noise substitution reseeds per decoder, so a segment can never be
/// promised bit-exact against another run (engine/src/audio.rs docs, ledger).
fn assert_close(got: &[f32], want: &[f32], what: &str) {
    let (diff, at) = max_diff(got, want);
    eprintln!(
        "{what}: max abs diff {diff:e} at sample {at} of {}",
        got.len()
    );
    assert!(diff < 1e-3, "{what}: max abs diff {diff} at sample {at}");
}

#[test]
fn two_segments_concat() {
    let (got, first, chunks) = segments(&[(1.0, 2.0), (3.0, 4.0)]);
    // `drain` already proved every chunk butts against the previous one, so a
    // full-length run is a joint with no gap and no overlap.
    assert_eq!(first, RATE, "numbering starts at the first segment's start");
    assert_eq!(
        got.len() as u64 / CHANNELS as u64,
        2 * RATE,
        "{chunks} chunks"
    );

    let want: Vec<f32> = [window(1.0, RATE), window(3.0, RATE)].concat();
    assert_close(&got, &want, "two segments");
}

#[test]
fn tiny_segment_is_one_frame_long() {
    // A single 30 fps video frame's worth of audio: 1470 samples per channel,
    // decoded from two packets and trimmed at both.
    let (got, first, chunks) = segments(&[(1.0, 1.0 + 1.0 / 30.0)]);
    assert_eq!(first, RATE);
    assert_eq!(got.len() as u64 / CHANNELS as u64, 1470, "{chunks} chunks");
    assert_close(&got, &window(1.0, 1470), "one video frame");
}

#[test]
fn sub_packet_segment_trims_one_buffer_at_both_ends() {
    // 441 samples starting 44100 in: with priming that is media 45124..45565,
    // both inside packet 45 ([45056, 46080)), so the head trim and the tail trim
    // hit the same decoded buffer.
    let (got, first, chunks) = segments(&[(1.0, 1.01)]);
    assert_eq!(first, RATE);
    assert_eq!(got.len() as u64 / CHANNELS as u64, 441);
    assert_eq!(chunks, 1, "both trims must land in one decoded buffer");
    assert_close(&got, &window(1.0, 441), "sub-packet segment");
}

#[test]
fn segment_past_the_end_is_capped() {
    // test_av.mp4 is 5 s: the ask runs 55 s past the track and must simply stop.
    let (got, first, _) = segments(&[(4.0, 60.0)]);
    assert_eq!(first, 4 * RATE);
    let frames = got.len() as u64 / CHANNELS as u64;
    assert!(
        (RATE..=RATE + PACKET).contains(&frames),
        "{frames} frames for the last second (+0..={PACKET} tail padding)"
    );
}

#[test]
fn no_segments_ends_clean() {
    let (meta, rx) = AudioSession::open_segments(asset(), &[])
        .expect("open")
        .expect("test_av.mp4 has an audio track");
    assert_eq!(meta.sample_rate as u64, RATE);
    assert_eq!(
        rx.into_iter().count(),
        0,
        "an empty edit list decodes nothing"
    );
}
