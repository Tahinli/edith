//! The audio of a ramped clip follows the ramp's shape, not its mean rate: a click
//! track with a click every 0.25 s, ramped 1x to 2x over four seconds, plays each
//! click where the ramp's mapping puts it, and the stream is exactly the footprint.
//!
//! Everything goes through the front door, as `speed.rs` does: the three lists
//! `PlaybackSession::seek` builds, handed to the opener it hands them to.

use engine::project::{Lane, Speed, SpeedKey, ramp_timeline_at};
use engine::scratch::Scratch;
use engine::{AudioSession, Project};

const RATE: u32 = 44_100;
const FPS: f64 = 30.0;
/// The source is six seconds: a 1x-to-2x ramp over 120 frames reads 180 of them.
const SOURCE_SECS: u32 = 6;

/// A mono 16-bit WAV with a click every `every` seconds from `every` on: two
/// milliseconds of a Hann-windowed 2 kHz sine.
fn write_click_wav(path: &std::path::Path, secs: u32, every: f64) {
    let frames = secs * RATE;
    let mut pcm = vec![0f64; frames as usize];
    let len = (0.002 * f64::from(RATE)) as usize;
    let mut k = 1;
    while f64::from(k) * every < f64::from(secs) - 0.01 {
        let at = (f64::from(k) * every * f64::from(RATE)) as usize;
        for i in 0..len {
            let w = (std::f64::consts::PI * i as f64 / len as f64).sin().powi(2);
            let t = i as f64 / f64::from(RATE);
            pcm[at + i] = 0.8 * w * (2.0 * std::f64::consts::PI * 2000.0 * t).sin();
        }
        k += 1;
    }
    let data_len = frames * 2;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for v in pcm {
        out.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, out).expect("write wav");
}

/// What the device would be fed: interleaved samples and the channel count.
fn play(project: &Project) -> (Vec<f32>, usize) {
    let sources = project.audio_sources();
    let segs = project.audio_segments_from(0, FPS);
    let eqs = project.audio_eqs_from(0, FPS);
    let speeds = project.audio_speeds_from(0, FPS);
    let (meta, chunks) = AudioSession::open_mixed_streams_speed(&sources, &segs, &eqs, &speeds)
        .expect("open the timeline's audio")
        .expect("the timeline has audio");
    assert_eq!(meta.sample_rate, RATE);
    let mut samples = Vec::new();
    for chunk in chunks {
        samples.extend(chunk.samples);
    }
    (samples, usize::from(meta.channels))
}

/// The energy-weighted centre of the click near `expected` seconds in `mono`, within
/// `radius` seconds either side.
fn click_centre(mono: &[f32], expected: f64, radius: f64) -> Option<f64> {
    let lo = ((expected - radius) * f64::from(RATE)).max(0.0) as usize;
    let hi = (((expected + radius) * f64::from(RATE)) as usize).min(mono.len());
    let (mut sum, mut moment) = (0f64, 0f64);
    for (i, &s) in mono.iter().enumerate().take(hi).skip(lo) {
        let e = f64::from(s).powi(2);
        sum += e;
        moment += e * i as f64;
    }
    (sum > 1e-3).then(|| moment / sum / f64::from(RATE))
}

#[test]
fn a_ramps_audio_follows_its_shape_clicks_land_where_the_mapping_says_and_the_length_is_exact() {
    let wav = Scratch::file("ve_ramp_clicks", "wav");
    write_click_wav(&wav, SOURCE_SECS, 0.25);
    let mut project = Project::single(&wav, SOURCE_SECS * FPS as u32);
    let keys = [
        SpeedKey { at: 0, speed: Speed::NORMAL },
        SpeedKey { at: 120, speed: Speed::from_permille(2000) },
    ];
    project.set_speed_ramp(Lane::V1, 0, &keys).expect("the ramp sets");
    assert_eq!(project.timeline_frames(), 120, "4 s of timeline");

    let (samples, channels) = play(&project);
    assert_eq!(
        samples.len() / channels,
        4 * RATE as usize,
        "the stream is the footprint to the sample"
    );
    let mono: Vec<f32> = samples.chunks(channels).map(|f| f[0]).collect();

    let mut worst = 0f64;
    let mut seen = 0;
    for k in 1.. {
        // The click's centre in the source: its start plus half its 2 ms.
        let source_secs = f64::from(k) * 0.25 + 0.001;
        if source_secs > f64::from(SOURCE_SECS) - 0.01 {
            break;
        }
        let expected = ramp_timeline_at(&keys, source_secs * FPS) / FPS;
        let Some(at) = click_centre(&mono, expected, 0.04) else {
            panic!("click {k}: nothing near {expected:.4} s");
        };
        worst = worst.max((at - expected).abs());
        // Heard whole: not smeared to nothing, not doubled.
        let near = |x: f64| ((x * f64::from(RATE)) as usize).min(mono.len());
        let loudest = mono[near(expected - 0.01)..near(expected + 0.01)]
            .iter()
            .fold(0f32, |m, v| m.max(v.abs()));
        assert!(loudest > 0.3, "click {k} is only {loudest:.2} loud");
        assert!(
            (at - expected).abs() < 0.001,
            "click {k} lands at {at:.4} s, the ramp's mapping says {expected:.4} s"
        );
        seen += 1;
    }
    assert!(seen >= 20, "{seen} clicks checked; worst error {worst:.5} s");
}

/// A pure tone through the same ramp keeps its pitch: the ramp moves where the audio
/// is read, never how fast a period is played, so the zero crossings stay 440 Hz's.
#[test]
fn a_ramped_tone_keeps_its_pitch() {
    let wav = Scratch::file("ve_ramp_tone", "wav");
    write_tone_wav(&wav, SOURCE_SECS, 440.0);
    let mut project = Project::single(&wav, SOURCE_SECS * FPS as u32);
    let keys = [
        SpeedKey { at: 0, speed: Speed::NORMAL },
        SpeedKey { at: 120, speed: Speed::from_permille(2000) },
    ];
    project.set_speed_ramp(Lane::V1, 0, &keys).expect("the ramp sets");
    let (samples, channels) = play(&project);
    let mono: Vec<f32> = samples.chunks(channels).map(|f| f[0]).collect();
    // Skip the first and last half second: the stretch primes and drains there.
    let body = &mono[RATE as usize / 2..mono.len() - RATE as usize / 2];
    let crossings = body.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    let hz = crossings as f64 / (body.len() as f64 / f64::from(RATE));
    assert!((hz - 440.0).abs() < 8.0, "the tone plays at {hz:.1} Hz, not 440");
    // No seam clicks: a clean 440 Hz moves 0.03 a sample at most, a splice jumps.
    let jump = body.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    assert!(jump < 0.15, "a {jump:.2} jump between samples is a click");
}

fn write_tone_wav(path: &std::path::Path, secs: u32, hz: f64) {
    let frames = secs * RATE;
    let data_len = frames * 2;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for n in 0..frames {
        let v = 0.5 * (2.0 * std::f64::consts::PI * hz * f64::from(n) / f64::from(RATE)).sin();
        out.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, out).expect("write wav");
}
