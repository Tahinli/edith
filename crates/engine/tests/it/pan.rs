//! Clip and lane pan where it has to be true: in the samples playback feeds the
//! device and in the samples an export writes. Both come off the same opener
//! with the same per-segment envelope ([`engine::Project::audio_fades_from`]),
//! so a clip at -6 dB must mix at half amplitude through that one door.
//!
//! The fixture is the 440 Hz / 880 Hz stereo tone in `test_av.mp4`; the assertion
//! is a ratio of RMS levels, never a level, so the fixture's own loudness drops
//! out. No picture is decoded.

use std::path::PathBuf;

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

fn chan_rms(samples: &[f32], ch: usize) -> f64 {
    let v: Vec<f64> = samples.chunks(2).skip(8820).map(|f| f64::from(f[ch])).collect();
    (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt()
}

#[test]
fn a_clip_panned_hard_right_silences_the_left_and_centre_is_untouched() {
    let a1 = Lane::new(LaneKind::Audio, 0);
    let centre = play(&project(0.0));
    let (l0, r0) = (chan_rms(&centre, 0), chan_rms(&centre, 1));
    assert!(l0 > 0.01 && r0 > 0.01);
    let mut p = project(0.0);
    assert!(p.set_clip_pan(a1, 0, 1.0));
    assert!(!p.set_clip_pan(a1, 0, 1.0), "already set");
    assert!(!p.set_clip_pan(Lane::V1, 0, 0.5), "video takes no pan");
    let hard = play(&p);
    assert!(chan_rms(&hard, 0) < 1e-6, "left is gone");
    assert!((chan_rms(&hard, 1) / r0 - 1.0).abs() < 1e-3, "right is as it was");
    p.undo();
    assert_eq!(p.clip_pan(a1, 0), Some(0.0));
}

#[test]
fn a_lane_pan_stacks_on_the_clip_pan() {
    let a1 = Lane::new(LaneKind::Audio, 0);
    let mut p = project(0.0);
    assert!(p.set_lane_pan(a1, -1.0));
    assert!(!p.set_lane_pan(Lane::V1, 0.3));
    let played = play(&p);
    assert!(chan_rms(&played, 1) < 1e-6, "right is gone");
    assert!(chan_rms(&played, 0) > 0.01);
    assert!(p.set_clip_pan(a1, 0, 0.5));
    let (l, r) = engine::project::pan_gains(-0.5, 1.0);
    assert!(l == 0.0 && r < 0.71 && r > 0.70, "{l} {r}");
}
