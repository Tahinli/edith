//! The timeline's audio written out on its own, as a WAV or a FLAC, and read
//! straight back in through the engine's own reader — which is the check that
//! matters: a file this program cannot reopen is a file it had no business
//! writing.
//!
//! The fixtures carry a 1 Hz volume pulse (`gen_fixtures.sh`), so a second of
//! exported audio has a measurable shape and not just a level: the pattern is
//! asserted as a peak/dip *ratio*, never an absolute level, because ffmpeg's
//! sine fixtures peak around 0.12.
//!
//! No picture is decoded anywhere here, so nothing needs the plugin and the
//! whole file runs on any machine.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::LaneKind;
use engine::project::Source;
use engine::{AudioSession, Clip, ExportHandle, Project};

const RATE: u32 = 44_100;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Per-run unique, like the mp4 suite's: two suites at once must not delete
/// each other's output.
fn out_path(name: &str, ext: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ve_audio_{name}_{}.{ext}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(part_path(&path));
    path
}

fn part_path(out: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", out.display()))
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

/// One second of `test_av`, one second of hole, one second of `test_tone.mp3`.
///
/// Three things at once: a cut, a gap (which must arrive as silence), and a
/// source the mp4 export flatly refuses — there is no AAC encoder here, so an
/// mp3 on the timeline can *only* leave through this door.
fn mixed_project() -> Project {
    let clip = |start, source| Clip {
        start,
        in_frame: 0,
        out_frame: 30,
        source,
        link: None,
        eq: None,
    };
    Project::from_parts(
        vec![
            Source::new(asset("test_av.mp4"), 0),
            Source::new(asset("test_tone.mp3"), 0),
        ],
        // The picture is beside the point here, but a project has two lanes and
        // the timeline's length is the longer of them: three seconds of video
        // is what makes the third second of audio part of the timeline.
        vec![
            (
                LaneKind::Video,
                vec![Clip {
                    start: 0,
                    in_frame: 0,
                    out_frame: 90,
                    source: 0,
                    link: None,
                    eq: None,
                }],
            ),
            (LaneKind::Audio, vec![clip(0, 0), clip(60, 1)]),
        ],
        Vec::new(),
    )
    .expect("a two-source project with a hole in the audio lane")
}

fn meta() -> engine::VideoMeta {
    engine::demux::Demuxer::open(&asset("test_av.mp4"))
        .expect("open test_av")
        .0
}

/// Interleaved samples of a written file, through the engine's own reader --
/// the same door an import comes in through, so a pass here means the export
/// can be brought back into a timeline.
fn decode(path: &Path) -> (engine::AudioMeta, Vec<f32>) {
    let (meta, chunks) = AudioSession::open(path)
        .expect("reopen the export")
        .expect("the export has audio");
    let mut samples = Vec::new();
    for chunk in chunks {
        samples.extend(chunk.samples);
    }
    (meta, samples)
}

/// Root mean square of `[from, to)` seconds of an interleaved stereo buffer.
fn rms(samples: &[f32], channels: usize, from: f64, to: f64) -> f64 {
    let at = |secs: f64| (secs * f64::from(RATE)) as usize * channels;
    let window = &samples[at(from).min(samples.len())..at(to).min(samples.len())];
    assert!(!window.is_empty(), "no samples in [{from}, {to})");
    (window
        .iter()
        .map(|s| f64::from(*s) * f64::from(*s))
        .sum::<f64>()
        / window.len() as f64)
        .sqrt()
}

/// What every audio export must be, whatever encoded it: exactly as long as the
/// timeline, silent where the timeline is empty, and audible where it is not --
/// with the fixtures' 1 Hz pulse still in it, which is what says the samples are
/// the *edit's* and not just noise of the right length.
fn assert_timeline_shape(meta: &engine::AudioMeta, samples: &[f32]) {
    assert_eq!((meta.sample_rate, meta.channels), (RATE, 2), "as decoded");
    let channels = usize::from(meta.channels);
    // Three seconds of timeline, to the sample: the segments round their own
    // windows and the mp3 need not end where the second does, so this is the
    // export's own trim/pad and not an accident of the sources.
    let frames = (3.0 * f64::from(RATE)) as usize;
    assert_eq!(
        samples.len(),
        frames * channels,
        "timeline length in samples"
    );

    // Second 0 is test_av, second 2 is the mp3; both carry the tone.
    let av = rms(samples, channels, 0.05, 0.95);
    let tone = rms(samples, channels, 2.05, 2.95);
    // Second 1 is the hole. Silence, not a stall and not a skip: it occupies
    // its full second, which is what the sample count above already proved.
    let gap = rms(samples, channels, 1.0, 2.0);
    println!("rms: av {av:.4}  gap {gap:.6}  mp3 {tone:.4}");
    assert!(av > 0.01, "the first second is audible");
    assert!(tone > 0.01, "the mp3 second is audible");
    assert_eq!(gap, 0.0, "the gap is exact silence");

    // The 1 Hz pulse: `volume = 0.5 + 0.5 sin(2 pi t)`, so a peak sits at
    // t = 0.25 and the dip at t = 0.75. Ratio, never an absolute level.
    let peak = rms(samples, channels, 0.2, 0.3);
    let dip = rms(samples, channels, 0.7, 0.8);
    println!(
        "pulse: peak {peak:.4}  dip {dip:.4}  ratio {:.1}",
        peak / dip
    );
    assert!(
        peak > dip * 4.0,
        "the 1 Hz pulse survived: peak {peak:.4} vs dip {dip:.4}"
    );
}

/// The RIFF header itself, byte for byte: hound writes it, but what a player
/// reads is the bytes, and a wrong rate or a wrong `data` size is a file that
/// decodes at the wrong speed rather than one that fails to open.
fn assert_wav_header(path: &Path, frames: usize) {
    let bytes = std::fs::read(path).expect("read the wav back");
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    assert_eq!(u32_at(4) as usize, bytes.len() - 8, "RIFF size");
    assert_eq!(u16_at(20), 1, "PCM, not a float or a compressed tag");
    assert_eq!(u16_at(22), 2, "stereo");
    assert_eq!(u32_at(24), RATE, "sample rate");
    assert_eq!(u32_at(28), RATE * 2 * 2, "byte rate = rate * channels * 2");
    assert_eq!(u16_at(32), 4, "block align");
    assert_eq!(u16_at(34), 16, "bits per sample");
    // The `data` chunk carries every sample and nothing else. hound may write a
    // `fact` chunk first, so it is found rather than assumed to be at 36.
    let data = bytes
        .windows(4)
        .position(|w| w == b"data")
        .expect("a data chunk");
    assert_eq!(u32_at(data + 4) as usize, frames * 2 * 2, "data chunk size");
}

#[test]
fn exports_the_timeline_as_a_wav() {
    let out = out_path("wav", "wav");
    let handle = engine::export::start(
        mixed_project(),
        meta(),
        &out,
        &ExportSettings {
            format: Format::Wav,
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(60)).expect("wav export");
    assert_eq!(handle.progress(), 1.0, "finished at full progress");
    assert!(!part_path(&out).exists(), "the .part was renamed away");

    let (meta, samples) = decode(&out);
    assert_timeline_shape(&meta, &samples);
    assert_wav_header(&out, samples.len() / usize::from(meta.channels));
    std::fs::remove_file(&out).unwrap();
}

#[test]
fn exports_the_timeline_as_a_flac() {
    let out = out_path("flac", "flac");
    let handle = engine::export::start(
        mixed_project(),
        meta(),
        &out,
        &ExportSettings {
            format: Format::Flac,
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(60)).expect("flac export");
    assert_eq!(handle.progress(), 1.0, "finished at full progress");

    let bytes = std::fs::read(&out).expect("read the flac back");
    assert_eq!(&bytes[0..4], b"fLaC", "a FLAC stream, by its own magic");
    // Lossless is a claim about the samples, so it is checked on the samples:
    // the same shape the WAV of the same timeline has.
    let (meta, samples) = decode(&out);
    assert_timeline_shape(&meta, &samples);
    // Smaller than the PCM it encodes, or the whole point of the row is gone.
    let pcm = samples.len() * 2;
    println!("flac {} bytes for {pcm} bytes of PCM", bytes.len());
    assert!(bytes.len() < pcm, "compressed");
    std::fs::remove_file(&out).unwrap();
}

/// Cancel leaves nothing behind -- not the output, not the `.part`. The
/// timeline is long enough (a minute of audio) that the escape lands mid-decode
/// rather than after the file is already closed.
#[test]
fn a_cancelled_audio_export_leaves_no_file() {
    let source = asset("test_av.mp4");
    let (meta, _) = engine::demux::Demuxer::open(&source).expect("open test_av");
    let mut project = Project::single(&source, meta.frame_count);
    for _ in 0..11 {
        assert!(project.append_clip(0, meta.frame_count), "a minute of it");
    }
    let out = out_path("cancel", "wav");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Wav,
            ..Default::default()
        },
    );
    handle.cancel();
    let outcome = wait(&handle, Duration::from_secs(120));
    let text = outcome.expect_err("a cancelled export fails").to_string();
    assert!(text.contains("cancelled"), "{text}");
    assert!(!out.exists(), "no output file");
    assert!(!part_path(&out).exists(), "no half-written .part either");
}

/// A silent timeline cannot become an audio file, and says so rather than
/// writing zero samples that a user would take for a broken microphone.
#[test]
fn a_silent_timeline_refuses_an_audio_export() {
    let source = asset("test_mismatch.mp4"); // video only, no audio track
    let (meta, _) = engine::demux::Demuxer::open(&source).expect("open the fixture");
    let out = out_path("silent", "wav");
    let handle = engine::export::start(
        Project::single(&source, meta.frame_count),
        meta,
        &out,
        &ExportSettings {
            format: Format::Wav,
            ..Default::default()
        },
    );
    let text = wait(&handle, Duration::from_secs(60))
        .expect_err("no audio to export")
        .to_string();
    assert!(text.contains("no audio"), "{text}");
    assert!(
        !out.exists() && !part_path(&out).exists(),
        "nothing written"
    );
}
