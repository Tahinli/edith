//! Files carry more than one audio track — a remux has one per language — so
//! the engine has to *list* them all and open the one it is asked for, instead
//! of taking whichever AAC track it stumbles on first.

use std::path::PathBuf;

use engine::audio::{AudioSession, StreamInfo};

/// Three audio streams: 0 is AAC 44.1k stereo (440/880 Hz, und), 1 is AAC
/// 22.05k mono (220 Hz, fra), 2 is AC-3, which we cannot decode.
/// See `scripts/gen_fixtures.sh`.
const MULTI: &str = "test_multiaudio.mp4";

/// Two audio streams in **Matroska**, the shape a dual-audio remux has: 0 is
/// AAC 44.1k stereo 440/880 Hz English, 1 is the same shape at 220/330 Hz
/// French. See `scripts/gen_fixtures.sh`.
const MULTI_MKV: &str = "test_multiaudio.mkv";

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
            // mp4 0.14 parses no sample entry for AC-3 and keeps no fourcc, so
            // this row is described by the AC-3 reader itself: the codec by the
            // stsd fourcc read by hand, the rate and the layout by the first
            // syncframe. Stereo, because that is what the downmix hands out.
            StreamInfo {
                index: 2,
                codec: "ac-3".into(),
                channels: 2,
                sample_rate: 44100,
                lang: None,
                decodable: true,
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

/// The same listing for the container a film actually arrives in. A Matroska
/// file used to be worth exactly one row here -- whichever track the readers
/// stumbled on first -- so the second language of a dual-audio remux was
/// invisible, and the language it is in was never shown at all.
#[test]
fn probe_streams_lists_every_matroska_audio_track_with_its_language() {
    let streams = AudioSession::probe_streams(asset(MULTI_MKV)).expect("probes");
    assert_eq!(
        streams,
        vec![
            StreamInfo {
                index: 0,
                codec: "aac".into(),
                channels: 2,
                sample_rate: 44100,
                lang: Some("eng".into()),
                decodable: true,
            },
            StreamInfo {
                index: 1,
                codec: "aac".into(),
                channels: 2,
                sample_rate: 44100,
                lang: Some("fra".into()),
                decodable: true,
            },
        ]
    );
}

/// The stream index is a position in the file's `Tracks` element and the reader
/// is pointed at that entry's *track number*; a mapping that drifts plays the
/// other language and saves it into the project, so it is measured by the tones
/// each track carries.
#[test]
fn a_named_matroska_stream_decodes_that_stream_and_not_the_first() {
    // The left channel of each stream: 440 Hz on stream 0, 220 Hz on stream 1.
    for (stream, hz) in [(0usize, 440.0f64), (1, 220.0)] {
        let (meta, samples) = decode(MULTI_MKV, stream);
        assert_eq!((meta.sample_rate, meta.channels), (44100, 2));
        let left: Vec<f32> = samples.chunks(2).map(|c| c[0]).collect();
        let middle = &left[left.len() / 4..left.len() * 3 / 4];
        let expected = 2.0 * hz * middle.len() as f64 / 44100.0;
        let crossings = zero_crossings(middle) as f64;
        assert!(
            (crossings - expected).abs() < 0.05 * expected,
            "stream {stream} is {crossings} crossings, {hz} Hz would be {expected}"
        );
        let rms = (samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>()
            / samples.len() as f64)
            .sqrt();
        assert!(rms > 0.01, "stream {stream} decoded silent: RMS {rms:.6}");
    }

    // Stream 0 is what the file has always played: the wrapper that names no
    // stream and stream 0 are the same track, sample for sample.
    let (wrapped, rx) = AudioSession::open(asset(MULTI_MKV))
        .expect("opens")
        .expect("audio");
    let plain: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
    let (meta, named) = decode(MULTI_MKV, 0);
    assert_eq!(wrapped, meta);
    assert_eq!(plain, named);

    // ...and a stream the file does not have is refused, not quietly served the
    // first track.
    let err = AudioSession::open_multi_streams(&[(asset(MULTI_MKV), 2)], &[(Some(0), 0.0, 1.0)])
        .expect_err("stream 2 of 2 does not exist");
    assert!(
        err.to_string().contains("audio stream 2 of 2 streams"),
        "unhelpful refusal: {err}"
    );
}

/// The file the ask came from: a dual-audio Blu-ray rip, two AAC tracks, and
/// the second one has to play. Existence-gated -- it is not in the repo.
#[test]
fn a_real_dual_audio_remux_lists_both_tracks_and_plays_the_second() {
    let path = PathBuf::from(
        "/path/to/\
a-real-h264-dual-audio-film.mkv",
    );
    if !path.exists() {
        eprintln!("skipped: {} is not on this machine", path.display());
        return;
    }
    let streams = AudioSession::probe_streams(&path).expect("probes");
    assert_eq!(streams.len(), 2, "two audio tracks: {streams:?}");
    for (index, info) in streams.iter().enumerate() {
        assert_eq!(info.index, index);
        assert_eq!(info.codec, "aac", "{info:?}");
        assert!(info.decodable, "{info:?}");
    }
    // A second of each track, ten seconds in (past the silence a title card
    // opens on): both have to carry sound and they have to be *different*
    // sound. The failure this guards against is a stream index that resolves to
    // nothing, or to the first track again.
    let mut takes = Vec::new();
    for stream in [0, 1] {
        let (meta, rx) =
            AudioSession::open_multi_streams(&[(path.clone(), stream)], &[(Some(0), 10.0, 11.0)])
                .expect("the stream opens")
                .expect("the stream is there");
        let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
        assert!(
            samples.len() > meta.sample_rate as usize / 2,
            "half a second of stream {stream} at least, got {}",
            samples.len()
        );
        let rms = (samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>()
            / samples.len() as f64)
            .sqrt();
        assert!(rms > 1e-4, "stream {stream} came out silent: RMS {rms:.8}");
        takes.push(samples);
    }
    assert_ne!(
        takes[0], takes[1],
        "the two languages decoded to the same samples: the stream index picked one track twice"
    );
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
    // Stream 2 is AC-3, and a *named* stream is opened by its own reader: it
    // decodes to the same 44.1 kHz stereo the AAC stream 0 does, which is what
    // lets it share this timeline at all.
    let ac3 = [(asset(MULTI), 2)];
    let (meta, _rx) = AudioSession::open_multi_streams(&ac3, &[(Some(0), 0.0, 1.0)])
        .expect("AC-3 decodes now")
        .expect("the stream is there");
    assert_eq!((meta.sample_rate, meta.channels), (44100, 2));
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

/// Where the stream picker meets a file that has no picture to pick against: a
/// standalone audio file is one stream, so it lists as one and answers to 0
/// alone. A picker that offered a second row here would be offering a track
/// that does not exist.
#[test]
fn a_standalone_audio_file_is_exactly_one_stream() {
    for (name, codec) in [("test_tone.mp3", "mp3"), ("test_tone.flac", "flac")] {
        let streams = AudioSession::probe_streams(asset(name)).expect("probes");
        assert_eq!(
            streams,
            vec![StreamInfo {
                index: 0,
                codec: codec.into(),
                channels: 2,
                sample_rate: 44100,
                // No mdhd to carry an ISO-639 tag, and one track needs no
                // telling apart.
                lang: None,
                decodable: true,
            }],
            "{name}"
        );
        // Stream 0 opens; anything above it is refused rather than quietly
        // served the only track there is.
        assert!(
            AudioSession::open_multi_streams(&[(asset(name), 0)], &[(Some(0), 0.0, 1.0)])
                .expect("stream 0 opens")
                .is_some(),
            "{name}"
        );
        let err = AudioSession::open_multi_streams(&[(asset(name), 1)], &[(Some(0), 0.0, 1.0)])
            .expect_err("there is no stream 1");
        assert!(
            err.to_string().contains("audio stream 1 of 1 stream"),
            "unhelpful refusal for {name}: {err}"
        );
    }
    // The ALAC .m4a is the awkward one: it parses as an mp4 whose only audio
    // track is not AAC, so the mp4 walk would call it "unknown" and undecodable
    // where symphonia decodes it perfectly well.
    let alac = AudioSession::probe_streams(asset("test_tone.m4a")).expect("probes");
    assert_eq!(alac.len(), 1);
    assert!(alac[0].decodable, "ALAC decodes: {alac:?}");
    assert_eq!((alac[0].sample_rate, alac[0].channels), (44100, 2));
}
