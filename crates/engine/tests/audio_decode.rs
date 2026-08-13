use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::audio::AudioSession;
use engine::scratch::Scratch;

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

/// Opus, which used to be the codec the refusal string was *for*: a standalone
/// `.opus`, the `.webm` sound off the web, and the 5.1 in an `.mka` that a film
/// soundtrack is -- four Opus streams with a channel mapping table, decoded by
/// `ruopus` and folded to stereo by the same fold the AC-3 and AAC paths use.
///
/// The 5.1 fixture is the channel-order check as well: its tone is in FL and BR
/// and silent in between, so both output channels carry sound only if the
/// decoder's Vorbis order (FL, FC, FR, BL, BR, LFE) was put into the film order
/// the fold reads (FL, FR, FC, LFE, BL, BR). Skipping that permutation folds BR
/// as if it were BL and the right channel comes out silent.
#[test]
fn opus_decodes_from_every_container_and_51_comes_down_in_order() {
    for (name, secs, channels) in [("test_tone.opus", 3.0, 2), ("test_vp9.webm", 2.0, 1)] {
        let path = asset(name);
        let (meta, rx) = AudioSession::open(&path)
            .expect("open")
            .expect("Opus decodes now");
        assert_eq!((meta.sample_rate, meta.channels), (48000, channels), "{name}");
        let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
        let decoded = (samples.len() / channels as usize) as f64 / 48000.0;
        assert!(
            (secs - decoded).abs() < 0.1,
            "{name} is {secs}s, decoded {decoded:.3}s"
        );
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
        // ...and the same file as a session, where the picture allows it: the
        // webm's is VP9, which is the VA-API plugin's and not this test's.
        if !name.ends_with(".webm") {
            let session = engine::PlaybackSession::open(&path).expect("open for playback");
            assert_eq!(
                session.audio_disabled_reason(),
                None,
                "{name}: a track that decodes owes no excuse"
            );
        }
    }

    let path = asset("test_opus_51.mka");
    let (meta, rx) = AudioSession::open(&path)
        .expect("open")
        .expect("5.1 Opus decodes now");
    assert_eq!((meta.sample_rate, meta.channels), (48000, 2));
    let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
    let rms = |channel: usize| {
        let side: Vec<f64> = samples[channel..]
            .iter()
            .step_by(2)
            .map(|s| f64::from(*s) * f64::from(*s))
            .collect();
        (side.iter().sum::<f64>() / side.len() as f64).sqrt()
    };
    let (left, right) = (rms(0), rms(1));
    assert!(left > 0.02, "the 5.1 fold came out silent on the left: {left}");
    assert!(
        right > 0.3 * left,
        "BR was folded as if it were BL: left {left:.4}, right {right:.4}"
    );
    // ...and it is the *tone* on each side, not noise at the right level. This is
    // the assert that bites: a multistream decoder that mis-slices the packet
    // (all but the last stream of an Opus 5.1 packet are self-delimited) hands
    // back full-scale hash whose RMS passes every level check above -- measured
    // -13 dBFS of it on his film, where the film is silent.
    let secs = (samples.len() / 2) as f64 / 48000.0;
    for (name, channel, hz) in [("left", 0, 440.0), ("right", 1, 880.0)] {
        let side: Vec<f32> = samples[channel..].iter().step_by(2).copied().collect();
        let want = 2.0 * hz * secs;
        let got = zero_crossings(&side) as f64;
        assert!(
            (got - want).abs() <= 0.05 * want,
            "{name}: {got} zero crossings, want {want} +/-5% -- a decode this far off is noise"
        );
    }
}

/// The amplitude of an `hz` sine inside `samples`, by Goertzel. The window is a
/// whole second at 48 kHz, so every frequency this asks about is an exact bin
/// and there is no leakage to correct for; a real sine of amplitude A puts
/// `A * N / 2` in its bin.
fn amplitude(samples: &[f32], hz: f64) -> f64 {
    let n = samples.len() as f64;
    let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0, 0.0);
    for &sample in samples {
        let s0 = f64::from(sample) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    2.0 * (s1 * s1 + s2 * s2 - coeff * s1 * s2).sqrt() / n
}

/// **7.1 Opus**, the widest layout the fold has a table for and the one his
/// largest film carries: five Opus streams, three coupled, arriving on the
/// timeline as an ordinary stereo source instead of the import refusal it used
/// to be ("unsupported channel layout: 8 channels (max stereo)").
///
/// The fixture puts one frequency in each of four channels and silence in the
/// rest (`gen_fixtures.sh`), so the whole downmix is one measurement: 440 in FL
/// is left only, 880 in FC is both sides at -3 dB, 1320 in the LFE must be gone
/// entirely, and 1760 in SR is right only. Every coefficient and the whole
/// Vorbis-to-film permutation at this width fail visibly here.
#[test]
fn seven_one_opus_folds_to_the_stereo_the_timeline_carries() {
    let path = asset("test_opus_71.mka");
    let streams = AudioSession::probe_streams(&path).expect("streams");
    assert_eq!(streams.len(), 1);
    assert!(streams[0].decodable, "a 7.1 row is not greyed out any more");
    assert_eq!(streams[0].channels, 2, "what reaches the timeline is a pair");

    let (meta, rx) = AudioSession::open(&path)
        .expect("open")
        .expect("7.1 Opus decodes now");
    assert_eq!((meta.sample_rate, meta.channels), (48000, 2));
    let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
    assert!(
        samples.iter().all(|s| s.abs() <= 1.0),
        "the fold must not leave the device's range"
    );
    // One whole second out of the middle, past any codec fade-in.
    let second = |channel: usize| -> Vec<f32> {
        samples[channel..]
            .iter()
            .step_by(2)
            .skip(48_000)
            .take(48_000)
            .copied()
            .collect()
    };
    // 1 + 3 * (-3 dB): FL/FR keep their side, FC and both surround pairs are the
    // three that join it. Nothing may come out above full scale, which is what
    // dividing by the coefficient sum buys.
    let norm = 1.0 + 3.0 * f64::from(std::f32::consts::FRAC_1_SQRT_2);
    let c = f64::from(std::f32::consts::FRAC_1_SQRT_2);
    let (left, right) = (second(0), second(1));
    // FL is the one channel that keeps its side at unity, so what it comes out
    // as names the tone the fixture was written with -- `sine` peaks at 0.125 in
    // ffmpeg, and reading that off the fold rather than hard-coding it keeps the
    // coefficients below the claim instead of the generator's level. A fold that
    // lost FL, or scaled the pair, leaves this band.
    let source = amplitude(&left, 440.0) * norm;
    assert!(
        (0.10..0.15).contains(&source),
        "FL came out as a {source:.4} tone; the fixture's is 0.125"
    );
    for (side, cut, want) in [
        ("left", &left, [(440.0, 1.0), (880.0, c), (1320.0, 0.0), (1760.0, 0.0)]),
        ("right", &right, [(440.0, 0.0), (880.0, c), (1320.0, 0.0), (1760.0, c)]),
    ] {
        for (hz, coeff) in want {
            let got = amplitude(cut, hz);
            let expect = source * coeff / norm;
            match coeff {
                // The LFE, and the channels this side has none of: what is left
                // is codec noise and bleed, an order below the quietest tone.
                0.0 => assert!(
                    got < 0.1 * source / norm,
                    "{side}: {hz} Hz came through at {got:.4}, and nothing there should"
                ),
                _ => assert!(
                    (got - expect).abs() < 0.15 * expect,
                    "{side}: {hz} Hz at {got:.4}, want {expect:.4} +/-15%"
                ),
            }
        }
    }
}

/// His own 7.1 remux, which is the file this whole change exists for: 12.9 GB of
/// 2160p AV1 with an 8-channel Opus track that used to come back "IMPORT FAILED:
/// unsupported channel layout: 8 channels (max stereo)" and take the picture
/// with it.
///
/// Skipped, not failed, on a machine that does not have the film: a fixture
/// cannot stand in for the claim (the refusal was measured on *this* file) and a
/// test that cannot make it must say nothing rather than something false.
#[test]
fn his_seven_one_remux_imports_and_plays() {
    let Some(film) = engine::real_library::film("av1_opus_71") else {
        return;
    };
    // The import gate itself: this is the call whose `Err` the app printed.
    let probe = AudioSession::probe(&film, 0)
        .expect("the 7.1 track no longer refuses the import")
        .expect("the film has audio");
    assert_eq!(
        (probe.sample_rate, probe.channels),
        (48_000, 2),
        "7.1 reaches the timeline as a pair"
    );
    // ...and every one of its three tracks is offered, none greyed out.
    let streams = AudioSession::probe_streams(&film).expect("streams");
    assert_eq!(streams.len(), 3, "7.1 plus two 5.1 commentary tracks");
    assert!(
        streams.iter().all(|s| s.decodable && s.channels == 2),
        "{streams:?}"
    );

    // Ten seconds out of the middle of the film, through the very worker
    // playback and a WAV export both feed from -- with sound on *both* sides,
    // which a fold that dropped half the layout would not have.
    let (meta, rx) = AudioSession::open_segments(&film, &[(600.0, 610.0)])
        .expect("open")
        .expect("the film has audio");
    assert_eq!((meta.sample_rate, meta.channels), (48_000, 2));
    let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
    let secs = (samples.len() / 2) as f64 / 48_000.0;
    assert!((9.9..10.1).contains(&secs), "{secs:.3} s decoded, want 10");
    assert!(
        samples.iter().all(|s| s.abs() <= 1.0),
        "the fold must not leave the device's range"
    );
    for (side, channel) in [("left", 0), ("right", 1)] {
        let one: Vec<f64> = samples[channel..].iter().step_by(2).map(|s| f64::from(*s)).collect();
        let rms = (one.iter().map(|s| s * s).sum::<f64>() / one.len() as f64).sqrt();
        assert!(rms > 0.001, "{side} came out silent: RMS {rms:.6}");
        eprintln!("{side}: RMS {rms:.6} over {secs:.2}s from 600s");
    }

    // ...and the whole way out: the file on a timeline, trimmed to ten seconds,
    // written as a WAV by the export path and read back through this engine's
    // own reader. A session open is what the app's Import does, so a picture
    // this size failing to open would fail here rather than in his hands.
    let mut session = engine::PlaybackSession::open(&film).expect("the film opens as a timeline");
    assert_eq!(
        session.audio_disabled_reason(),
        None,
        "a track that decodes owes no excuse"
    );
    let frames = (10.0 * session.meta().frame_rate).round() as u32;
    for lane in session.lanes() {
        session.trim_clip(lane, 0, engine::project::Edge::End, frames);
    }
    let out = Scratch::file("ve_seven_one", "wav");
    let handle = session.export_to_with(
        &out,
        &engine::export::ExportSettings {
            format: engine::export::Format::Wav,
            ..Default::default()
        },
    );
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(300), "export hung");
        std::thread::sleep(Duration::from_millis(50));
    }
    handle.result().expect("outcome").expect("the WAV is written");
    let (wav, rx) = AudioSession::open(&out)
        .expect("the WAV reopens")
        .expect("it has sound");
    let written: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
    let channels = usize::from(wav.channels);
    assert_eq!(channels, 2, "a stereo WAV out of a 7.1 source");
    for (side, channel) in [("left", 0), ("right", 1)] {
        let one: Vec<f64> = written[channel..]
            .iter()
            .step_by(channels)
            .map(|s| f64::from(*s))
            .collect();
        let rms = (one.iter().map(|s| s * s).sum::<f64>() / one.len() as f64).sqrt();
        assert!(rms > 0.001, "the exported {side} is silent: RMS {rms:.6}");
        eprintln!(
            "exported {side}: RMS {rms:.6} over {:.2}s at {} Hz",
            one.len() as f64 / f64::from(wav.sample_rate),
            wav.sample_rate
        );
    }
}

/// The same two ends of the AC-3 path, in **Matroska**: a stereo AC-3 track and
/// a 5.1 E-AC-3 one -- the codec a remux carries and the one the "(AAC only)"
/// refusal used to fire on -- both come out of the blocks, both arrive as the
/// stereo the §7.8 downmix hands the timeline, and neither owes the session an
/// excuse any more.
#[test]
fn matroska_ac3_and_eac3_decode_out_of_the_blocks() {
    for (name, codec) in [("test_ac3.mkv", "ac-3"), ("test_eac3.mkv", "e-ac-3")] {
        let path = asset(name);
        let (meta, rx) = AudioSession::open(&path)
            .expect("open")
            .expect("Matroska AC-3 decodes now");
        assert_eq!((meta.sample_rate, meta.channels), (48000, 2), "{name}");
        let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
        let secs = (samples.len() / 2) as f64 / 48000.0;
        assert!(
            (1.9..2.1).contains(&secs),
            "{name} is two seconds, decoded {secs:.3}s"
        );
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
        // The user-facing half: the session comes up with sound and no notice,
        // and the file's one audio stream is offered by the name it is in.
        let session = engine::PlaybackSession::open(&path).expect("open for playback");
        assert_eq!(
            session.audio_disabled_reason(),
            None,
            "{name}: a track that decodes owes no excuse"
        );
        let streams = AudioSession::probe_streams(&path).expect("streams");
        assert_eq!(streams.len(), 1, "{name}: one audio track, one row");
        assert_eq!(streams[0].codec, codec, "{name}");
        assert!(streams[0].decodable, "{name}");
        assert_eq!(
            AudioSession::unsupported(&path).expect("unsupported"),
            None,
            "{name}: nothing to excuse when it plays"
        );
    }
    // The invariant, stated as an assert: the AAC track of a Matroska file is
    // still symphonia's, listed and decoded exactly as it was.
    let streams = AudioSession::probe_streams(asset("test_av1.mkv")).expect("streams");
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].codec, "aac", "AAC in an mkv is untouched by this");
    assert!(
        AudioSession::open(asset("test_av1.mkv"))
            .expect("open")
            .is_some(),
        "AAC in an mkv still decodes"
    );
}

/// A seek into a Matroska AC-3 track: Matroska indexes no samples, so the block
/// a decode starts on is found by the blocks' own timestamps -- and getting that
/// wrong is a tail that is the wrong length or lands in the wrong place. It is
/// the same check `audio_seek` makes of the mp4 path, on the container that has
/// no sample table.
#[test]
fn a_seek_into_a_matroska_ac3_track_is_the_tail_of_a_full_run() {
    let path = asset("test_eac3.mkv");
    let full: Vec<f32> = AudioSession::open(&path)
        .expect("open")
        .expect("audio")
        .1
        .into_iter()
        .flat_map(|c| c.samples)
        .collect();
    let start = 48_000u64; // one second in, in frames per channel
    let mut next = start;
    let mut tail = Vec::new();
    for chunk in AudioSession::open_at(&path, 1.0).expect("open").expect("audio").1 {
        assert_eq!(chunk.start_sample, next, "a seeked run numbers from the seek");
        next += (chunk.samples.len() / 2) as u64;
        tail.extend(chunk.samples);
    }
    let want = &full[start as usize * 2..];
    assert_eq!(tail.len(), want.len(), "the seeked run is a different length");
    let diff = tail
        .iter()
        .zip(want)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    // One syncframe of pre-roll is what the 256-sample overlap-add needs, so
    // this is exact in practice; the tolerance is for a decoder that carries
    // more state than the frame before, not for a wrong block.
    assert!(diff < 1e-3, "max abs diff {diff} after the seek");
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

/// The two things a Matroska file with no *readable* sound can be, told apart:
/// a file with no audio track is the silent source it says it is, and a file
/// whose audio will not open at all is an error. They used to be the same
/// answer -- silence -- so a broken track played and exported as a hole with
/// nothing but an `eprintln` to say why.
#[test]
fn a_matroska_with_no_track_is_silent_and_a_broken_one_is_an_error() {
    // No audio track at all: a picture and two subtitle tracks. Silent, and the
    // stream picker offers nothing rather than refusing the file.
    let subs = asset("test_subs.mkv");
    assert!(
        AudioSession::probe(&subs, 0)
            .expect("no audio is not an error")
            .is_none(),
        "an mkv with no audio track is a silent source"
    );
    assert!(AudioSession::probe_streams(&subs).expect("list").is_empty());
    assert!(
        AudioSession::open(&subs).expect("open").is_none(),
        "no sound"
    );

    // ...and a file that will not parse at all, under the same extension: an
    // `Err` a front-end can show, not a silent import.
    let broken = Scratch::file("ve_broken", "mkv");
    std::fs::write(&broken, b"\x1a\x45\xdf\xa3not a matroska file at all").expect("write");
    let err = AudioSession::probe(&broken, 0)
        .expect_err("a broken file must not pass for a silent one")
        .to_string();
    assert!(!err.is_empty(), "the refusal says something");
    assert!(
        AudioSession::probe_streams(&broken).is_err(),
        "and so does the listing"
    );
    std::fs::remove_file(&broken).unwrap();
}
