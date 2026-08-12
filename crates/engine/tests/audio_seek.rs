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

/// The seek lands on the second it was asked for, in *content*, ten and twenty
/// seconds into a file that has a cue table.
///
/// That is not what one symphonia seek gives on a Matroska file: it goes
/// through the cues and lands on the first frame at or past the *cluster* the
/// cue names, which on this fixture is up to 4.99 s later than the request
/// (asking 25.0 s landed the reader at 29.987 s) and on his 5.1 Opus film was
/// 1.47 s at 600 s and 3.39 s at 610 s. Nothing downstream can recover samples
/// the reader is already past, so the film simply played from there while the
/// timeline said otherwise -- sound against picture, after every scrub.
///
/// The chirp is what makes that measurable without a reference decode: 200 Hz
/// rising 160 Hz a second, so the frequency of a quarter second of audio names
/// the second of the file it was cut from.
#[test]
fn a_seek_into_a_cued_matroska_lands_on_its_own_second() {
    for start in [5.0, 12.5, 25.0] {
        let (meta, rx) = AudioSession::open_at(asset("test_seek_chirp.mkv"), start)
            .expect("open")
            .expect("test_seek_chirp.mkv has an audio track");
        let at = chirp_second(&meta, rx.into_iter().flat_map(|c| c.samples));
        assert!(
            (at - start).abs() < 0.1,
            "asked {start}s of the chirp, heard {at:.3}s"
        );
    }
}

/// ...and the *second* segment of one session lands too. Two clips of one file
/// is two seeks on one reader, which is where symphonia's mkv reader gives up
/// altogether: it hands out the frames left over from the first landing and
/// then reads on from wherever its iterator ended up -- measured on his film as
/// the second seek playing the *first cluster* of it, 0.094 s in, whatever it
/// was asked for.
#[test]
fn the_second_segment_of_a_session_lands_too() {
    let (meta, rx) =
        AudioSession::open_segments(asset("test_seek_chirp.mkv"), &[(5.0, 6.0), (22.5, 23.5)])
            .expect("open")
            .expect("test_seek_chirp.mkv has an audio track");
    let channels = usize::from(meta.channels);
    let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
    let second = meta.sample_rate as usize * channels;
    assert_eq!(samples.len(), 2 * second, "two one-second segments");
    for (want, cut) in [(5.0, &samples[..second]), (22.5, &samples[second..])] {
        let at = chirp_second(&meta, cut.iter().copied());
        assert!(
            (at - want).abs() < 0.1,
            "segment from {want}s of the chirp came out as {at:.3}s"
        );
    }
}

/// Where in `test_seek_chirp.mkv` a stream of samples was cut from, by the
/// frequency of its first quarter second: the chirp starts at 200 Hz and rises
/// 160 Hz a second, and zero crossings date it without a reference decode.
fn chirp_second(meta: &engine::audio::AudioMeta, samples: impl Iterator<Item = f32>) -> f64 {
    const WINDOW: f64 = 0.25;
    let channels = usize::from(meta.channels);
    let want = (WINDOW * f64::from(meta.sample_rate)) as usize * channels;
    let first: Vec<f32> = samples.take(want).step_by(channels).collect();
    assert_eq!(first.len(), want / channels, "short of a window to date");
    let crossings = first
        .windows(2)
        .filter(|p| (p[0] < 0.0) != (p[1] < 0.0))
        .count();
    // The window's *midpoint* is what its mean frequency dates.
    (crossings as f64 / 2.0 / WINDOW - 200.0) / 160.0 - WINDOW / 2.0
}
