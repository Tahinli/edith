//! `AudioSession::copy_segments` hands out the source's own AAC bytes — nothing
//! is decoded and nothing is re-encoded. What has to hold: the bytes are
//! verbatim, the run starts one packet early (the source priming packet), and
//! the per-segment packet counts follow the bresenham rounding so a cut list
//! does not walk out of lip sync.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use engine::audio::{AacPacket, AudioSession};
use mp4::{MediaType, Mp4Reader};

const RATE: f64 = 44100.0;
/// Frames per channel in one AAC-LC packet.
const PACKET: usize = 1024;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn copy(segs: &[(f64, f64)]) -> Vec<AacPacket> {
    AudioSession::copy_segments(asset("test_av.mp4"), segs)
        .expect("copy")
        .expect("test_av.mp4 has an audio track")
        .1
}

/// The AAC track's sample bytes for 1-based `ids`, read straight from the
/// demuxer — the reference the copy must match byte for byte.
fn raw(ids: impl IntoIterator<Item = u32>) -> Vec<Vec<u8>> {
    let file = File::open(asset("test_av.mp4")).unwrap();
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
                .unwrap_or_else(|| panic!("no sample {id}"))
                .bytes
                .to_vec()
        })
        .collect()
}

fn assert_same_bytes(got: &[AacPacket], want: &[Vec<u8>], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: packet count");
    for (i, (p, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(&p.bytes, w, "{what}: packet {i} ({} bytes)", p.bytes.len());
        assert_eq!(p.samples as usize, PACKET, "{what}: packet {i} duration");
    }
}

#[test]
fn whole_track_copy_is_byte_identical() {
    // The fixture is 5 s, so this asks for all of it: the copy must be the raw
    // sample table from id 1 (priming packet included) with nothing rewritten.
    let packets = copy(&[(0.0, 5.0)]);
    let ids = 1..=packets.len() as u32;
    eprintln!("copied {} packets", packets.len());
    assert_same_bytes(&packets, &raw(ids), "0..5 s");

    // Audible length (priming dropped by the reader) inside half a packet of 5 s.
    let audible = ((packets.len() - 1) * PACKET) as f64;
    assert!(
        (audible - 5.0 * RATE).abs() < PACKET as f64,
        "{audible} samples for 5 s"
    );
}

#[test]
fn two_segments_match_the_bresenham_prediction() {
    let packets = copy(&[(0.0, 1.0), (2.0, 3.0)]);
    // One second is 43.07 packets: the first segment copies 43 and owes 68
    // samples, so the second is measured against 44100 + 68 = 43.13 -> 43 too.
    let n1 = (RATE / PACKET as f64).round();
    let err = n1 * PACKET as f64 - RATE;
    let n2 = ((RATE - err) / PACKET as f64).round();
    assert_eq!((n1, n2), (43.0, 43.0), "arithmetic changed, not the code");
    assert_eq!(packets.len(), 1 + n1 as usize + n2 as usize);

    // Segment 1 is ids 1 (priming) .. 1 + 43; the fixture carries the standard
    // 1024 priming (see tests/audio_segments.rs), so 2.0 s is media sample
    // 88200 + 1024 = 89224, which packet 88 ([89088, 90112)) holds.
    let split = 1 + n1 as usize;
    assert_same_bytes(&packets[..split], &raw(1..=split as u32), "0..1 s");
    assert_same_bytes(
        &packets[split..],
        &raw(88..88 + n2 as u32),
        "2..3 s starting at packet 88",
    );

    // Two seconds asked for, two seconds copied, to inside half a packet.
    let audible = ((packets.len() - 1) * PACKET) as f64;
    assert!(
        (audible - 2.0 * RATE).abs() < PACKET as f64,
        "{audible} samples for 2 s over one join"
    );
}

#[test]
fn nothing_to_copy_is_a_clean_none() {
    assert!(
        AudioSession::copy_segments(asset("test_av.mp4"), &[])
            .unwrap()
            .is_none(),
        "an empty edit list copies nothing"
    );
    assert!(
        AudioSession::copy_segments(asset("test_baseline.mp4"), &[(0.0, 1.0)])
            .unwrap()
            .is_none(),
        "a file with no AAC track is a valid silent session"
    );
}
