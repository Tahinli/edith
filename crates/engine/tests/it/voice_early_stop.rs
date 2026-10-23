//! A ranged export stops mixing once its range is in: it drops the mix's receiver while the
//! mixer, the voice stage and the lane decoders are still producing. Each of them has to end
//! on its next send into the closed channel -- quietly, with no thread left behind -- and an
//! export cancelled mid-mix, and one started after it, must be unaffected.
//!
//! Its own test binary, alone in it: the check counts the process's audio threads by name,
//! which a test running beside it would disturb.

use std::path::Path;
use std::time::{Duration, Instant};

use engine::export::{ExportHandle, ExportSettings, Format};
use engine::project::{Clip, Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::voice::{self, Strength, VoiceFx};
use engine::Project;

const RATE: u32 = 44_100;
const FPS: f64 = 30.0;
const SECONDS: u32 = 60;

fn clip(frames: u32) -> Clip {
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
        source: 0,
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

/// A mono 16-bit wav of `SECONDS` of noise at about -30 dBFS with a 1 kHz tone over it.
fn write_wav(path: &Path) {
    let frames = SECONDS * RATE;
    let mut state = 1u64;
    let mut data = Vec::with_capacity(frames as usize * 2);
    for i in 0..frames {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let noise = ((state >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.11;
        let tone = 0.25 * (std::f32::consts::TAU * 1000.0 * i as f32 / RATE as f32).sin();
        data.extend_from_slice(&(((noise + tone) * 32767.0).round() as i16).to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out).expect("write the generated wav");
}

/// The process's threads that belong to a mix: lane decoders, voice stages, mixers.
fn audio_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("the thread list")
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .filter(|name| matches!(name.trim(), "audio-decode" | "voice-fx" | "audio-mix"))
        .count()
}

/// Waits up to two seconds for every mix thread to end, then says how many are left.
fn threads_left() -> usize {
    let started = Instant::now();
    while audio_threads() > 0 && started.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(5));
    }
    audio_threads()
}

fn wait(handle: &ExportHandle) -> Duration {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(120), "export hung");
        std::thread::sleep(Duration::from_millis(2));
    }
    started.elapsed()
}

#[test]
fn a_short_range_stops_the_mix_early_and_leaves_no_thread_behind() {
    let dir = Scratch::dir("ve_voice_early_stop");
    let wav = dir.join("voice.wav");
    write_wav(&wav);
    let mut project = Project::from_parts(
        vec![Source::new(&wav, 0)],
        vec![(LaneKind::Audio, vec![clip(SECONDS * FPS as u32)])],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("one audio lane over a generated file");
    let learned = voice::learn_noise(&project, Lane::A1, FPS, None, Some((0.1, 0.9))).unwrap();
    assert!(project.set_lane_voice(
        Lane::A1,
        VoiceFx::clean_voice(Strength::Strong, learned.profile, Some(learned.noise_db))
    ));
    let mut meta = engine::demux::Demuxer::open(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4"),
    )
    .expect("open test_av")
    .0;
    meta.frame_rate = FPS;
    let export = |range: Option<(u32, u32)>, name: &str| {
        engine::export::start(
            project.clone(),
            meta,
            &dir.join(name),
            &ExportSettings { format: Format::Wav, range, ..Default::default() },
            None,
        )
    };
    assert_eq!(threads_left(), 0, "the noise learn leaves no thread behind either");

    let full = export(None, "full.wav");
    let full_took = wait(&full);
    full.result().expect("an outcome").expect("the full export");
    assert_eq!(threads_left(), 0, "a finished mix leaves no thread behind");

    // One second from the start: no pre-roll, and the mix of the other 59 is never read.
    let short = export(Some((0, FPS as u32)), "short.wav");
    let short_took = wait(&short);
    short.result().expect("an outcome").expect("the ranged export");
    assert!(
        short_took * 4 < full_took,
        "one second took {short_took:?}, the whole minute {full_took:?}"
    );
    assert_eq!(threads_left(), 0, "the early stop leaves no mix thread behind");

    let cancelled = export(None, "cancelled.wav");
    cancelled.cancel();
    wait(&cancelled);
    assert!(cancelled.result().expect("an outcome").is_err(), "a cancel reports as one");
    assert_eq!(threads_left(), 0, "a cancel leaves no mix thread behind");

    let again = export(Some((0, FPS as u32)), "again.wav");
    wait(&again);
    again.result().expect("an outcome").expect("an export after the cancel");
    let bytes = std::fs::metadata(dir.join("again.wav")).unwrap().len();
    assert_eq!(bytes, std::fs::metadata(dir.join("short.wav")).unwrap().len(), "the same second again");
    assert_eq!(threads_left(), 0);
}
