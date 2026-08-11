use std::path::PathBuf;
use std::time::Instant;

use engine::audio::AudioSession;

const RATE: u32 = 44100;
const SECONDS: u32 = 5;
/// One AAC-LC packet; the container's tail padding is not trimmed, so the
/// decoded length may run up to a frame past the nominal duration.
const FRAME: i64 = 1024;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Sign changes ignoring exact zeros, so the 1 Hz volume pulse touching zero
/// does not register as a crossing.
fn zero_crossings(samples: &[f32]) -> usize {
    let mut crossings = 0;
    let mut last = 0f32;
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
fn decodes_av_mp4_audio() {
    let start = Instant::now();
    let (meta, rx) = AudioSession::open(asset("test_av.mp4"))
        .expect("open")
        .expect("test_av.mp4 has an audio track");
    assert_eq!((meta.sample_rate, meta.channels), (RATE, 2));

    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut next_sample = 0u64;
    let mut chunks = 0usize;
    for chunk in rx {
        // d. no gaps, no overlaps.
        assert_eq!(chunk.start_sample, next_sample, "chunk {chunks} start");
        assert!(!chunk.samples.is_empty(), "empty chunk {chunks}");
        assert_eq!(
            chunk.samples.len() % 2,
            0,
            "partial frame in chunk {chunks}"
        );
        next_sample += (chunk.samples.len() / 2) as u64;
        left.extend(chunk.samples.iter().step_by(2));
        right.extend(chunk.samples[1..].iter().step_by(2));
        chunks += 1;
    }
    let elapsed = start.elapsed();
    let frames = left.len();
    assert!(frames > 0, "no audio decoded");

    // b. ~5 s of audio, priming trimmed, at most a frame of tail padding over.
    let want = (RATE * SECONDS) as i64;
    let slack = frames as i64 - want;
    assert!(
        (0..=FRAME).contains(&slack),
        "{frames} frames per channel, want {want} (+0..={FRAME})"
    );
    assert_eq!(right.len(), frames, "channel lengths differ");

    // c. channel identity: L is 440 Hz, R is 880 Hz. A swap or a bad interleave
    // shows up as the wrong crossing count.
    let secs = frames as f64 / RATE as f64;
    for (name, samples, hz) in [("left", &left, 440.0), ("right", &right, 880.0)] {
        let want = 2.0 * hz * secs;
        let got = zero_crossings(samples) as f64;
        assert!(
            (got - want).abs() <= 0.05 * want,
            "{name}: {got} zero crossings, want {want} +/-5%"
        );
    }

    // Priming trimmed: the sine starts at a rising zero crossing, and audio is
    // present immediately rather than after a silent or garbage run-in.
    assert!(
        left[0].abs() < 0.05,
        "first sample {} not near zero",
        left[0]
    );
    assert!(left[1] > 0.0, "first sine quarter is not rising");
    let head_peak = left[..FRAME as usize]
        .iter()
        .fold(0f32, |m, s| m.max(s.abs()));
    assert!(head_peak > 0.01, "first frame is silent: peak {head_peak}");

    eprintln!(
        "{frames} frames/ch in {chunks} chunks, {elapsed:?} ({:?}/packet)",
        elapsed / chunks as u32
    );
}

#[test]
fn video_only_file_has_no_audio() {
    assert!(
        AudioSession::open(asset("test_baseline.mp4"))
            .expect("open")
            .is_none()
    );
    assert!(
        AudioSession::unsupported(asset("test_baseline.mp4"))
            .expect("probe")
            .is_none(),
        "a file with no audio track is not a complaint"
    );
}

/// Both ends of the AC-3 path decode: the 44.1 kHz **mono** fixture stays the
/// mono source it is, and the 48 kHz **5.1** one — the shape a BluRay remux has
/// — arrives as stereo, downmixed by the decoder itself (A/52 §7.8). Both play
/// their two seconds, at a level that is neither silence nor clipping, and
/// neither owes the session an excuse.
#[test]
fn an_ac3_track_decodes_and_51_comes_down_to_stereo() {
    for (name, rate, channels) in [("test_ac3.mp4", 44100, 1), ("test_ac3_51.mp4", 48000, 2)] {
        let path = asset(name);
        let (meta, rx) = AudioSession::open(&path)
            .expect("open")
            .expect("AC-3 decodes now");
        assert_eq!((meta.sample_rate, meta.channels), (rate, channels), "{name}");
        let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
        let secs = (samples.len() / channels as usize) as f64 / f64::from(rate);
        assert!(
            (1.9..2.1).contains(&secs),
            "{name} is two seconds, decoded {secs:.3}s"
        );
        // The 440 Hz sine survives at a sane level. The vetting caveat was an
        // LFE passthrough running hot, which the built-in downmix is what
        // avoids; this is that level check, and it is also what catches a
        // downmix that comes out silent.
        let rms = (samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>()
            / samples.len() as f64)
            .sqrt();
        assert!(
            (0.005..0.75).contains(&rms),
            "{name}: RMS {rms:.6} is not a sine at a sane level"
        );
        assert!(
            samples.iter().all(|s| s.abs() <= 1.0),
            "{name}: the decode must not leave the device's range"
        );
        let session = engine::PlaybackSession::open(&path).expect("open for playback");
        assert_eq!(
            session.audio_disabled_reason(),
            None,
            "{name}: a track that decodes owes no excuse"
        );
    }
}

/// An AC-3 syncframe is not something an `mp4a` sample table holds, so the
/// *copy* path names it and hands the export on to the re-encode (which decodes
/// this very source: the downmix above is what it encodes). Named either way --
/// never a silent export.
#[test]
fn an_ac3_source_refuses_an_mp4_audio_copy() {
    let Err(err) = AudioSession::copy_segments(asset("test_ac3.mp4"), &[(0.0, 1.0)]) else {
        panic!("AC-3 cannot become an mp4a track");
    };
    let err = err.to_string();
    assert!(
        err.contains("needs AAC in an mp4") && err.contains("AC-3"),
        "unhelpful refusal: {err}"
    );
}
