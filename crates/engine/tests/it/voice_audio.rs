//! Voice cleanup where it has to be true: in the samples the mix sums, which playback feeds
//! the device and an export writes. Both reach them through
//! `AudioSession::open_mixed_streams_master_at_fade` with the project's own `audio_voices`,
//! so this drives that opener with the lists the export hands it.
//!
//! The fixture is generated: one lane of 4 s, white noise at -30 dBFS throughout and a
//! -12 dBFS 1 kHz tone over it from 2 s to 4 s. The first two seconds are the noise alone.

use std::path::Path;

use engine::project::{Clip, Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::voice::{self, Strength, VoiceFx};
use engine::{AudioSession, Project};

const RATE: u32 = 44_100;
const FPS: f64 = 30.0;
const SECONDS: f64 = 4.0;

fn clip(source: usize, frames: u32) -> Clip {
    Clip {
        speed_keys: Vec::new(),
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
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
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        transition_kind: Default::default(),
        reversed: false,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
    }
}

/// Deterministic noise in about -0.5..0.5 per sample index, scaled to the given rms.
fn noise(i: usize, rms: f32) -> f32 {
    let mut sum = 0.0f32;
    for k in 0..4u64 {
        let mut x = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(k * 0xD1B5_4A32_D192_ED03);
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 32;
        sum += (x >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
    }
    sum * rms / (4.0f32 / 12.0).sqrt()
}

fn write_wav(path: &Path, at: impl Fn(usize) -> f32) {
    let frames = (SECONDS * f64::from(RATE)) as usize;
    let mut data = Vec::with_capacity(frames * 4);
    for i in 0..frames {
        let q = (at(i) * 32767.0).round() as i16;
        data.extend_from_slice(&q.to_le_bytes());
        data.extend_from_slice(&q.to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out).expect("write the generated wav");
}

fn project(wav: &Path) -> Project {
    let frames = (SECONDS * FPS) as u32;
    Project::from_parts(
        vec![Source::new(wav, 0)],
        vec![(LaneKind::Audio, vec![clip(0, frames)])],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("one audio lane over a generated file")
}

fn mixed(project: &Project) -> Vec<f32> {
    let (meta, chunks) = AudioSession::open_mixed_streams_master_at_fade(
        &project.audio_sources(),
        &project.audio_segments_from(0, FPS),
        &project.audio_eqs_from(0, FPS),
        &project.audio_speeds_from(0, FPS),
        &project.audio_fades_from(0, FPS),
        FPS,
        &project.audio_gains(),
        &project.audio_ducks(),
        &project.audio_voices(),
        project.limiter(),
        None,
    )
    .expect("open the mix")
    .expect("the project has audio");
    assert_eq!((meta.sample_rate, meta.channels), (RATE, 2));
    chunks.into_iter().flat_map(|c| c.samples).collect()
}

/// Left channel only, `[from, to)` seconds.
fn left(x: &[f32], from: f64, to: f64) -> Vec<f32> {
    let at = |s: f64| (s * f64::from(RATE)) as usize;
    x[at(from) * 2..at(to) * 2].as_chunks::<2>().0.iter().map(|f| f[0]).collect()
}

fn rms_db(x: &[f32]) -> f32 {
    20.0 * (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt().max(1e-12).log10()
}

/// Level of the 1 kHz component in the left channel over `[from, to)` seconds, dBFS peak.
fn tone_db(x: &[f32], from: f64, to: f64) -> f32 {
    let at = |s: f64| (s * f64::from(RATE)) as usize;
    let (mut s, mut c) = (0.0f64, 0.0f64);
    let (a, b) = (at(from), at(to));
    for i in a..b {
        let ph = std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(RATE);
        s += f64::from(x[i * 2]) * ph.sin();
        c += f64::from(x[i * 2]) * ph.cos();
    }
    (20.0 * (2.0 * (s * s + c * c).sqrt() / (b - a) as f64).log10()) as f32
}

#[test]
fn cleaning_a_lane_takes_the_noise_out_of_the_mix_and_keeps_the_tone_and_the_length() {
    let dir = Scratch::dir("ve_voice_audio");
    let wav = dir.join("voice.wav");
    write_wav(&wav, |i| {
        let t = i as f64 / f64::from(RATE);
        let tone = if t >= 2.0 { 0.25 * (std::f64::consts::TAU * 1000.0 * t).sin() as f32 } else { 0.0 };
        noise(i, 0.0316) + tone
    });
    let mut project = project(&wav);
    let a1 = Lane::A1;
    let plain = mixed(&project);
    assert!(project.audio_voices().iter().all(|v| !v.is_active()), "nothing on by default");

    // The noise is learned from the quietest stretch, found by itself: it is in the first two
    // seconds, where only the noise plays.
    let learned = voice::learn_noise(&project, a1, FPS, None, None).expect("a stretch to learn from");
    assert!(learned.from.0 >= 0.0 && learned.from.1 <= 2.05, "found at {:?}", learned.from);
    assert!((learned.noise_db + 30.0).abs() < 1.5, "learned {:.1} dB", learned.noise_db);
    // ...or from a range given by hand, which has to agree about the level.
    let by_hand = voice::learn_noise(&project, a1, FPS, None, Some((0.2, 1.7))).expect("the given range");
    assert!((by_hand.noise_db - learned.noise_db).abs() < 1.0);
    assert!(voice::learn_noise(&project, a1, FPS, None, Some((0.2, 0.21))).is_err(), "too short to learn");

    // Noise reduction alone, switched on without a profile, is refused.
    let mut fx = VoiceFx::clean_voice(Strength::Medium, learned.profile.clone(), Some(learned.noise_db));
    fx.gate.on = false;
    fx.comp.on = false;
    fx.deess.on = false;
    fx.nr.strength = 0.8;
    let mut bare = fx.clone();
    bare.nr.profile.clear();
    assert!(!project.set_lane_voice(a1, bare), "nr on with no profile");
    assert!(project.set_lane_voice(a1, fx.clone()));
    assert!(!project.set_lane_voice(a1, fx.clone()), "unchanged is no change");

    let cleaned = mixed(&project);
    assert_eq!(cleaned.len(), plain.len(), "the stage keeps the lane its length");
    let drop = rms_db(&left(&plain, 0.5, 1.8)) - rms_db(&left(&cleaned, 0.5, 1.8));
    assert!(drop >= 15.0, "the noise-only stretch fell {drop:.1} dB");
    let kept = tone_db(&cleaned, 2.5, 3.8) - tone_db(&plain, 2.5, 3.8);
    assert!(kept.abs() < 1.0, "the tone moved by {kept:.2} dB");

    // One undo step takes it all back, and the mix is the bit-exact plain one again.
    assert!(project.undo());
    assert!(!project.lane_voice(a1).is_active());
    assert_eq!(mixed(&project), plain, "no processors on: the same samples as before");
}

#[test]
fn a_one_click_preset_is_one_undo_step() {
    let dir = Scratch::dir("ve_voice_preset");
    let wav = dir.join("voice.wav");
    write_wav(&wav, |i| noise(i, 0.0316));
    let mut project = project(&wav);
    let a1 = Lane::A1;
    let learned = voice::learn_noise(&project, a1, FPS, None, None).unwrap();
    let fx = VoiceFx::clean_voice(Strength::Medium, learned.profile, Some(learned.noise_db));
    assert!(project.set_lane_voice(a1, fx.clone()));
    assert_eq!(project.lane_voice(a1), fx);
    assert_eq!(project.lane_voices(), vec![fx.clone()], "saved by its own lane");
    assert!(project.undo(), "the whole preset is one step");
    assert_eq!(project.lane_voice(a1), VoiceFx::default());
    assert!(project.redo());
    assert_eq!(project.lane_voice(a1), fx);
    // A video lane takes no voice.
    assert!(!project.set_lane_voice(Lane::V1, fx));
}

/// A lane an hour long (900 four-second clips of one file) is searched for noise by reading a
/// bounded amount of it, so the time does not grow with the lane.
#[test]
fn the_noise_search_of_an_hour_long_lane_is_bounded() {
    let dir = Scratch::dir("ve_voice_hour");
    let wav = dir.join("voice.wav");
    write_wav(&wav, |i| noise(i, 0.0316));
    let clips = 900u32;
    let each = (SECONDS * FPS) as u32;
    let lane: Vec<Clip> = (0..clips)
        .map(|k| {
            let mut c = clip(0, each);
            c.start = k * each;
            c
        })
        .collect();
    let project = Project::from_parts(
        vec![Source::new(&wav, 0)],
        vec![(LaneKind::Audio, lane)],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("an hour of clips");
    let started = std::time::Instant::now();
    let learned = voice::learn_noise(&project, Lane::A1, FPS, None, None).expect("noise in an hour of noise");
    let took = started.elapsed();
    assert!((learned.noise_db + 30.0).abs() < 1.5, "learned {:.1} dB", learned.noise_db);
    assert!(learned.from.1 <= 3600.0 + 0.1);
    // Reading the whole hour decodes 3600 s of audio; the bounded search reads 60 s of it.
    assert!(took < std::time::Duration::from_secs(20), "an hour took {took:?}");
}

/// The full chain, noise reduction and de-esser included, keeps the lane where it was: a noise
/// burst comes out at the same sample, within one.
#[test]
fn the_full_chain_adds_no_delay() {
    let dir = Scratch::dir("ve_voice_align");
    let wav = dir.join("voice.wav");
    let burst = |i: usize| {
        let t = i as f64 / f64::from(RATE);
        if (2.0..2.3).contains(&t) { noise(i + 7_000_000, 0.2) } else { 0.0 }
    };
    write_wav(&wav, |i| noise(i, 0.0063) + burst(i));
    let mut project = project(&wav);
    let plain = mixed(&project);
    let learned = voice::learn_noise(&project, Lane::A1, FPS, None, Some((0.2, 1.7))).unwrap();
    let fx = VoiceFx::clean_voice(Strength::Medium, learned.profile, Some(learned.noise_db));
    assert!(fx.gate.on && fx.nr.on && fx.comp.on && fx.deess.on, "the whole chain");
    assert!(project.set_lane_voice(Lane::A1, fx));
    let cleaned = mixed(&project);
    assert_eq!(cleaned.len(), plain.len());
    let a = left(&plain, 2.05, 2.25);
    let from = (2.05 * f64::from(RATE)) as usize;
    let b: Vec<f32> = cleaned.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
    let (mut best, mut at) = (f64::MIN, 0i64);
    for lag in -200i64..=200 {
        let dot: f64 = a
            .iter()
            .enumerate()
            .map(|(i, &x)| f64::from(x) * f64::from(b[(from + i).wrapping_add_signed(lag as isize)]))
            .sum();
        if dot > best {
            (best, at) = (dot, lag);
        }
    }
    assert!(at.abs() <= 1, "the processed lane is {at} samples off");
}

/// A ranged export starts with the chain warm: its first second is the same span of the full export.
#[test]
fn a_ranged_export_matches_the_same_span_of_the_full_one() {
    use engine::export::{ExportSettings, Format};
    let dir = Scratch::dir("ve_voice_range");
    let wav = dir.join("voice.wav");
    write_wav(&wav, |i| {
        let t = i as f64 / f64::from(RATE);
        let tone = if t >= 1.0 { 0.25 * (std::f64::consts::TAU * 1000.0 * t).sin() as f32 } else { 0.0 };
        noise(i, 0.0316) + tone
    });
    let mut project = project(&wav);
    let learned = voice::learn_noise(&project, Lane::A1, FPS, None, Some((0.1, 0.9))).unwrap();
    let fx = VoiceFx::clean_voice(Strength::Strong, learned.profile, Some(learned.noise_db));
    assert!(project.set_lane_voice(Lane::A1, fx));
    let meta = engine::demux::Demuxer::open(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4"))
        .expect("open test_av")
        .0;
    let export = |fps: f64, range: Option<(u32, u32)>, name: &str| -> Vec<f32> {
        let out = dir.join(name);
        let mut meta = meta;
        meta.frame_rate = fps;
        let handle = engine::export::start(
            project.clone(),
            meta,
            &out,
            &ExportSettings { format: Format::Wav, range, ..Default::default() },
            None,
        );
        let started = std::time::Instant::now();
        while !handle.is_finished() {
            assert!(started.elapsed().as_secs() < 60, "export hung");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        handle.result().expect("an outcome").expect("a wav");
        let (_, chunks) = AudioSession::open(&out).unwrap().unwrap();
        chunks.into_iter().flat_map(|c| c.samples).collect()
    };
    // 30 fps, and the two rates whose frames are no whole number of samples: the range
    // starts where rounding the pre-roll's own length would land one sample off.
    for (name, fps, pre) in [("30", 30.0, 15u32), ("29.97", 30000.0 / 1001.0, 15), ("23.976", 24000.0 / 1001.0, 12)] {
        let at = |f: u32| (f64::from(f) / fps * f64::from(RATE)).round() as usize;
        let lone = |pre: u32| (f64::from(pre) / fps * f64::from(RATE)).round() as usize;
        let from = if fps == 30.0 {
            60
        } else {
            (pre..120).find(|&f| at(f) - at(f - pre) != lone(pre)).expect("a start where the roundings part")
        };
        let to = from + (fps * 1.5) as u32;
        let full = export(fps, None, &format!("full_{name}.wav"));
        let ranged = export(fps, Some((from, to)), &format!("ranged_{name}.wav"));
        let want = (f64::from(to - from) / fps * f64::from(RATE)).round() as usize * 2;
        assert!(ranged.len().abs_diff(want) <= 4, "{name}: the range is {} samples, not {want}", ranged.len());
        // The first second of the range against the same second of the full file, at the
        // same sample: any lag but none is a miss.
        let span = RATE as usize * 2; // one second, two channels
        let start = at(from) * 2;
        let worst = |lag: isize| {
            let b = &full[start.wrapping_add_signed(lag * 2)..];
            ranged[..span].iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max)
        };
        let best = (-2isize..=2).min_by(|&p, &q| worst(p).total_cmp(&worst(q))).unwrap();
        assert_eq!(best, 0, "{name}: the ranged export sits {best} samples off the full one");
        // The noise reduction's frames sit on a grid counted from where the mix opened, so the
        // left-over noise differs a little; the difference stays some 60 dB down.
        let err = ranged[..span].iter().zip(&full[start..]).map(|(x, y)| f64::from(x - y).powi(2)).sum::<f64>() / span as f64;
        let err_db = 10.0 * err.log10();
        assert!(err_db < -50.0, "{name}: the ranged export's first second differs by {err_db:.1} dB rms");
    }
}
