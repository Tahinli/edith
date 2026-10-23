//! Auto-ducking where it has to be true: in the samples the mix sums, which
//! playback feeds the device and an export writes. Both reach that sum through
//! `AudioSession::open_mixed_streams_master_at_fade` with the project's own
//! `audio_gains` and `audio_ducks`, so this drives that opener with the same
//! lists the export hands it (`export.rs`), and reads the summed stream.
//!
//! The fixtures are generated here, so the levels are known exactly:
//! * `A1` is a dialogue stand-in: a 0.5 amplitude tone for one second, from
//!   1 s to 2 s, and silence everywhere else.
//! * `A2` is a music-bed stand-in: a 0.2 amplitude tone for the whole four
//!   seconds.
//!
//! `A2` is ducked under `A1` by 12 dB. The mix with the duck minus the mix
//! without it is then exactly `A2 * (gain - 1)` while the duck is in effect,
//! because `A1` is the same in both and only `A2` moves. So the test can read
//! `A2`'s own contribution off the sum, in the burst and out of it.

use std::path::{Path, PathBuf};

use engine::duck::Duck;
use engine::project::{Clip, Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::{AudioSession, Project};

const RATE: u32 = 44_100;
const FPS: f64 = 30.0;
const SECONDS: f64 = 4.0;
const DIALOGUE_AMP: f32 = 0.5;
const BED_AMP: f32 = 0.2;
const DEPTH_DB: f32 = -12.0;

fn clip(source: usize, frames: u32) -> Clip {
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
        start: 0,
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

/// A 16-bit stereo PCM WAV of `SECONDS`, the sample at each frame from `at`.
fn write_wav(path: &Path, at: impl Fn(f64) -> f32) {
    let frames = (SECONDS * f64::from(RATE)) as usize;
    let mut data = Vec::with_capacity(frames * 4);
    for i in 0..frames {
        let s = at(i as f64 / f64::from(RATE));
        let q = (s * 32767.0).round() as i16;
        data.extend_from_slice(&q.to_le_bytes());
        data.extend_from_slice(&q.to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // stereo
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out).expect("write the generated wav");
}

fn tone(hz: f64, amp: f32, t: f64) -> f32 {
    amp * (std::f64::consts::TAU * hz * t).sin() as f32
}

/// The summed stream, interleaved stereo, from the mix the export opens.
fn mixed(project: &Project, ducks: &[Vec<Duck>]) -> Vec<f32> {
    let sources = project.audio_sources();
    let segs = project.audio_segments_from(0, FPS);
    let (meta, chunks) = AudioSession::open_mixed_streams_master_at_fade(
        &sources,
        &segs,
        &[],
        &[],
        &[],
        FPS,
        &project.audio_gains(),
        ducks,
        &project.audio_voices(),
        project.limiter(),
        None,
    )
    .expect("open the mix")
    .expect("the project has audio");
    assert_eq!((meta.sample_rate, meta.channels), (RATE, 2));
    chunks.into_iter().flat_map(|c| c.samples).collect()
}

/// The `A2` contribution the duck removed, per sample, over `[from, to)`.
fn removed(without: &[f32], with: &[f32], from: f64, to: f64) -> f64 {
    let at = |secs: f64| (secs * f64::from(RATE)) as usize * 2;
    let (a, b) = (at(from), at(to));
    let diff: Vec<f32> = without[a..b]
        .iter()
        .zip(&with[a..b])
        .map(|(x, y)| x - y)
        .collect();
    (diff.iter().map(|&d| f64::from(d) * f64::from(d)).sum::<f64>() / diff.len() as f64).sqrt()
}

#[test]
fn a_bed_ducked_under_dialogue_is_quieter_only_while_the_dialogue_plays() {
    let dir = Scratch::dir("ve_duck_audio");
    let dialogue: PathBuf = dir.join("dialogue.wav");
    let bed: PathBuf = dir.join("bed.wav");
    write_wav(&dialogue, |t| {
        if (1.0..2.0).contains(&t) {
            tone(220.0, DIALOGUE_AMP, t)
        } else {
            0.0
        }
    });
    write_wav(&bed, |t| tone(440.0, BED_AMP, t));

    let frames = (SECONDS * FPS) as u32;
    let mut project = Project::from_parts(
        vec![Source::new(dialogue.clone(), 0), Source::new(bed.clone(), 0)],
        vec![
            (LaneKind::Audio, vec![clip(0, frames)]),
            (LaneKind::Audio, vec![clip(1, frames)]),
        ],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("two audio lanes over two generated files");
    // The duck is set the way an editor sets it: on A2, keyed off A1.
    let a2 = Lane::new(LaneKind::Audio, 1);
    let duck = Duck {
        under: 0,
        depth_db: DEPTH_DB,
        threshold_db: -40.0,
        attack_ms: 80.0,
        release_ms: 400.0,
    };
    assert!(project.set_lane_duck(a2, Some(duck)), "A2 ducks under A1");
    let ducks = project.audio_ducks();
    assert_eq!(ducks, vec![vec![], vec![duck]], "in the mix, A2 keys off mixed lane 0");

    let without = mixed(&project, &[]);
    let with = mixed(&project, &ducks);
    assert_eq!(without.len(), with.len(), "the same timeline either way");

    // Before the dialogue the key is silent, so nothing moves at all.
    assert!(
        removed(&without, &with, 0.0, 0.9) < 1e-7,
        "a silent key leaves the bed exactly as it was"
    );
    // In the burst, A2 is at the depth, so what it loses is (1 - depth) of
    // its own level: a bed at 0.2 amplitude is 0.2/sqrt(2) rms.
    let bed_rms = f64::from(BED_AMP) / std::f64::consts::SQRT_2;
    let depth_lin = 10f64.powf(f64::from(DEPTH_DB) / 20.0);
    let expected = (1.0 - depth_lin) * bed_rms;
    let in_burst = removed(&without, &with, 1.3, 1.8);
    assert!(
        (in_burst - expected).abs() < expected * 0.03,
        "the bed is ducked to its depth under the dialogue: removed {in_burst:.5}, want {expected:.5}"
    );
    // Long after the dialogue the duck has released, and the bed is back.
    assert!(
        removed(&without, &with, 2.6, 3.8) < 1e-5,
        "the bed is back to its level once the dialogue has gone"
    );
}

/// A bed ducked under two keys at once: each key's burst pulls the bed down
/// to its depth, and the bed is back in between, so both ducks are heard and
/// neither is lost to the other being set later.
#[test]
fn a_bed_ducked_under_two_keys_drops_under_each_of_them() {
    let dir = Scratch::dir("ve_duck_two_keys");
    let first: PathBuf = dir.join("first.wav");
    let second: PathBuf = dir.join("second.wav");
    let bed: PathBuf = dir.join("bed.wav");
    write_wav(&first, |t| {
        if (1.0..2.0).contains(&t) {
            tone(220.0, DIALOGUE_AMP, t)
        } else {
            0.0
        }
    });
    write_wav(&second, |t| {
        if (3.0..3.5).contains(&t) {
            tone(330.0, DIALOGUE_AMP, t)
        } else {
            0.0
        }
    });
    write_wav(&bed, |t| tone(440.0, BED_AMP, t));

    let frames = (SECONDS * FPS) as u32;
    let mut project = Project::from_parts(
        vec![
            Source::new(first.clone(), 0),
            Source::new(second.clone(), 0),
            Source::new(bed.clone(), 0),
        ],
        vec![
            (LaneKind::Audio, vec![clip(0, frames)]),
            (LaneKind::Audio, vec![clip(1, frames)]),
            (LaneKind::Audio, vec![clip(2, frames)]),
        ],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("three audio lanes over three generated files");
    let a3 = Lane::new(LaneKind::Audio, 2);
    let under_first = Duck {
        under: 0,
        depth_db: DEPTH_DB,
        threshold_db: -40.0,
        attack_ms: 80.0,
        release_ms: 400.0,
    };
    let under_second = Duck {
        under: 1,
        ..under_first
    };
    assert!(project.set_lane_duck(a3, Some(under_first)), "A3 ducks under A1");
    assert!(project.set_lane_duck(a3, Some(under_second)), "and under A2 too");
    let ducks = project.audio_ducks();
    assert_eq!(ducks[2], vec![under_first, under_second], "both kept, in order");

    let without = mixed(&project, &[]);
    let with = mixed(&project, &ducks);

    // Each burst is alone in its window, so the bed loses (1 - depth) of its
    // level under that key and under no other.
    let bed_rms = f64::from(BED_AMP) / std::f64::consts::SQRT_2;
    let depth_lin = 10f64.powf(f64::from(DEPTH_DB) / 20.0);
    let expected = (1.0 - depth_lin) * bed_rms;
    for (from, to, key) in [(1.3, 1.8, "A1"), (3.1, 3.4, "A2")] {
        let got = removed(&without, &with, from, to);
        assert!(
            (got - expected).abs() < expected * 0.03,
            "under {key} the bed drops to its depth: removed {got:.5}, want {expected:.5}"
        );
    }
    // Between the bursts both keys are silent or released, and the bed is back.
    assert!(
        removed(&without, &with, 2.6, 2.9) < 1e-5,
        "the bed is back between the two keys"
    );
}

/// Clearing one of two keys leaves the other in force: the bed ducks under the
/// key that stays and not under the one that went.
#[test]
fn clearing_one_key_leaves_the_other_duck_in_the_mix() {
    let dir = Scratch::dir("ve_duck_clear_key");
    let first: PathBuf = dir.join("first.wav");
    let second: PathBuf = dir.join("second.wav");
    let bed: PathBuf = dir.join("bed.wav");
    write_wav(&first, |t| {
        if (1.0..2.0).contains(&t) {
            tone(220.0, DIALOGUE_AMP, t)
        } else {
            0.0
        }
    });
    write_wav(&second, |t| {
        if (3.0..3.5).contains(&t) {
            tone(330.0, DIALOGUE_AMP, t)
        } else {
            0.0
        }
    });
    write_wav(&bed, |t| tone(440.0, BED_AMP, t));
    let frames = (SECONDS * FPS) as u32;
    let mut project = Project::from_parts(
        vec![
            Source::new(first.clone(), 0),
            Source::new(second.clone(), 0),
            Source::new(bed.clone(), 0),
        ],
        vec![
            (LaneKind::Audio, vec![clip(0, frames)]),
            (LaneKind::Audio, vec![clip(1, frames)]),
            (LaneKind::Audio, vec![clip(2, frames)]),
        ],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("three audio lanes");
    let a3 = Lane::new(LaneKind::Audio, 2);
    let under_first = Duck::new(0, DEPTH_DB);
    assert!(project.set_lane_duck(a3, Some(under_first)));
    assert!(project.set_lane_duck(a3, Some(Duck::new(1, DEPTH_DB))));
    assert!(project.clear_lane_duck(a3, 1), "the A2 key goes");
    let ducks = project.audio_ducks();
    assert_eq!(ducks[2], vec![under_first], "only the A1 key is left");

    let without = mixed(&project, &[]);
    let with = mixed(&project, &ducks);
    assert!(
        removed(&without, &with, 3.1, 3.4) < 1e-5,
        "with A2 gone, its burst no longer ducks the bed"
    );
    let bed_rms = f64::from(BED_AMP) / std::f64::consts::SQRT_2;
    let depth_lin = 10f64.powf(f64::from(DEPTH_DB) / 20.0);
    let expected = (1.0 - depth_lin) * bed_rms;
    let got = removed(&without, &with, 1.3, 1.8);
    assert!(
        (got - expected).abs() < expected * 0.03,
        "the A1 key still ducks the bed: removed {got:.5}, want {expected:.5}"
    );
}

/// Two ducks on one lane are saved and reopened as two: the file names each
/// key on its own `duck` line, and the reopened lane has both back in order.
#[test]
fn two_ducks_on_one_lane_survive_a_save_and_reopen() {
    let mut s = engine::PlaybackSession::open(asset("test_av.mp4")).expect("open");
    s.set_gain(0.0);
    let a2 = s.add_lane(LaneKind::Audio);
    let a3 = s.add_lane(LaneKind::Audio);
    let end = s.timeline_duration();
    assert!(s.place_at(a2, end, clip(0, 150)), "a key on A2");
    assert!(s.place_at(a3, end, clip(0, 150)), "a bed on A3");
    assert!(s.set_lane_duck(a3, Some(Duck::new(0, DEPTH_DB))), "A3 under A1");
    assert!(s.set_lane_duck(a3, Some(Duck::new(1, DEPTH_DB))), "and under A2");
    assert_eq!(s.lane_duck_list(a3).len(), 2);

    let dir = Scratch::dir("ve_duck_two_keys_save");
    let path = dir.join("two.edith");
    s.save_project(&path).expect("save");
    let back = engine::PlaybackSession::open_project(&path).expect("reopen");
    let list = back.lane_duck_list(a3);
    assert_eq!(list.len(), 2, "both ducks came back");
    assert_eq!(list[0].under, 0);
    assert_eq!(list[1].under, 1);
    assert!(back.load_warnings().is_empty(), "{:?}", back.load_warnings());
}

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}
