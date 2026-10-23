//! Mixed audio lanes whose sources disagree on width and rate: a mono 22.05 kHz
//! voice on one lane beside a stereo 48 kHz bed on another. The voice has to be
//! heard for its whole length in the mix and in a WebM (Opus, which codes at
//! 48 kHz) export, not cut off at half of it.

use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use engine::audio::{AudioChunk, AudioSession};
use engine::export::{ExportSettings, Format};
use engine::project::{LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::{Clip, ExportHandle, Project};

/// A 16-bit PCM WAV written to `path`: a sine at `freq` Hz in every channel at
/// `amp` of full scale.
fn write_wav(path: impl AsRef<Path>, rate: u32, channels: u16, secs: f64, freq: f64, amp: f64) {
    let frames = (secs * f64::from(rate)) as u32;
    let data_len = frames * u32::from(channels) * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for n in 0..frames {
        let t = f64::from(n) / f64::from(rate);
        let v = (amp * (2.0 * std::f64::consts::PI * freq * t).sin() * 32767.0) as i16;
        for _ in 0..channels {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    std::fs::write(path, out).expect("write wav");
}

/// Drains a stream, asserting its chunks are contiguous, and returns the
/// interleaved samples.
fn drain(rx: Receiver<AudioChunk>, channels: usize) -> Vec<f32> {
    let mut samples = Vec::new();
    let mut next = None;
    for chunk in rx {
        if let Some(next) = next {
            assert_eq!(chunk.start_sample, next, "gap or overlap in the stream");
        }
        next = Some(chunk.start_sample + (chunk.samples.len() / channels) as u64);
        samples.extend(chunk.samples);
    }
    samples
}

/// Root mean square of the first channel over each `window` seconds.
fn rms_per_window(samples: &[f32], channels: usize, rate: u32, window: f64) -> Vec<f64> {
    let per = (window * f64::from(rate)) as usize;
    samples
        .chunks(per * channels)
        .map(|w| {
            let sum: f64 = w
                .iter()
                .step_by(channels)
                .map(|&s| f64::from(s).powi(2))
                .sum();
            (sum / (w.len() / channels) as f64).sqrt()
        })
        .collect()
}

/// Every half second of a 4 s voice must carry energy, the tail included.
fn assert_voice_throughout(samples: &[f32], channels: usize, rate: u32, what: &str) {
    let levels = rms_per_window(samples, channels, rate, 0.5);
    assert!(
        levels.len() >= 8,
        "{what}: only {} half-second windows, the voice is 4 s",
        levels.len()
    );
    for (i, level) in levels.iter().enumerate().take(8) {
        assert!(
            *level > 0.05,
            "{what}: window {i} (at {:.1} s) is silent, rms {level:.5}; all {levels:?}",
            i as f64 * 0.5
        );
    }
}

fn voice_and_bed() -> (Scratch, Scratch) {
    let voice = Scratch::file("mix_voice_22k_mono", "wav");
    let bed = Scratch::file("mix_bed_48k_stereo", "wav");
    write_wav(&voice, 22_050, 1, 4.0, 220.0, 0.5);
    write_wav(&bed, 48_000, 2, 1.0, 660.0, 0.2);
    (voice, bed)
}

#[test]
fn mono_22k_voice_mixed_with_a_stereo_48k_bed_is_heard_its_whole_length() {
    let (voice, bed) = voice_and_bed();
    let sources = vec![(voice.to_path_buf(), 0), (bed.to_path_buf(), 0)];
    let lanes = vec![vec![(Some(0), 0.0, 4.0)], vec![(Some(1), 0.0, 1.0)]];
    let (meta, rx) = AudioSession::open_mixed_streams(&sources, &lanes)
        .expect("open")
        .expect("audio");
    let channels = usize::from(meta.channels);
    let samples = drain(rx, channels);

    let secs = samples.len() as f64 / channels as f64 / f64::from(meta.sample_rate);
    assert!(secs > 3.9, "mix is {secs:.2} s, the voice is 4 s");
    assert_voice_throughout(&samples, channels, meta.sample_rate, "mixed");
}

#[test]
fn mono_22k_voice_alone_on_its_lane_is_heard_its_whole_length() {
    // The control for the test above: the same voice with no bed beside it.
    let (voice, _) = voice_and_bed();
    let sources = vec![(voice.to_path_buf(), 0)];
    let lanes = vec![vec![(Some(0), 0.0, 4.0)]];
    let (meta, rx) = AudioSession::open_mixed_streams(&sources, &lanes)
        .expect("open")
        .expect("audio");
    let channels = usize::from(meta.channels);
    let samples = drain(rx, channels);
    assert_voice_throughout(&samples, channels, meta.sample_rate, "solo");
}

#[test]
fn a_mono_lane_beside_a_stereo_lane_is_the_mixs_width_not_its_own() {
    // Mixed at 48 kHz, as WebM mixes: the voice is resampled up and must still
    // be the mix's two channels wide, which is what the width rule guarantees.
    let (voice, bed) = voice_and_bed();
    let sources = vec![(voice.to_path_buf(), 0), (bed.to_path_buf(), 0)];
    let lanes = vec![vec![(Some(0), 0.0, 4.0)], vec![(Some(1), 0.0, 1.0)]];
    let (meta, rx) = AudioSession::open_mixed_streams_master_at(
        &sources,
        &lanes,
        &[],
        &[],
        &[],
        engine::limiter::Limiter::default(),
        Some(48_000),
    )
    .expect("open")
    .expect("audio");
    assert_eq!(meta.channels, 2, "the mix takes the widest lane");
    let channels = usize::from(meta.channels);
    let samples = drain(rx, channels);
    assert_voice_throughout(&samples, channels, meta.sample_rate, "mixed at 48k");
}

fn asset(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// One clip of `source` at timeline frame `start`, `frames` long at the
/// project's rate, playing its source from the start.
fn clip(start: u32, frames: u32, source: usize) -> Clip {
    Clip {
        speed_keys: Vec::new(),
        reversed: false,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        transition_kind: Default::default(),
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        start,
        in_frame: 0,
        out_frame: frames,
        source,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
        transform: None,
    }
}

#[test]
fn a_webm_of_a_mono_voice_and_a_stereo_bed_keeps_the_voice_to_its_end() {
    engine::envcfg::set("VE_SW", "1");
    engine::envcfg::set("VE_SW_ENC", "1");
    let (voice, bed) = voice_and_bed();
    let project = Project::from_parts(
        vec![
            Source::new(asset("test_av.mp4"), 0),
            Source::new(&voice, 0),
            Source::new(&bed, 0),
        ],
        vec![
            (LaneKind::Video, vec![clip(0, 120, 0)]),
            (LaneKind::Audio, vec![clip(0, 120, 1)]),
            (LaneKind::Audio, vec![clip(0, 30, 2)]),
        ],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("a picture with a voice and a bed under it");
    let meta = engine::demux::Demuxer::open(&asset("test_av.mp4"))
        .expect("open test_av")
        .0;
    let out = Scratch::file("mix_voice_bed", "webm");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Webm,
            ..Default::default()
        },
        None,
    );
    wait(&handle, Duration::from_secs(120)).expect("webm export");

    let (audio, chunks) = AudioSession::open(&*out)
        .expect("reopen the webm")
        .expect("the webm has audio");
    let channels = usize::from(audio.channels);
    let mut samples = Vec::new();
    for chunk in chunks {
        samples.extend(chunk.samples);
    }
    let secs = samples.len() as f64 / channels as f64 / f64::from(audio.sample_rate);
    assert!(
        secs > 3.8,
        "the webm sound is {secs:.2} s, the voice is 4 s"
    );
    assert_voice_throughout(&samples, channels, audio.sample_rate, "webm");
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < limit,
            "export did not finish in {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.result().expect("a finished export has an outcome")
}

/// Opens the mix of `sources` on `lanes` the way playback and export do, drains
/// it, and returns the rate it runs at, its channel count, and its length in
/// seconds as the stream delivers it.
fn open_mix(sources: &[(std::path::PathBuf, usize)], lanes: &[Vec<(Option<usize>, f64, f64)>]) -> (u32, u16, f64) {
    let (meta, rx) = AudioSession::open_mixed_streams(sources, lanes)
        .expect("open")
        .expect("audio");
    let channels = usize::from(meta.channels);
    let samples = drain(rx, channels);
    let secs = samples.len() as f64 / channels as f64 / f64::from(meta.sample_rate);
    (meta.sample_rate, meta.channels, secs)
}

#[test]
fn a_22k_voiceover_beside_a_48k_bed_mixes_at_48k() {
    // The bug: the first source's rate (the voiceover's) was the mix's, and a
    // podcast exported at 22.05 kHz beside its 48 kHz music bed.
    let (voice, bed) = voice_and_bed();
    let sources = vec![(voice.to_path_buf(), 0), (bed.to_path_buf(), 0)];
    let lanes = vec![vec![(Some(0), 0.0, 4.0)], vec![(Some(1), 0.0, 1.0)]];
    let (rate, channels, secs) = open_mix(&sources, &lanes);
    assert_eq!(rate, 48_000, "the highest source rate wins");
    assert_eq!(channels, 2, "and the mix is as wide as its widest source");
    assert!(secs > 3.9, "the 4 s voice is heard whole at 48k, got {secs:.2} s");
}

#[test]
fn a_44k_source_beside_a_48k_source_mixes_at_48k() {
    let low = Scratch::file("mix_rate_44k_src", "wav");
    write_wav(&low, 44_100, 2, 4.0, 220.0, 0.3);
    let high = Scratch::file("mix_rate_48k_src", "wav");
    write_wav(&high, 48_000, 2, 4.0, 660.0, 0.3);
    let sources = vec![(low.to_path_buf(), 0), (high.to_path_buf(), 0)];
    let lanes = vec![vec![(Some(0), 0.0, 4.0)], vec![(Some(1), 0.0, 4.0)]];
    let (rate, _, secs) = open_mix(&sources, &lanes);
    assert_eq!(rate, 48_000);
    assert!(secs > 3.9, "the 44.1 kHz source is heard whole, got {secs:.2} s");
}

#[test]
fn only_a_22k_source_mixes_at_48k() {
    // The documented choice: a mix whose sources are all below 44.1 kHz runs at
    // 48 kHz, which AAC accepts and Opus needs. The length is unchanged.
    let (voice, _) = voice_and_bed();
    let sources = vec![(voice.to_path_buf(), 0)];
    let lanes = vec![vec![(Some(0), 0.0, 4.0)]];
    let (rate, channels, secs) = open_mix(&sources, &lanes);
    assert_eq!(rate, 48_000);
    assert_eq!(channels, 1);
    assert!((secs - 4.0).abs() < 0.05, "a 4 s voice is 4 s at 48k, got {secs:.3} s");
}

#[test]
fn a_44k_only_mix_stays_at_44k() {
    let low = Scratch::file("mix_rate_44k_only", "wav");
    write_wav(&low, 44_100, 2, 4.0, 220.0, 0.3);
    let sources = vec![(low.to_path_buf(), 0)];
    let lanes = vec![vec![(Some(0), 0.0, 4.0)]];
    let (rate, _, secs) = open_mix(&sources, &lanes);
    assert_eq!(rate, 44_100, "a 44.1 kHz timeline is not resampled");
    assert!(secs > 3.9, "the 4 s source is heard whole, got {secs:.2} s");
}
