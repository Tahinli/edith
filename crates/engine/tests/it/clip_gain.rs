//! The per-clip gain where it has to be true: in the samples playback feeds the
//! device and in the samples an export writes. Both come off the same opener
//! with the same per-segment envelope ([`engine::Project::audio_fades_from`]),
//! so a clip at -6 dB must mix at half amplitude through that one door.
//!
//! The fixture is the 440 Hz / 880 Hz stereo tone in `test_av.mp4`; the assertion
//! is a ratio of RMS levels, never a level, so the fixture's own loudness drops
//! out. No picture is decoded.

use std::path::PathBuf;

use engine::GainKey;
use engine::project::{Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::{AudioSession, Clip, Project};

const RATE: u32 = 44_100;
const FPS: f64 = 30.0;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn clip(frames: u32, gain_db: f32) -> Clip {
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
        gain_db,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        start: 0,
        in_frame: 0,
        out_frame: frames,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
        transform: None,
    }
}

/// One video clip and one audio clip of `gain_db`, both from `test_av.mp4`.
fn project(gain_db: f32) -> Project {
    let frames = 90;
    Project::from_parts(
        vec![Source::new(asset("test_av.mp4"), 0)],
        vec![
            (LaneKind::Video, vec![clip(frames, 0.0)]),
            (LaneKind::Audio, vec![clip(frames, gain_db)]),
        ],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("a one-source project")
}

/// What the device is fed, drained to the end: the opener `PlaybackSession::seek`
/// and the export both call, with the same segments, eqs, speeds and fades.
fn play(project: &Project) -> Vec<f32> {
    let sources = project.audio_sources();
    let segs = project.audio_segments_from(0, FPS);
    let eqs = project.audio_eqs_from(0, FPS);
    let speeds = project.audio_speeds_from(0, FPS);
    let fades = project.audio_fades_from(0, FPS);
    let (meta, chunks) = AudioSession::open_mixed_streams_master_at_fade(
        &sources,
        &segs,
        &eqs,
        &speeds,
        &fades,
        FPS,
        &project.audio_gains(),
        &project.audio_ducks(),
        &project.audio_voices(),
        project.limiter(),
        None,
    )
    .expect("open the timeline's audio")
    .expect("the timeline has audio");
    assert_eq!((meta.sample_rate, meta.channels), (RATE, 2));
    chunks.into_iter().flat_map(|c| c.samples).collect()
}

/// Root mean square of `[from, to)` seconds of the interleaved stereo stream.
fn rms(samples: &[f32], from: f64, to: f64) -> f64 {
    let at = |secs: f64| (secs * f64::from(RATE)) as usize * 2;
    let window = &samples[at(from).min(samples.len())..at(to).min(samples.len())];
    assert!(!window.is_empty(), "no samples in [{from}, {to})");
    (window
        .iter()
        .map(|s| f64::from(*s) * f64::from(*s))
        .sum::<f64>()
        / window.len() as f64)
        .sqrt()
}

#[test]
fn a_clip_at_minus_six_db_mixes_at_half_amplitude() {
    let unity = rms(&play(&project(0.0)), 0.2, 2.8);
    let quiet = rms(&play(&project(-6.0)), 0.2, 2.8);
    let ratio = quiet / unity;
    let want = 10f64.powf(-6.0 / 20.0);
    assert!(
        (ratio - want).abs() < 0.01,
        "-6 dB should mix at {want:.4} of unity, got {ratio:.4}"
    );
}

#[test]
fn a_gained_clip_on_a_video_lane_is_refused_and_the_audio_is_untouched() {
    let mut p = project(0.0);
    assert!(
        !p.set_clip_gain(Lane::V1, 0, -6.0),
        "a video clip takes no gain"
    );
    assert!(p.set_clip_gain(Lane::new(LaneKind::Audio, 0), 0, -6.0));
    assert_eq!(p.clip_gain(Lane::V1, 0), Some(0.0));
}

/// The same tone through a volume envelope that falls from 0 dB at frame 0 to
/// -20 dB at frame 30 (1 s at 30 fps). At frame 15 (0.5 s) it is halfway, -10 dB;
/// at frame 45 (1.5 s) it is past the last point and holds at -20 dB. Each
/// level is measured as a ratio over a 40 ms window against the same window of
/// the unshaped clip, so the tone's own level drops out.
#[test]
fn a_gain_envelope_falls_from_unity_to_minus_twenty_over_thirty_frames() {
    let base = play(&project(0.0));
    let mut shaped_project = project(0.0);
    assert!(shaped_project.set_gain_keys(
        Lane::new(LaneKind::Audio, 0),
        0,
        vec![GainKey::new(0, 0.0), GainKey::new(30, -20.0)],
    ));
    let shaped = play(&shaped_project);
    let db_at = |secs: f64| {
        let ratio = rms(&shaped, secs - 0.02, secs + 0.02) / rms(&base, secs - 0.02, secs + 0.02);
        20.0 * ratio.log10()
    };
    let mid = db_at(0.5);
    assert!(
        (mid + 10.0).abs() < 1.0,
        "frame 15 should sit near -10 dB, got {mid:.2}"
    );
    let held = db_at(1.5);
    assert!(
        (held + 20.0).abs() < 0.5,
        "frame 45 should hold at -20 dB, got {held:.2}"
    );
}
