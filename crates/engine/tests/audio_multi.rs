//! Multi-source audio: two files poured into one stream. A source join has to
//! behave exactly like a cut inside one file — no gap in `start_sample`, no
//! second priming packet, one shared rounding debt — while each source still
//! contributes its own content, its own packet table and its own length.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use engine::audio::{AacPacket, AudioChunk, AudioSession};
use mp4::{MediaType, Mp4Reader};

const RATE: u64 = 44100;
const CHANNELS: usize = 2;
/// Frames per channel in one AAC-LC packet.
const PACKET: u64 = 1024;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The import-policy pair: same 1280x720@30 / AAC-LC 44.1k stereo, plainly
/// different content (440/880 Hz vs 660/1320 Hz, 5 s vs 4 s).
fn sources() -> Vec<PathBuf> {
    vec![asset("test_av.mp4"), asset("test_av2.mp4")]
}

/// Drains a session flat, asserting chunks are contiguous, and reports the
/// first chunk's `start_sample` plus how many chunks arrived.
/// Every segment names a source: these tests are about joins between files,
/// not about gaps (which name none).
fn named(segs: &[(usize, f64, f64)]) -> Vec<(Option<usize>, f64, f64)> {
    segs.iter().map(|&(s, a, b)| (Some(s), a, b)).collect()
}

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

fn multi(segs: &[(usize, f64, f64)]) -> (Vec<f32>, u64, usize) {
    let segs = named(segs);
    let (_, rx) = AudioSession::open_multi_segments(&sources(), &segs)
        .expect("open")
        .expect("the first source has an audio track");
    drain(rx)
}

/// The first `frames` per channel of a plain single-file `open_at(start)`.
fn window(name: &str, start: f64, frames: u64) -> Vec<f32> {
    let (_, rx) = AudioSession::open_at(asset(name), start).unwrap().unwrap();
    let (mut samples, _, _) = drain(rx);
    assert!(samples.len() >= frames as usize * CHANNELS, "short window");
    samples.truncate(frames as usize * CHANNELS);
    samples
}

/// Perceptual noise substitution reseeds per decoder, so a segment can never be
/// promised bit-exact against another run (engine/src/audio.rs docs, ledger).
fn assert_close(got: &[f32], want: &[f32], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: length");
    let (diff, at) = got
        .iter()
        .zip(want)
        .enumerate()
        .map(|(i, (x, y))| ((x - y).abs(), i))
        .fold((0.0f32, 0), |m, d| if d.0 > m.0 { d } else { m });
    eprintln!(
        "{what}: max abs diff {diff:e} at sample {at} of {}",
        got.len()
    );
    assert!(diff < 1e-3, "{what}: max abs diff {diff} at sample {at}");
}

/// Cycles of the left channel: rising edges through a deadband, so the coding
/// noise around the fixture's 1 Hz volume dip is not counted as a cycle. One
/// second of the 440 Hz source reads ~440, of the 660 Hz one ~660.
fn cycles(samples: &[f32]) -> usize {
    let (mut n, mut high) = (0usize, false);
    for &s in samples.iter().step_by(CHANNELS) {
        if !high && s > 0.05 {
            high = true;
            n += 1;
        } else if high && s < -0.05 {
            high = false;
        }
    }
    n
}

/// The AAC sample bytes for 1-based `ids` of `name`, read straight from the
/// demuxer — the reference a copy must match byte for byte.
fn raw(name: &str, ids: impl IntoIterator<Item = u32>) -> Vec<Vec<u8>> {
    let file = File::open(asset(name)).unwrap();
    let size = file.metadata().unwrap().len();
    let mut reader = Mp4Reader::read_header(BufReader::new(file), size).unwrap();
    let track = reader
        .tracks()
        .values()
        .find(|t| matches!(t.media_type(), Ok(MediaType::AAC)))
        .unwrap()
        .track_id();
    ids.into_iter()
        .map(|id| {
            reader
                .read_sample(track, id)
                .unwrap()
                .unwrap_or_else(|| panic!("no sample {id} in {name}"))
                .bytes
                .to_vec()
        })
        .collect()
}

fn assert_same_bytes(got: &[AacPacket], want: &[Vec<u8>], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: packet count");
    for (i, (p, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(&p.bytes, w, "{what}: packet {i} ({} bytes)", p.bytes.len());
    }
}

fn copy(segs: &[(usize, f64, f64)]) -> Vec<AacPacket> {
    AudioSession::copy_multi_segments(&sources(), &named(segs))
        .expect("copy")
        .expect("the first source has an audio track")
        .1
}

#[test]
fn two_sources_join_without_a_gap() {
    let (got, first, chunks) = multi(&[(0, 1.0, 2.0), (1, 0.0, 1.0)]);
    // `drain` already proved every chunk butts against the previous one, so a
    // full-length run is a joint with no gap and no overlap.
    assert_eq!(first, RATE, "numbering starts at the first segment's start");
    assert_eq!(
        got.len() as u64 / CHANNELS as u64,
        2 * RATE,
        "{chunks} chunks"
    );

    let half = RATE as usize * CHANNELS;
    assert_close(&got[..half], &window("test_av.mp4", 1.0, RATE), "source 0");
    assert_close(&got[half..], &window("test_av2.mp4", 0.0, RATE), "source 1");

    // And the two halves are audibly different files, not the same one twice:
    // an octave-and-a-fifth apart, 440 -> 660 Hz.
    let (c0, c1) = (cycles(&got[..half]), cycles(&got[half..]));
    eprintln!("cycles: {c0} then {c1}");
    assert!(
        (c1 as f64 / c0 as f64 - 1.5).abs() < 0.1,
        "{c0} then {c1} cycles: expected the 440 Hz source then the 660 Hz one"
    );
}

#[test]
fn each_source_ends_where_its_own_track_does() {
    // test_av2 is 4 s: the ask runs a second past it and must simply stop there,
    // then the next segment continues the numbering (`drain` checks that).
    let (got, first, _) = multi(&[(1, 3.5, 5.0), (0, 0.0, 0.5)]);
    assert_eq!(first, 3 * RATE + RATE / 2);
    let frames = got.len() as u64 / CHANNELS as u64;
    assert!(
        (RATE..=RATE + PACKET).contains(&frames),
        "{frames} frames for 0.5 s of a 4 s source plus 0.5 s of the next \
         (+0..={PACKET} tail padding)"
    );
    let tail = got.len() - RATE as usize / 2 * CHANNELS;
    assert_close(
        &got[tail..],
        &window("test_av.mp4", 0.0, RATE / 2),
        "back to source 0",
    );
}

#[test]
fn probe_reads_the_import_policy_fields() {
    let av = AudioSession::probe(asset("test_av.mp4"), 0)
        .expect("probe")
        .expect("test_av.mp4 has audio");
    assert_eq!(av.channels, 2);
    assert_eq!(av.sample_rate, RATE as u32);
    assert_eq!(
        AudioSession::probe(asset("test_av2.mp4"), 0)
            .unwrap()
            .unwrap(),
        av,
        "the fixtures are the matching pair import must accept"
    );
    for silent in ["test_baseline.mp4", "test_mismatch.mp4"] {
        assert!(
            AudioSession::probe(asset(silent), 0).unwrap().is_none(),
            "{silent} has no audio track"
        );
    }
}

#[test]
fn one_rounding_debt_is_carried_across_the_source_joins() {
    // A second is 43.07 packets, so seven seconds copy 43 each and owe 68
    // samples apiece; the eighth crosses half a packet and copies 44 (the unit
    // test `packet_run_rounds_against_the_running_debt` pins that arithmetic).
    // Per segment rounding would copy 43 eight times and start drifting out of
    // sync — and every one of these joins is a *source* join.
    let segs: Vec<_> = (0..8).map(|i| (i % 2, 0.0, 1.0)).collect();
    let packets = copy(&segs);
    assert_eq!(
        packets.len(),
        1 + 7 * 43 + 44,
        "priming packet + seven seconds owing 68 each + the eighth repaying"
    );
    let audible = (packets.len() as u64 - 1) * PACKET;
    assert!(
        (audible as i64 - 8 * RATE as i64).unsigned_abs() < PACKET,
        "{audible} samples for 8 s over seven source joins"
    );
}

#[test]
fn the_join_carries_no_second_priming_packet() {
    let packets = copy(&[(0, 0.0, 1.0), (1, 0.0, 1.0)]);
    let split = 1 + 43;
    assert_eq!(packets.len(), split + 43);
    for p in &packets {
        assert_eq!(p.samples as u64, PACKET, "AAC-LC packets are 1024 frames");
    }
    // Source 0 from id 1 — its own priming packet, the one the reader drops.
    assert_same_bytes(&packets[..split], &raw("test_av.mp4", 1..=44), "source 0");
    // Source 1 from id 2: 0.0 s is media 1024 (its priming), which packet 2
    // holds. Starting at id 1 here would insert a packet the reader keeps.
    assert_same_bytes(
        &packets[split..],
        &raw("test_av2.mp4", 2..45),
        "source 1 at the join",
    );
}

#[test]
fn a_source_that_disagrees_on_rate_or_layout_is_refused() {
    // Import refuses these up front (one output device, one set of parameters);
    // the copy and the decode paths refuse them again rather than mislabel one
    // esds for two different tracks. Stream 1 of the multi-audio fixture is
    // 22.05 kHz mono against the timeline's 44.1 kHz stereo.
    let mixed = [
        (asset("test_av.mp4"), 0),
        (asset("test_multiaudio.mp4"), 1),
    ];
    let segs = [(Some(0), 0.0, 1.0), (Some(1), 0.0, 1.0)];
    // (`.err()`, not `unwrap_err`: neither Ok payload is `Debug`.)
    for e in [
        AudioSession::copy_multi_streams(&mixed, &segs).err(),
        AudioSession::open_multi_streams(&mixed, &segs).err(),
    ]
    .map(|e| e.expect("a source at another rate must be refused"))
    {
        let msg = e.to_string();
        assert!(msg.contains("source 1"), "{msg}");
    }
    // An out-of-range index is an error, not a panic.
    assert!(AudioSession::copy_multi_segments(&sources(), &[(Some(2), 0.0, 1.0)]).is_err());
    assert!(AudioSession::open_multi_segments(&sources(), &[(Some(2), 0.0, 1.0)]).is_err());
}

/// A source with **no** track is not a refusal at all: its segments are the
/// same silence a gap is, in both paths. That is what a silent clip on an audio
/// lane is -- a picture over a hole -- and the timeline lets one in
/// (`PlaybackSession::import`) precisely because these two agree here.
#[test]
fn a_silent_source_is_silence_and_not_a_refusal() {
    let mixed = [asset("test_av.mp4"), asset("test_mismatch.mp4")];
    let segs = [(Some(0), 0.0, 1.0), (Some(1), 0.0, 1.0), (Some(0), 0.0, 1.0)];

    // Decoded: three seconds, the middle one exact zero, and the chunks still
    // arrive contiguous (`drain` asserts that), so the clock is fed through it.
    let (_, rx) = AudioSession::open_multi_segments(&mixed, &segs)
        .expect("a silent source opens")
        .expect("the first source has an audio track");
    let (samples, _, _) = drain(rx);
    let frames = samples.len() / CHANNELS;
    assert!(
        (frames as i64 - 3 * RATE as i64).unsigned_abs() < PACKET,
        "{frames} frames for three seconds"
    );
    let second = |n: usize| {
        let at = |s: usize| (s * RATE as usize * CHANNELS).min(samples.len());
        &samples[at(n)..at(n + 1)]
    };
    assert!(second(0).iter().any(|s| *s != 0.0), "the first second plays");
    assert!(
        second(1).iter().all(|s| *s == 0.0),
        "the silent source is exact silence"
    );
    assert!(second(2).iter().any(|s| *s != 0.0), "and it comes back");

    // Copied: the same shape in packets, so an mp4 export of that timeline is
    // as long as the timeline and quiet where the silent clip is.
    let (params, packets) = AudioSession::copy_multi_segments(&mixed, &segs)
        .expect("a silent source copies")
        .expect("the timeline has an audible source");
    let copied = packets.len() as u64 * PACKET;
    assert!(
        (copied as i64 - 3 * RATE as i64).unsigned_abs() < 2 * PACKET,
        "{copied} samples for three seconds, priming included"
    );
    assert_eq!(params.sample_rate, RATE as u32, "the audible source's track");
    let silence: Vec<_> = packets[44..44 + 43].iter().map(|p| &p.bytes).collect();
    assert!(
        silence.iter().all(|b| b.len() <= 7),
        "the hole is written as the hand-made silent packet, not as audio"
    );
}
