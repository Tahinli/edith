//! Audio stays on the picture's clock. The mixer numbers its output from the
//! first chunk, and the sample at which timeline frame `f` begins must be
//! `round(f * rate / fps)` -- within a sample. A source click at source time
//! `tau`, inside a cut that starts at timeline frame `start` and reads from
//! source frame `in`, must come out at `(start + (tau - in / fps) * fps) *
//! rate / fps`. The checks run through the same door the device and an audio
//! export use: `Project::audio_*` into `open_mixed_streams_master_at_fade`, on
//! hour-long timelines of many short cuts at each frame rate a camera shoots,
//! from sources at 44.1 kHz and 48 kHz, after a save and a reload.
//!
//! Sources carry one click every 20 ms and nothing else, so every click is an
//! unmistakable marker and the output has no other energy to confuse it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use engine::audio::{AudioChunk, AudioSession};
use engine::edith;
use engine::project::{Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::voice::{self, NoiseReduction, VoiceFx};
use engine::{Clip, Project};

/// The frame rates a camera shoots, as the exact ratios they are.
const RATES: [(&str, f64); 6] = [
    ("23.976", 24000.0 / 1001.0),
    ("25", 25.0),
    ("29.97", 30000.0 / 1001.0),
    ("30", 30.0),
    ("59.94", 60000.0 / 1001.0),
    ("60", 60.0),
];

/// A click every `rate / 50` samples, mono 16-bit, silence otherwise.
fn write_clicks(path: &Path, rate: u32, secs: u32) {
    let frames = rate * secs;
    let period = rate / 50;
    let data_len = frames * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
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
    for n in 0..frames {
        let v: i16 = if n % period == 0 { 29_491 } else { 0 };
        out.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, out).expect("write clicks");
}

/// One cut: timeline frames `start..start + (out - in)` read from source frames
/// `in..out` of source `source`.
#[derive(Clone, Copy, Debug)]
struct Shot {
    start: u32,
    in_frame: u32,
    out_frame: u32,
    source: usize,
    /// Thousandths of real time, as [`Speed`] counts them.
    speed: u16,
}

fn clip(shot: Shot) -> Clip {
    Clip {
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        transition_kind: Default::default(),
        speed_keys: Vec::new(),
        reversed: false,
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
        start: shot.start,
        in_frame: shot.in_frame,
        out_frame: shot.out_frame,
        source: shot.source,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::from_permille(shot.speed),
        transform: None,
    }
}

/// Cuts covering `total` timeline frames back to back, 1..=`max_len` frames
/// each, reading from one of `sources` source files of `src_secs` seconds.
fn shots(total: u32, fps: f64, src_secs: f64, sources: usize, max_len: u32, seed: u64) -> Vec<Shot> {
    let mut state = seed;
    let mut next = move |n: u32| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 33) % u64::from(n)) as u32
    };
    let src_frames = (src_secs * fps) as u32;
    let mut out = Vec::new();
    let mut at = 0;
    while at < total {
        let len = (1 + next(max_len)).min(total - at);
        let in_frame = next(src_frames - len);
        out.push(Shot {
            start: at,
            in_frame,
            out_frame: in_frame + len,
            source: next(sources as u32) as usize,
            speed: 1000,
        });
        at += len;
    }
    out
}

/// What a mix put out: the sample rate, where each click landed (in samples
/// from the first chunk) and how many samples in all.
struct Heard {
    rate: u32,
    clicks: Vec<f64>,
    samples: u64,
}

/// Where each click landed. A click is a run of samples above a small
/// threshold, reported at its loudest sample.
fn hear(rx: Receiver<AudioChunk>, channels: usize, rate: u32) -> Heard {
    let mut clicks = Vec::new();
    let mut run: Option<(u64, f32)> = None;
    let mut base = None;
    let mut samples = 0;
    for chunk in rx {
        let base = *base.get_or_insert(chunk.start_sample);
        for (i, frame) in chunk.samples.chunks(channels).enumerate() {
            let n = chunk.start_sample - base + i as u64;
            samples = n + 1;
            let v = frame[0].abs();
            match (run, v > 0.3) {
                (None, true) => run = Some((n, v)),
                (Some((_, best)), true) if v > best => run = Some((n, v)),
                (Some((at, _)), false) => {
                    clicks.push(at as f64);
                    run = None;
                }
                _ => {}
            }
        }
    }
    Heard {
        rate,
        clicks,
        samples,
    }
}

/// Where every click of `shots` should land at mix rate `out_rate`, in order.
/// `src_rates[s]` is the rate of source `s`; only real-time cuts are placed.
fn expected(shots: &[Shot], fps: f64, src_rates: &[u32], out_rate: u32) -> Vec<f64> {
    let mut out = Vec::new();
    for s in shots.iter().filter(|s| s.speed == 1000) {
        let src_rate = src_rates[s.source];
        let period = u64::from(src_rate / 50);
        // A click exactly on a cut's edge belongs to the cut that starts there;
        // the epsilon keeps float error from deciding which one that is.
        let at = |frame: u32| (f64::from(frame) / fps * f64::from(src_rate) - 1e-6).ceil() as u64;
        let (lo, hi) = (at(s.in_frame), at(s.out_frame));
        let mut k = lo.div_ceil(period) * period;
        while k < hi {
            let secs = f64::from(s.start) / fps
                + (k as f64 / f64::from(src_rate) - f64::from(s.in_frame) / fps);
            out.push(secs * f64::from(out_rate));
            k += period;
        }
    }
    out
}

/// A project of `shots` over `sources` (one file each), on one audio lane.
fn project_for(sources: &[PathBuf], shots: &[Shot]) -> Project {
    Project::from_parts(
        sources
            .iter()
            .map(|p| Source::new(p.clone(), 0))
            .collect(),
        vec![(LaneKind::Audio, shots.iter().map(|&s| clip(s)).collect())],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("an audio project over its sources")
}

/// Mixes `project`'s audio at `fps` the way the device and an export do.
fn mix(project: &Project, fps: f64) -> Heard {
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
    .expect("open the timeline's audio")
    .expect("the timeline has audio");
    hear(rx, usize::from(meta.channels), meta.sample_rate)
}

/// How far the mix's clicks sit from the timeline's, click by click.
#[derive(Debug, Default)]
struct Report {
    /// Expected clicks with no output click anywhere near them.
    missing: usize,
    /// Output clicks no expected click claims.
    extra: usize,
    /// Largest offset of a matched click, in samples.
    worst: f64,
}

/// A click further than this from its expected sample is not that click. Clicks
/// sit 960 samples apart, so this is the nearest-click rule with room for the
/// one-sample rounding at a cut's edge and nothing wider.
const MATCH: f64 = 4.0;

fn report(got: &[f64], want: &[f64]) -> Report {
    let mut r = Report::default();
    for w in want {
        let j = got.partition_point(|g| g < w);
        let near = [j.checked_sub(1), Some(j)]
            .into_iter()
            .flatten()
            .filter_map(|k| got.get(k).map(|g| g - w))
            .min_by(|a, b| a.abs().total_cmp(&b.abs()));
        match near {
            Some(d) if d.abs() <= MATCH => r.worst = r.worst.max(d.abs()),
            _ => r.missing += 1,
        }
    }
    r.extra = got
        .iter()
        .filter(|g| {
            let j = want.partition_point(|w| w < *g);
            [j.checked_sub(1), Some(j)]
                .into_iter()
                .flatten()
                .filter_map(|k| want.get(k))
                .all(|w| (**g - w).abs() > MATCH)
        })
        .count();
    r
}

/// Builds the timeline over click sources at `src_rates`, mixes it, reports.
fn measure(label: &str, shots: &[Shot], fps: f64, src_rates: &[u32]) -> Report {
    let sources: Vec<Scratch> = src_rates
        .iter()
        .enumerate()
        .map(|(i, &rate)| {
            let file = Scratch::file(&format!("av_sync_{label}_{i}"), "wav");
            write_clicks(&file, rate, 30);
            file
        })
        .collect();
    let paths: Vec<PathBuf> = sources.iter().map(|s| s.to_path_buf()).collect();
    let project = project_for(&paths, shots);
    let heard = mix(&project, fps);
    let want = expected(shots, fps, src_rates, heard.rate);
    let r = report(&heard.clicks, &want);
    eprintln!("{label}: {r:?}");
    r
}

/// Cuts that may lose or gain a sample at their edge: a click on the last
/// sample of one cut is heard as the first of the next, or the reverse.
fn edge_budget(shots: usize) -> usize {
    shots / 500
}

/// Runs one hour of cuts at every frame rate, returning what failed.
fn hour_at_each_rate(src_rates: &[u32], seed: u64) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, fps) in RATES {
        let total = (3600.0 * fps).round() as u32;
        let shots = shots(total, fps, 30.0, src_rates.len(), 45, seed);
        let r = measure(&format!("hour_{name}"), &shots, fps, src_rates);
        if r.missing + r.extra > edge_budget(shots.len()) {
            failures.push(format!(
                "{name}: {} clicks lost and {} invented over {} cuts",
                r.missing,
                r.extra,
                shots.len()
            ));
        }
        if r.worst > 1.0 {
            failures.push(format!("{name}: worst click {:.2} samples off", r.worst));
        }
    }
    failures
}

#[test]
fn an_hour_of_short_cuts_holds_the_picture_clock_at_every_frame_rate() {
    let failures = hour_at_each_rate(&[48_000], 1);
    assert!(failures.is_empty(), "48 kHz source: {failures:#?}");
}

#[test]
fn the_same_hour_from_a_44_1_khz_source_holds_too() {
    let failures = hour_at_each_rate(&[44_100], 2);
    assert!(failures.is_empty(), "44.1 kHz source: {failures:#?}");
}

#[test]
fn cuts_from_44_1_and_48_khz_sources_mixed_hold_within_resampling_error() {
    // The 44.1 kHz cuts are resampled onto the 48 kHz mix, so a click moves by
    // the resampler's own delay and shape: a couple of samples, not a frame.
    let mut failures = Vec::new();
    for (name, fps) in RATES {
        let total = (1200.0 * fps).round() as u32;
        let shots = shots(total, fps, 30.0, 2, 45, 3);
        let r = measure(&format!("mixed_{name}"), &shots, fps, &[44_100, 48_000]);
        if r.worst > 2.0 || r.missing + r.extra > edge_budget(shots.len()) {
            failures.push(format!("{name}: worst {:.2}, lost {}, invented {}", r.worst, r.missing, r.extra));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_voice_chain_keeps_every_click_on_its_sample() {
    // Noise reduction, compressor and de-esser on the lane: the noise reduction's STFT
    // delay is taken out again by the lane stage, so the clicks land where they did dry.
    // A profile far below the clicks keeps them intact, so only their position is tested.
    let mut fx = VoiceFx {
        nr: NoiseReduction { on: true, strength: 1.0, floor_db: -18.0, profile: vec![-200.0; voice::PROFILE_BANDS] },
        ..VoiceFx::default()
    };
    fx.comp.on = true;
    fx.deess.on = true;
    let source = Scratch::file("av_sync_voice", "wav");
    write_clicks(&source, 48_000, 30);
    let mut failures = Vec::new();
    for (name, fps) in RATES {
        let total = (120.0 * fps).round() as u32;
        let shots = shots(total, fps, 30.0, 1, 45, 4);
        let mut project = project_for(&[source.to_path_buf()], &shots);
        assert!(project.set_lane_voice(Lane::A1, fx.clone()), "the chain goes on");
        let heard = mix(&project, fps);
        let want = (total as f64 * f64::from(heard.rate) / fps).round() as u64;
        let r = report(&heard.clicks, &expected(&shots, fps, &[48_000], heard.rate));
        if r.worst > 1.0 || r.missing + r.extra > edge_budget(shots.len()) || heard.samples.abs_diff(want) > 1 {
            failures.push(format!("{name}: {r:?}, {} samples of {want}", heard.samples));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_mix_length_is_the_timeline_length_at_every_speed() {
    // Speeded cuts owe the timeline their own rounded frame counts; however
    // many speeds are mixed, the mix must be exactly the timeline's samples.
    let fps = 30000.0 / 1001.0;
    let source = Scratch::file("av_sync_speeds", "wav");
    write_clicks(&source, 48_000, 30);
    let mut shots = Vec::new();
    let mut at = 0;
    let speeds = [500u16, 1500, 2000, 1000, 750, 1250];
    for (i, &speed) in speeds.iter().cycle().take(60).enumerate() {
        let len = 20 + (i as u32 * 7) % 40;
        let shot = Shot {
            start: at,
            in_frame: 0,
            out_frame: len,
            source: 0,
            speed,
        };
        at += clip(shot).frames();
        shots.push(shot);
    }
    // Every cut reads the file from its top: the lengths are the point here.
    let project = project_for(&[source.to_path_buf()], &shots);
    let heard = mix(&project, fps);
    let want = (at as f64 * f64::from(heard.rate) / fps).round() as u64;
    assert!(
        heard.samples.abs_diff(want) <= 1,
        "{} samples for {at} timeline frames, want {want}",
        heard.samples
    );
}

#[test]
fn a_saved_and_reloaded_timeline_mixes_exactly_as_before() {
    let fps = 59.94_f64;
    let total = (600.0 * fps).round() as u32;
    let shots = shots(total, fps, 30.0, 1, 45, 4);
    let source = Scratch::file("av_sync_reload_src", "wav");
    write_clicks(&source, 44_100, 30);
    let project = project_for(&[source.to_path_buf()], &shots);
    let before = mix(&project, fps);

    let file = Scratch::file("av_sync_reload", "edith");
    edith::save(
        &file,
        project.sources(),
        &[(LaneKind::Audio, project.lane(Lane::A1).to_vec())],
        &[0.0],
        &[Vec::new()],
        &[],
        &[],
        &[],
        &[],
        (1280, 720),
        Some(fps),
        engine::tonemap::Preset::default(),
        false,
        false,
        engine::export::EncoderSeat::default(),
        project.limiter(),
        None,
        0,
    )
    .expect("the timeline saves");
    let doc = edith::load(&file).expect("the timeline reloads");
    let reloaded = Project::from_parts(doc.sources, doc.lanes, doc.eq, doc.color, doc.transform)
        .expect("the reloaded timeline");
    let loaded_fps = doc.fps.expect("the frame rate is saved");
    assert_eq!(loaded_fps, fps, "the saved frame rate reads back exact");

    let after = mix(&reloaded, loaded_fps);
    assert_eq!(after.samples, before.samples, "reload changed the length");
    let r = report(&after.clicks, &before.clicks);
    assert_eq!((r.missing, r.extra), (0, 0), "reload moved clicks: {r:?}");
    assert!(r.worst <= 1e-6, "reload moved a click by {} samples", r.worst);
}


// ---- the exported file, read back ----------------------------------------

/// The source's picture: white on each flash frame, black otherwise, at 160x90.
const FLASH_W: usize = 160;
const FLASH_H: usize = 90;
const FLASH_RATE: (&str, f64) = ("24000/1001", 24000.0 / 1001.0);
const AUDIO_RATE: u32 = 48_000;

/// The flash frames of a source of `secs` seconds: the first frame at or after
/// each whole second, which is also the instant its beep starts.
fn flash_frames(secs: u32, fps: f64) -> Vec<usize> {
    (1..secs)
        .map(|k| (f64::from(k) * fps).ceil() as usize)
        .collect()
}

/// Writes a `secs`-second source: a white flash frame and a 1 kHz beep at the
/// same instant every second, at a non-integer frame rate.
fn make_flash_source(dir: &Path, secs: u32) -> PathBuf {
    let fps = FLASH_RATE.1;
    let frames = (f64::from(secs) * fps).ceil() as usize;
    let flashes = flash_frames(secs, fps);
    let audio = dir.join("beeps.s16");
    let mut pcm = vec![0i16; secs as usize * AUDIO_RATE as usize];
    for &n in &flashes {
        let start = (n as f64 / fps * f64::from(AUDIO_RATE)).round() as usize;
        for i in 0..(AUDIO_RATE as usize / 20) {
            let t = i as f64 / f64::from(AUDIO_RATE);
            let env = (i as f64 / 48.0).min(1.0); // 1 ms attack, no click of its own
            let v = 0.5 * env * (2.0 * std::f64::consts::PI * 1000.0 * t).sin();
            if let Some(slot) = pcm.get_mut(start + i) {
                *slot = (v * 32767.0) as i16;
            }
        }
    }
    let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    std::fs::write(&audio, bytes).expect("write beeps");

    let out = dir.join("flash.mp4");
    let mut ff = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "gray"])
        .args(["-s", &format!("{FLASH_W}x{FLASH_H}"), "-r", FLASH_RATE.0, "-i", "-"])
        .args(["-f", "s16le", "-ar", "48000", "-ac", "1", "-i"])
        .arg(&audio)
        .args(["-map", "0:v", "-map", "1:a", "-t", &secs.to_string()])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "16", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", "192k"])
        .arg(&out)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("ffmpeg runs");
    {
        let stdin = ff.stdin.as_mut().expect("piped stdin");
        let flash_set: std::collections::HashSet<usize> = flashes.into_iter().collect();
        for n in 0..frames {
            let v = if flash_set.contains(&n) { 255u8 } else { 0u8 };
            std::io::Write::write_all(stdin, &vec![v; FLASH_W * FLASH_H]).expect("frame");
        }
    }
    assert!(ff.wait().expect("ffmpeg ends").success(), "the source encodes");
    out
}

/// What a decode of the exported file holds, read back through ffmpeg.
fn decode(file: &Path, dir: &Path) -> (Vec<u8>, Vec<f32>) {
    let video = dir.join("out.gray");
    let audio = dir.join("out.f32");
    for (args, out) in [
        (vec!["-an", "-f", "rawvideo", "-pix_fmt", "gray"], &video),
        (vec!["-vn", "-f", "f32le", "-ac", "1", "-ar", "48000"], &audio),
    ] {
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(file)
            .args(args)
            .arg(out)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success(), "ffmpeg decodes the export");
    }
    let picture = std::fs::read(&video).expect("decoded picture");
    let sound: Vec<f32> = std::fs::read(&audio)
        .expect("decoded sound")
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    (picture, sound)
}

/// A cut list of `frames` timeline frames over a `src_secs`-second source,
/// as one lane: the same cuts go on the picture lane and the sound lane.
fn export_cuts(frames: u32, fps: f64, src_secs: f64, seed: u64) -> Vec<Shot> {
    shots(frames, fps, src_secs, 1, 24, seed)
}

#[test]
fn a_ranged_export_after_many_cuts_keeps_each_flash_on_its_beep() {
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("no ffmpeg CLI: the export read-back is skipped");
        return;
    }
    let fps = FLASH_RATE.1;
    let dir = Scratch::dir("av_sync_export");
    let source = make_flash_source(&dir, 60);

    // Two minutes of cuts reading a minute of source: the range we export is
    // 15 s in the middle, with a few hundred cuts behind it.
    let cuts = export_cuts((120.0 * fps).round() as u32, fps, 60.0, 9);
    let video: Vec<Clip> = cuts.iter().map(|&s| clip(s)).collect();
    let audio = video.clone();
    let project_file = dir.join("timeline.edith");
    edith::save(
        &project_file,
        &[Source::new(source.clone(), 0)],
        &[(LaneKind::Video, video), (LaneKind::Audio, audio)],
        &[0.0, 0.0],
        &[Vec::new(), Vec::new()],
        &[],
        &[],
        &[],
        &[],
        (FLASH_W as u32, FLASH_H as u32),
        Some(fps),
        engine::tonemap::Preset::default(),
        false,
        false,
        engine::export::EncoderSeat::Software,
        engine::limiter::Limiter::default(),
        None,
        0,
    )
    .expect("the timeline saves");
    let session = engine::PlaybackSession::open_project(&project_file).expect("the timeline opens");

    let range = ((40.0 * fps).round() as u32, (55.0 * fps).round() as u32);
    let out = dir.join("export.mp4");
    let settings = engine::export::ExportSettings {
        format: engine::export::Format::Mp4,
        seat: engine::export::EncoderSeat::Software,
        range: Some(range),
        ..Default::default()
    };
    let handle = session.export_to_with(&out, &settings);
    let started = std::time::Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < std::time::Duration::from_secs(300), "the export hangs");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");

    let (picture, sound) = decode(&out, &dir);
    let frame = FLASH_W * FLASH_H;
    let flashes: Vec<f64> = (0..picture.len() / frame)
        .filter(|&i| {
            let mean = picture[i * frame..(i + 1) * frame].iter().map(|&v| u64::from(v)).sum::<u64>() as f64 / frame as f64;
            mean > 128.0
        })
        .filter(|&i| i == 0 || {
            let prev = &picture[(i - 1) * frame..i * frame];
            let mean = prev.iter().map(|&v| u64::from(v)).sum::<u64>() as f64 / frame as f64;
            mean <= 128.0
        })
        .map(|i| i as f64 / fps)
        .collect();

    assert!(flashes.len() >= 10, "only {} flashes in the 15 s export", flashes.len());
    // Each flash's beep: the first sample from 20 ms before the flash to 100 ms
    // after it that rises above 0.2. The beep is 0.5 full scale and its leading
    // edge sits within a millisecond of its start; the codec's pre-echo stays well
    // under 0.2, so the threshold finds the beep and not its noise.
    let mut worst = 0.0f64;
    for &t in &flashes {
        let from = ((t - 0.02) * f64::from(AUDIO_RATE)).max(0.0) as usize;
        let to = (((t + 0.1) * f64::from(AUDIO_RATE)) as usize).min(sound.len());
        let beep = (from..to)
            .find(|&i| sound[i].abs() > 0.2)
            .map(|i| i as f64 / f64::from(AUDIO_RATE))
            .unwrap_or_else(|| panic!("no beep within 100 ms of the flash at {t:.3} s"));
        let offset = (beep - t) * fps;
        worst = worst.max(offset.abs());
        assert!(offset.abs() < 1.0, "flash at {t:.3} s is {offset:.2} frames from its beep");
    }
    eprintln!(
        "export: {} flashes, worst offset {worst:.3} frames",
        flashes.len()
    );
}
