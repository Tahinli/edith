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

/// A segment naming no source is a gap, and a gap is *synthesised silence* in
/// the same stream -- real chunks, contiguous with the audible ones (`drain`
/// asserts that), so the master clock keeps counting through the hole instead
/// of stalling on it.
#[test]
fn a_gap_segment_is_silence_of_its_own_length() {
    let sources = [asset()];
    let segs = [
        (Some(0), 0.0, 0.5),
        (None, 0.0, 1.0), // one second of hole
        (Some(0), 2.0, 2.5),
    ];
    let (_, rx) = AudioSession::open_multi_segments(&sources, &segs)
        .expect("open")
        .expect("test_av.mp4 has an audio track");
    let (samples, first, _) = drain(rx);
    assert_eq!(first, 0, "the run still starts at audible zero");

    let frames = samples.len() as u64 / CHANNELS as u64;
    let want = 2 * RATE / 2 + RATE;
    assert!(
        frames.abs_diff(want) <= PACKET,
        "{frames} frames for {want} asked for: a gap must cost exactly its length"
    );
    // The hole itself is digital silence, and what surrounds it is not.
    let hole = (RATE / 2) as usize * CHANNELS..(RATE / 2 + RATE) as usize * CHANNELS;
    assert!(
        samples[hole.clone()].iter().all(|s| *s == 0.0),
        "the gap must be silent"
    );
    assert!(
        samples[..hole.start].iter().any(|s| s.abs() > 1e-3)
            && samples[hole.end..].iter().any(|s| s.abs() > 1e-3),
        "the clips around it must not be"
    );

    // Nothing but gap is a valid run of silence; the meta still comes from the
    // source, because that is what the device was opened with.
    let (meta, rx) = AudioSession::open_multi_segments(&sources, &[(None, 0.0, 0.25)])
        .expect("open")
        .expect("meta comes from source 0 even with nothing to decode");
    assert_eq!(meta.sample_rate as u64, RATE);
    let (samples, _, _) = drain(rx);
    assert!(samples.iter().all(|s| *s == 0.0));
    assert!(
        (samples.len() as u64 / CHANNELS as u64).abs_diff(RATE / 4) <= PACKET,
        "quarter second of silence"
    );
}

/// The copy side of a gap: there is no AAC encoder here, so the hole is copied
/// as hand-written silent packets -- real access units, one 1024-frame stts
/// entry each, the rounding debt carried through them like any other segment.
/// Every packet after the gap therefore still lands at its timeline position.
#[test]
fn a_copied_gap_is_packets_of_hand_written_silence() {
    let sources = [asset()];
    let plain = [(Some(0), 0.0, 1.0), (Some(0), 2.0, 3.0)];
    let gapped = [(Some(0), 0.0, 1.0), (None, 0.0, 1.0), (Some(0), 2.0, 3.0)];

    let (_, without) = AudioSession::copy_multi_segments(&sources, &plain)
        .unwrap()
        .unwrap();
    let (_, with) = AudioSession::copy_multi_segments(&sources, &gapped)
        .unwrap()
        .unwrap();
    let total = |p: &[engine::AacPacket]| p.iter().map(|p| u64::from(p.samples)).sum::<u64>();
    assert!(
        with.iter().all(|p| u64::from(p.samples) == PACKET),
        "gap or not, every AAC-LC packet is 1024 frames"
    );
    let gap = total(&with) - total(&without);
    assert!(
        gap.abs_diff(RATE) <= PACKET / 2,
        "the hole is {gap} frames of silence for a second of timeline"
    );
    // The audible packets are the same bytes in the same order: the silence was
    // inserted between them, not spliced through them.
    let audible: Vec<&Vec<u8>> = with
        .iter()
        .map(|p| &p.bytes)
        .filter(|b| b.len() > 7)
        .collect();
    let want: Vec<&Vec<u8>> = without.iter().map(|p| &p.bytes).collect();
    assert_eq!(audible, want, "not one byte of audio moved");
    // Stereo silence is the 7-byte block, and it sits where the hole is.
    let silent = |p: &engine::AacPacket| p.bytes == [0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0E];
    let run: Vec<usize> = with
        .iter()
        .enumerate()
        .filter(|(_, p)| silent(p))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        run.len() as u64,
        gap / PACKET,
        "the gap is that many silent packets"
    );
    assert!(
        run.windows(2).all(|w| w[1] == w[0] + 1) && run[0] > 0,
        "and they are one interior run, at {run:?}"
    );

    // A gap at the *head* is packets of its own -- its own length plus the one
    // more a reader drops as priming. Without that one the drop would come out
    // of the hole and everything after it would play a packet early.
    let (_, leading) =
        AudioSession::copy_multi_segments(&sources, &[(None, 0.0, 0.5), (Some(0), 0.0, 1.0)])
            .unwrap()
            .unwrap();
    let head = leading.iter().take_while(|p| silent(p)).count() as u64;
    assert!(
        (head * PACKET).abs_diff(RATE / 2 + PACKET) <= PACKET / 2,
        "{head} silent packets for half a second of leading hole plus priming"
    );
    // ...and only a *leading* hole gets it: the interior one above did not.
    assert_eq!(
        run.len() as u64 * PACKET,
        gap,
        "an interior gap is its own length exactly"
    );
}
