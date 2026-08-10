//! Files carry more than one audio track — a remux has one per language — so
//! the engine has to *list* them all and open the one it is asked for, instead
//! of taking whichever AAC track it stumbles on first.

use std::path::PathBuf;

use engine::audio::{AudioSession, StreamInfo};

/// Three audio streams: 0 is AAC 44.1k stereo (440/880 Hz, und), 1 is AAC
/// 22.05k mono (220 Hz, fra), 2 is AC-3, which we cannot decode.
/// See `scripts/gen_fixtures.sh`.
const MULTI: &str = "test_multiaudio.mp4";

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Everything the worker produced for one stream, flat.
fn decode(name: &str, stream: usize) -> (engine::AudioMeta, Vec<f32>) {
    let sources = [(asset(name), stream)];
    let (meta, rx) = AudioSession::open_multi_streams(&sources, &[(Some(0), 0.0, f64::INFINITY)])
        .expect("opens")
        .expect("has audio");
    let mut samples = Vec::new();
    for chunk in rx {
        samples.extend_from_slice(&chunk.samples);
    }
    (meta, samples)
}

/// Sign changes, exact zeros ignored: at one tone per channel this is 2f per
/// second per channel.
fn zero_crossings(samples: &[f32]) -> usize {
    let (mut crossings, mut last) = (0, 0f32);
    for &s in samples {
        if s == 0.0 {
            continue;
        }
        if last != 0.0 && (s > 0.0) != (last > 0.0) {
            crossings += 1;
        }
        last = s;
    }
    crossings
}

#[test]
fn probe_streams_lists_every_audio_track_in_file_order() {
    let streams = AudioSession::probe_streams(asset(MULTI)).expect("probes");
    assert_eq!(
        streams,
        vec![
            StreamInfo {
                index: 0,
                codec: "aac".into(),
                channels: 2,
                sample_rate: 44100,
                lang: None, // the `und` every muxer writes by default
                decodable: true,
            },
            StreamInfo {
                index: 1,
                codec: "aac".into(),
                channels: 1,
                sample_rate: 22050,
                lang: Some("fra".into()),
                decodable: true,
            },
            // The one we cannot play is listed all the same, so a picker can
            // grey it rather than pretend the file has two streams. mp4 0.14
            // keeps no fourcc for a sample entry it does not parse, hence the
            // blanks — the row exists, which is the point.
            StreamInfo {
                index: 2,
                codec: "unknown".into(),
                channels: 0,
                sample_rate: 0,
                lang: None,
                decodable: false,
            },
        ]
    );
    // File order, run to run: `Mp4Reader::tracks` is a HashMap, so a careless
    // enumeration would shuffle these two.
    for _ in 0..8 {
        let again = AudioSession::probe_streams(asset(MULTI)).expect("probes");
        assert_eq!(again, streams);
    }
}

#[test]
fn probe_streams_on_one_track_and_on_none() {
    let one = AudioSession::probe_streams(asset("test_av.mp4")).expect("probes");
    assert_eq!(one.len(), 1, "test_av.mp4 has a single audio track");
    assert_eq!(
        (one[0].index, one[0].sample_rate, one[0].channels),
        (0, 44100, 2)
    );
    assert!(one[0].decodable);
    // No audio at all is an empty list, not an error: a silent source is valid.
    assert!(
        AudioSession::probe_streams(asset("test_baseline.mp4"))
            .expect("probes")
            .is_empty()
    );
}

#[test]
fn a_named_stream_decodes_that_stream_and_not_the_first() {
    let (meta, samples) = decode(MULTI, 1);
    assert_eq!((meta.sample_rate, meta.channels), (22050, 1));
    // 220 Hz mono is 2 crossings a period. Measured over the middle half only:
    // the encoder's ramp-in and the container's tail padding are low-level
    // noise that crosses zero far more often than the tone does.
    let middle = &samples[samples.len() / 4..samples.len() * 3 / 4];
    let expected = 2.0 * 220.0 * middle.len() as f64 / 22050.0;
    let crossings = zero_crossings(middle) as f64;
    assert!(
        (crossings - expected).abs() < 0.05 * expected,
        "stream 1 is {crossings} crossings, 220 Hz mono would be {expected}"
    );
    assert!(samples.len() > 22050, "less than a second of stream 1");

    // Stream 0 of the same file is the other one entirely.
    let (meta, _) = decode(MULTI, 0);
    assert_eq!((meta.sample_rate, meta.channels), (44100, 2));
}

#[test]
fn stream_0_stays_exactly_what_the_wrapper_gave() {
    // The default wrapper and stream 0 are the same track, sample for sample.
    let (wrapped, rx) = AudioSession::open(asset(MULTI))
        .expect("opens")
        .expect("audio");
    let mut plain = Vec::new();
    for chunk in rx {
        plain.extend_from_slice(&chunk.samples);
    }
    let (meta, named) = decode(MULTI, 0);
    assert_eq!(wrapped, meta);
    assert_eq!(plain, named);
}

#[test]
fn an_impossible_stream_is_refused_not_guessed() {
    let out_of_range = [(asset(MULTI), 3)];
    let err = AudioSession::open_multi_streams(&out_of_range, &[(Some(0), 0.0, 1.0)])
        .expect_err("stream 3 of 3 does not exist");
    assert!(
        err.to_string().contains("audio stream 3 of 3"),
        "unhelpful refusal: {err}"
    );
    // Stream 2 exists but is AC-3: refused with its own message, not decoded
    // into noise and not silently swapped for a stream that does decode.
    let ac3 = [(asset(MULTI), 2)];
    let err = AudioSession::open_multi_streams(&ac3, &[(Some(0), 0.0, 1.0)])
        .expect_err("AC-3 does not decode here");
    assert!(
        err.to_string().contains("audio stream 2 is not AAC"),
        "unhelpful refusal: {err}"
    );
    // A source a *segment* names, opened lazily, refuses just the same.
    let sources = [(asset(MULTI), 0), (asset(MULTI), 9)];
    assert!(
        AudioSession::open_multi_streams(&sources, &[(Some(0), 0.0, 1.0), (Some(1), 0.0, 1.0)])
            .is_err(),
        "a second source's bad stream index has to refuse too"
    );

    // Stream 0 of a file with no audio is still the silent source it always
    // was; a *named* stream of one is a promise the file cannot keep.
    let silent = asset("test_baseline.mp4");
    assert!(
        AudioSession::open_multi_streams(&[(silent.clone(), 0)], &[(Some(0), 0.0, 1.0)])
            .expect("no audio is not an error")
            .is_none()
    );
    assert!(AudioSession::open_multi_streams(&[(silent, 1)], &[(Some(0), 0.0, 1.0)]).is_err());
}
