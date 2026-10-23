//! Reverse clips: the picture a reversed clip shows is the forward picture run
//! backwards, its sound is the forward sound mirrored, it composes with speed,
//! and it survives a save. The picture is compared by hash against the same
//! source played forward through the same session path, so no tolerance is
//! needed: a reversed frame is bit-identical to the forward frame it mirrors.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant};

use engine::audio::AudioSession;
use engine::project::{Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::{Clip, PlaybackSession, Project};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn open() -> PlaybackSession {
    let s = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av.mp4");
    s.set_gain(0.0);
    s
}

/// Every frame of the timeline, as timeline index -> pixel hash.
fn hashes(s: &mut PlaybackSession) -> HashMap<u32, u64> {
    s.seek(0.0);
    s.play();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut out = HashMap::new();
    loop {
        s.tick();
        while let Some(f) = s.try_frame() {
            let mut h = DefaultHasher::new();
            f.bgra.hash(&mut h);
            out.insert(f.index, h.finish());
        }
        if s.is_eos() {
            return out;
        }
        assert!(Instant::now() < deadline, "still playing after 60 s");
        sleep(Duration::from_millis(2));
    }
}

/// The forward decode of test_av.mp4, shared by every test that compares against it.
fn forward_hashes() -> &'static HashMap<u32, u64> {
    static FORWARD: OnceLock<HashMap<u32, u64>> = OnceLock::new();
    FORWARD.get_or_init(|| hashes(&mut open()))
}

#[test]
fn a_reversed_clip_shows_the_forward_frames_backwards() {
    let forward = forward_hashes();
    assert_eq!(forward.len(), 150);
    let mut s = open();
    s.set_reverse(Lane::V1, 0, true).expect("reverse");
    assert!(s.reversed_of(Lane::V1, 0));
    let back = hashes(&mut s);
    assert_eq!(back.len(), 150, "every frame arrives");
    for t in 0..150u32 {
        assert_eq!(back[&t], forward[&(149 - t)], "timeline frame {t}");
    }
    // And forward again.
    s.set_reverse(Lane::V1, 0, false).expect("unreverse");
    assert!(!s.reversed_of(Lane::V1, 0));
    assert_eq!(hashes(&mut s), *forward);
}

#[test]
fn reverse_composes_with_speed() {
    let forward = forward_hashes();
    let mut s = open();
    s.set_speed(Lane::V1, 0, Speed::from_permille(2000)).expect("2x");
    s.set_reverse(Lane::V1, 0, true).expect("reverse");
    let back = hashes(&mut s);
    assert_eq!(back.len(), 75);
    for t in 0..75u32 {
        assert_eq!(back[&t], forward[&(149 - 2 * t)], "timeline frame {t} at 2x");
    }
}

#[test]
fn reverse_survives_save_and_load_and_undo() {
    let mut s = open();
    s.set_reverse(Lane::V1, 0, true).expect("reverse");
    let out = Scratch::file("edith-reverse-save", "edith");
    s.save_project(&out).expect("save");
    let head = std::fs::read(&out).expect("read");
    assert!(head.starts_with(b"edith 38"), "the newest dialect is written");
    let back = PlaybackSession::open_project(&out).expect("open");
    assert!(back.load_warnings().is_empty(), "{:?}", back.load_warnings());
    assert!(back.reversed_of(Lane::V1, 0));
    assert!(back.reversed_of(Lane::A1, 0), "the take reverses as one");
    s.undo();
    assert!(!s.reversed_of(Lane::V1, 0), "one undo step");
}

/// A mono 48 kHz second-and-a-bit with a click at each of `at` (seconds).
fn write_clicks(path: &Path, at: &[f64], secs: f64) {
    let rate = 48_000u32;
    let frames = (f64::from(rate) * secs) as usize;
    let mut pcm = vec![0i16; frames];
    for t in at {
        pcm[(t * f64::from(rate)) as usize] = 29_000;
    }
    let data_len = (frames * 2) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for v in pcm {
        out.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, out).expect("write clicks");
}

fn audio_clip(in_frame: u32, out_frame: u32, speed: u16, reversed: bool) -> Clip {
    Clip {
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        speed_keys: Vec::new(),
        transition_kind: Default::default(),
        reversed,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
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
        in_frame,
        out_frame,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::from_permille(speed),
        transform: None,
    }
}

/// Where the clicks of a one-clip audio timeline land, in seconds.
fn clicks_of(file: &Path, clip: Clip) -> Vec<f64> {
    let fps = 30.0;
    let project = Project::from_parts(
        vec![Source::new(file, 0)],
        vec![(LaneKind::Audio, vec![clip])],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("an audio project");
    let (meta, rx) = AudioSession::open_mixed_streams_master_at_fade(
        &project.audio_sources(),
        &project.audio_segments_from(0, fps),
        &project.audio_eqs_from(0, fps),
        &project.audio_speeds_from(0, fps),
        &project.audio_fades_from(0, fps),
        fps,
        &project.audio_gains(),
        &project.audio_ducks(),
        &project.audio_voices(),
        project.limiter(),
        None,
    )
    .expect("open")
    .expect("audio");
    let rate = f64::from(meta.sample_rate);
    let ch = usize::from(meta.channels);
    let mut out = Vec::new();
    let mut base = None;
    let mut run: Option<(u64, f32)> = None;
    for chunk in rx {
        let base = *base.get_or_insert(chunk.start_sample);
        for (i, f) in chunk.samples.chunks(ch).enumerate() {
            let n = chunk.start_sample - base + i as u64;
            let v = f[0].abs();
            match (run, v > 0.3) {
                (None, true) => run = Some((n, v)),
                (Some((_, b)), true) if v > b => run = Some((n, v)),
                (Some((at, _)), false) => {
                    out.push(at as f64 / rate);
                    run = None;
                }
                _ => {}
            }
        }
    }
    out
}

#[test]
fn reversed_audio_is_the_forward_samples_mirrored() {
    let file = Scratch::file("edith-reverse-clicks", "wav");
    write_clicks(&file, &[0.2, 0.22, 0.6, 1.3], 2.0);
    // Source frames 0..30 at 30 fps is the first second: three clicks.
    let fwd = clicks_of(&file, audio_clip(0, 30, 1000, false));
    assert_eq!(fwd.len(), 3, "{fwd:?}");
    let back = clicks_of(&file, audio_clip(0, 30, 1000, true));
    let mut want: Vec<f64> = fwd.iter().map(|t| 1.0 - t).collect();
    want.reverse();
    assert_eq!(back.len(), 3, "{back:?}");
    for (g, w) in back.iter().zip(&want) {
        assert!((g - w).abs() < 0.0002, "click at {g}, wanted {w}");
    }
    // A window that does not start at zero mirrors inside itself.
    let mid = clicks_of(&file, audio_clip(15, 45, 1000, true));
    // Clicks at 0.6 and 1.3 s sit in 0.5..1.5 s; mirrored they are 0.9 and 0.2.
    assert_eq!(mid.len(), 2, "{mid:?}");
    for (g, w) in mid.iter().zip([0.2, 0.9]) {
        assert!((g - w).abs() < 0.0002, "{mid:?}");
    }
}

#[test]
fn reversed_audio_at_double_speed_lands_where_the_picture_does() {
    let file = Scratch::file("edith-reverse-clicks-2x", "wav");
    write_clicks(&file, &[0.2, 0.6, 1.3], 2.0);
    // 60 source frames (2 s) at 2x is 30 timeline frames; a click at source
    // time t is heard at (2 - t) / 2.
    let back = clicks_of(&file, audio_clip(0, 60, 2000, true));
    assert!(!back.is_empty(), "something is heard");
    let want = [0.35, 0.7, 0.9];
    for w in want {
        assert!(
            back.iter().any(|g| (g - w).abs() < 0.03),
            "no click near {w} in {back:?}"
        );
    }
}

/// Plays backward from timeline frame `from` as a pull consumer would, to the
/// start: (frame, pixel hash) in the order they arrive.
fn backwards_from(s: &mut PlaybackSession, from: u32) -> Vec<(u32, u64)> {
    s.seek(f64::from(from) / s.meta().frame_rate);
    s.set_rate(-1.0);
    assert!(s.backward());
    s.play();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut out = Vec::new();
    loop {
        s.tick();
        while let Some(f) = s.try_frame() {
            let mut h = DefaultHasher::new();
            f.bgra.hash(&mut h);
            out.push((f.index, h.finish()));
        }
        if s.is_eos() {
            return out;
        }
        assert!(Instant::now() < deadline, "still playing after 60 s");
        sleep(Duration::from_millis(2));
    }
}

#[test]
fn backward_playback_draws_the_frames_from_the_playhead_down() {
    let forward = hashes(&mut open());
    let mut s = open();
    let got = backwards_from(&mut s, 90);
    let want: Vec<u32> = (0..=90).rev().collect();
    assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), want, "descending, none missing");
    for (t, h) in got {
        assert_eq!(h, forward[&t], "frame {t}");
    }
    s.halt_at_end();
    assert_eq!(s.now(), 0.0);
    assert_eq!(s.rate(), 1.0, "the end of a backward run is real time again");
    assert!(s.is_eos());
}

#[test]
fn backward_playback_crosses_cuts_and_plays_a_reversed_clip_forward_in_the_file() {
    let forward = hashes(&mut open());
    // Cut at 60 and reverse only the first piece: back from 100 the second piece
    // is drawn last frame first, then the first piece (reversed) frame by frame
    // up the file.
    let mut s = open();
    assert!(s.cut_at(60.0 / 30.0));
    s.set_reverse(Lane::V1, 0, true).expect("reverse the head piece");
    let got = backwards_from(&mut s, 100);
    assert_eq!(got.len(), 101);
    for (k, (t, h)) in got.iter().enumerate() {
        assert_eq!(*t, 100 - k as u32);
        let source = if *t >= 60 { *t } else { 59 - *t };
        assert_eq!(*h, forward[&source], "timeline frame {t}");
    }
}
