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

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mp4::{MediaType, Mp4Reader};

use engine::export::{ExportSettings, Format};
use engine::project::{Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
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
/// source no mp4 sample table holds — an mp3, which the mp4 path decodes and
/// re-encodes today and once refused outright.
fn mixed_project() -> Project {
    let clip = |start, source| Clip {
        start,
        in_frame: 0,
        out_frame: 30,
        source,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
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
                    color: None,
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                }],
            ),
            (LaneKind::Audio, vec![clip(0, 0), clip(60, 1)]),
        ],
        Vec::new(),
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

/// The third audio row, and the one that used to be a licence note: `rusty_mp3`
/// encodes it, and the file is read straight back through symphonia's mp3
/// decoder -- the very door an mp3 *import* comes in through, so an export can
/// be dropped back onto the timeline it came from.
///
/// The shape is checked loosely where MP3 cannot be exact: the encoder pads its
/// tail to a whole 1152-frame frame and the decoder hands back the padding, so
/// the length is asserted to within a frame instead of to the sample. What is
/// exact is the edit -- audible, silent, audible -- and the fixtures' 1 Hz
/// pulse inside the first second.
#[test]
fn exports_the_timeline_as_an_mp3() {
    let out = out_path("mp3", "mp3");
    let handle = engine::export::start(
        mixed_project(),
        meta(),
        &out,
        &ExportSettings {
            format: Format::Mp3,
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(120)).expect("mp3 export");
    assert_eq!(handle.progress(), 1.0, "finished at full progress");
    assert!(!part_path(&out).exists(), "the .part was renamed away");

    let (meta, samples) = decode(&out);
    assert_eq!((meta.sample_rate, meta.channels), (RATE, 2));
    let channels = usize::from(meta.channels);
    let frames = samples.len() / channels;
    let timeline = (3.0 * f64::from(RATE)) as usize;
    assert!(
        frames >= timeline && frames <= timeline + 2 * 1152,
        "{frames} frames for a three-second timeline ({timeline})"
    );
    let av = rms(&samples, channels, 0.05, 0.95);
    let gap = rms(&samples, channels, 1.1, 1.9);
    let tone = rms(&samples, channels, 2.05, 2.95);
    println!("mp3 rms: av {av:.4}  gap {gap:.6}  mp3 second {tone:.4}");
    assert!(av > 0.01, "the first second is audible");
    assert!(tone > 0.01, "the mp3 second is audible");
    assert!(gap < av / 20.0, "the hole is silence: {gap:.6}");
    let peak = rms(&samples, channels, 0.2, 0.3);
    let dip = rms(&samples, channels, 0.7, 0.8);
    assert!(
        peak > dip * 4.0,
        "the 1 Hz pulse survived: peak {peak:.4} vs dip {dip:.4}"
    );
    std::fs::remove_file(&out).unwrap();
}

/// The failure a user hit, as a test: a still picture and a song, exported as an
/// **mp4**. There is no AAC track anywhere in that timeline to copy -- an mp3
/// holds no such packets and a png holds nothing at all -- so this used to be
/// refused by name and the only way out was a WAV beside a picture nobody had.
/// The song is decoded and encoded as AAC instead (`export::copy_audio` falls
/// through to `encode_audio`), and the file comes back with picture *and* sound.
#[test]
fn a_still_and_a_song_export_as_an_mp4_with_sound() {
    let song = asset("test_tone.mp3");
    let still = asset("test_still.png");
    let clip = |source, frames| Clip {
        start: 0,
        in_frame: 0,
        out_frame: frames,
        source,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    // Two seconds of each at the project's own 30 fps: a still has no length of
    // its own and the song is longer than what is placed.
    let project = Project::from_parts(
        vec![Source::new(still, 0), Source::new(song, 0)],
        vec![
            (LaneKind::Video, vec![clip(0, 60)]),
            (LaneKind::Audio, vec![clip(1, 60)]),
        ],
        Vec::new(),
        Vec::new(),
    )
    .expect("a still under a song");
    // The project's own picture size and rate: the still decides the one and
    // the scaffold the other, exactly as opening a png in the editor does.
    let meta = engine::VideoMeta {
        width: 640,
        height: 360,
        frame_rate: 30.0,
        frame_count: 60,
        codec: engine::Codec::H264,
    };
    let out = out_path("still_song", "mp4");
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default());
    wait(&handle, Duration::from_secs(300)).expect("an mp4 of a still under a song");

    // Picture: two seconds of it, through the demuxer an import uses.
    let (written, _) = engine::demux::Demuxer::open(&out).expect("reopen the export");
    assert_eq!((written.width, written.height), (640, 360));
    assert_eq!(written.frame_count, 60, "two seconds at 30 fps");
    // ...and sound, which is the half that used to be missing.
    let (audio, samples) = decode(&out);
    let channels = usize::from(audio.channels);
    let heard = rms(&samples, channels, 0.2, 1.8);
    println!("still+song: {} Hz, rms {heard:.4}", audio.sample_rate);
    assert!(heard > 0.01, "the song is in the file (rms {heard:.4})");
    std::fs::remove_file(&out).unwrap();
}

/// Cancel leaves nothing behind -- not the output, not the `.part`. The
/// timeline is long enough (a minute of audio) that the escape lands mid-decode
/// rather than after the file is already closed.
/// What the two mix settings do to a *written file*, which is the only place
/// the claim can be checked: a track's fader is that track's alone, and the
/// master limiter holds the sum of them under its ceiling instead of letting it
/// square off at full scale.
///
/// Four audio tracks of the same second, so the sum is four times one file and
/// well past what a sample can hold. Measured as the peak of the WAV read back
/// through the engine's own reader -- a level, not a shape, because that is
/// exactly what a limiter promises.
#[test]
fn a_fader_is_one_tracks_own_and_the_limiter_holds_the_sum() {
    let clip = Clip {
        start: 0,
        in_frame: 0,
        out_frame: 30,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    let four = || {
        Project::from_parts(
            vec![Source::new(asset("test_av.mp4"), 0)],
            vec![
                (LaneKind::Video, vec![clip]),
                (LaneKind::Audio, vec![clip]),
                (LaneKind::Audio, vec![clip]),
                (LaneKind::Audio, vec![clip]),
                (LaneKind::Audio, vec![clip]),
            ],
            Vec::new(),
            Vec::new(),
        )
        .expect("one second on four audio tracks")
    };
    let write = |project: Project, name: &str| {
        let out = out_path(name, "wav");
        let handle = engine::export::start(
            project,
            meta(),
            &out,
            &ExportSettings {
                format: Format::Wav,
                ..Default::default()
            },
        );
        wait(&handle, Duration::from_secs(60)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (audio, samples) = decode(&out);
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let level = rms(&samples, usize::from(audio.channels), 0.05, 0.95);
        let _ = std::fs::remove_file(&out);
        (peak, level)
    };

    // Four copies of one file, each fader all the way up: the sum is well past
    // what a sample can hold, and with no limiter it is clamped at full scale
    // by the mixer's own backstop -- which is the clipping this exists for.
    let hot = || {
        let mut project = four();
        for ord in 0..4 {
            assert!(project.set_lane_gain_db(
                Lane::new(LaneKind::Audio, ord),
                engine::project::MAX_GAIN_DB
            ));
        }
        project
    };
    let (flat_peak, flat_level) = write(hot(), "mix_flat");
    println!("flat: peak {flat_peak:.4} rms {flat_level:.4}");

    // One track's fader, and one track's only: A2, A3 and A4 all the way down
    // leaves a quarter of the sum, so the level drops by ~12 dB against the
    // four of them at the same setting. A whole-band
    // change -- there is no frequency in this claim, which is what makes it a
    // different control from the equalizer.
    let (unity_peak, unity_level) = write(four(), "mix_unity");
    let mut down = four();
    for ord in 1..4 {
        assert!(down.set_lane_gain_db(
            Lane::new(LaneKind::Audio, ord),
            engine::project::MIN_GAIN_DB
        ));
    }
    let (_, quiet_level) = write(down, "mix_fader");
    let drop_db = 20.0 * (quiet_level / unity_level).log10();
    println!("one track alone: rms {quiet_level:.4} ({drop_db:.1} dB)");
    assert!(
        (drop_db + 12.0).abs() < 1.5,
        "three of four tracks down left {drop_db:.1} dB, not ~-12"
    );

    // The limiter over the same hot sum: the peak lands under the ceiling, and
    // the sound is still there (a limiter that silenced the mix would pass a
    // peak test and fail every ear).
    let ceiling_db = -3.0;
    let mut limited = hot();
    assert!(limited.set_limiter(engine::limiter::Limiter {
        ceiling_db,
        on: true,
    }));
    let (peak, level) = write(limited, "mix_limited");
    let ceiling = 10f32.powf(ceiling_db / 20.0);
    println!("limited: peak {peak:.4} vs ceiling {ceiling:.4} rms {level:.4}");
    assert!(
        flat_peak > ceiling,
        "the flat mix was not hot to begin with"
    );
    assert!(
        peak <= ceiling + 1e-3,
        "peak {peak:.4} passed the {ceiling:.4} ceiling"
    );
    assert!(level > unity_level, "the limiter took the mix out");
    assert!(
        unity_peak < 1.0,
        "four at unity fit; the +12 dB four did not"
    );
}

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

/// The rate an MP3 file *declares*, read out of its own frame headers -- the
/// only place the claim can be checked, since a caller's kbps is a request until
/// the bytes say so. MPEG-1 Layer III, whose bitrate index is the high nibble of
/// the third header byte.
fn mp3_declared_kbps(path: &Path) -> u32 {
    const KBPS: [u32; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    let bytes = std::fs::read(path).expect("read the mp3 back");
    let at = bytes
        .windows(4)
        .position(|w| w[0] == 0xFF && w[1] & 0xE0 == 0xE0)
        .expect("an MPEG frame sync");
    let header = &bytes[at..at + 4];
    assert_eq!(header[1] >> 3 & 0x3, 0b11, "MPEG-1");
    assert_eq!(header[1] >> 1 & 0x3, 0b01, "Layer III");
    let kbps = KBPS[usize::from(header[2] >> 4)];
    assert!(kbps > 0, "a free-format or invalid bitrate index");
    kbps
}

/// What an mp4's AAC track is really coded at: its sample bytes over the time
/// they play, in kbps. Every AAC-LC packet is 1024 frames per channel, so the
/// duration comes out of the packet count and needs no timescale of the file's
/// own -- and the bytes are what a player downloads, which is what a bitrate is.
fn mp4_aac_kbps(path: &Path, sample_rate: f64) -> f64 {
    let file = File::open(path).expect("reopen the mp4");
    let size = file.metadata().unwrap().len();
    let mut reader = Mp4Reader::read_header(BufReader::new(file), size).expect("mp4 header");
    let track = reader
        .tracks()
        .values()
        .find(|t| matches!(t.media_type(), Ok(MediaType::AAC)))
        .expect("an AAC track")
        .track_id();
    let count = reader.sample_count(track).expect("sample count");
    let mut bytes = 0usize;
    for id in 1..=count {
        bytes += reader
            .read_sample(track, id)
            .expect("read a sample")
            .expect("a sample at every id")
            .bytes
            .len();
    }
    let seconds = f64::from(count) * 1024.0 / sample_rate;
    bytes as f64 * 8.0 / seconds / 1000.0
}

/// The sound's rate is the caller's, in both kinds of file -- and the file
/// nobody asked a rate of is the one this program always wrote.
///
/// MP3 declares its rate in every frame header, so that half is exact. The AAC
/// half is measured off the mp4's own sample table: a rate control lands *near*
/// its target rather than on it, so the tolerance is a fifth, which is far
/// tighter than the gap between two offered rates.
#[test]
fn the_sound_is_written_at_the_rate_that_was_asked_for() {
    // Untouched settings first: this is the byte-behaviour a user who never
    // opens the row is entitled to keep.
    for (kbps, want) in [
        (None, engine::export::DEFAULT_AUDIO_KBPS),
        (Some(128), 128),
        (Some(320), 320),
    ] {
        let out = out_path(&format!("mp3_{}", want), "mp3");
        let handle = engine::export::start(
            mixed_project(),
            meta(),
            &out,
            &ExportSettings {
                format: Format::Mp3,
                audio_kbps: kbps,
                ..Default::default()
            },
        );
        wait(&handle, Duration::from_secs(120)).expect("mp3 export");
        let got = mp3_declared_kbps(&out);
        println!("mp3 asked {kbps:?}, header says {got} kbps");
        assert_eq!(got, want, "the mp3 frames declare the rate that was picked");
        // ...and it is still the edit, not just bytes at the right rate.
        let (meta, samples) = decode(&out);
        let channels = usize::from(meta.channels);
        assert!(rms(&samples, channels, 0.05, 0.95) > 0.01, "audible");
        std::fs::remove_file(&out).unwrap();
    }

    // The same choice inside a video export, where the sound is AAC: a song
    // under a still, which no mp4 sample table can be copied from, so the
    // encoder really runs.
    let project = Project::from_parts(
        vec![
            Source::new(asset("test_still.png"), 0),
            Source::new(asset("test_tone.mp3"), 0),
        ],
        vec![
            (
                LaneKind::Video,
                vec![Clip {
                    start: 0,
                    in_frame: 0,
                    out_frame: 60,
                    source: 0,
                    link: None,
                    eq: None,
                    color: None,
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                }],
            ),
            (
                LaneKind::Audio,
                vec![Clip {
                    start: 0,
                    in_frame: 0,
                    out_frame: 60,
                    source: 1,
                    link: None,
                    eq: None,
                    color: None,
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                }],
            ),
        ],
        Vec::new(),
        Vec::new(),
    )
    .expect("a still under a song");
    let meta = engine::VideoMeta {
        width: 640,
        height: 360,
        frame_rate: 30.0,
        frame_count: 60,
        codec: engine::Codec::H264,
    };
    for want in [128u32, 320] {
        let out = out_path(&format!("mp4_aac_{want}"), "mp4");
        let handle = engine::export::start(
            project.clone(),
            meta,
            &out,
            &ExportSettings {
                audio_kbps: Some(want),
                ..Default::default()
            },
        );
        wait(&handle, Duration::from_secs(300)).expect("an mp4 of a still under a song");
        let got = mp4_aac_kbps(&out, f64::from(RATE));
        println!("mp4 asked {want} kbps, the track measures {got:.1}");
        assert!(
            (got - f64::from(want)).abs() < f64::from(want) / 5.0,
            "{got:.1} kbps for a {want} kbps request"
        );
        std::fs::remove_file(&out).unwrap();
    }
}

/// The samples a silent take contributes: a clip of a file with no audio track
/// is silence for exactly its span, between two clips that are heard. This is
/// what makes `PlaybackSession::import` letting such a file in honest -- the
/// export reads the same play list playback does, so a hole here is a hole in
/// the ear too.
#[test]
fn a_silent_clip_exports_as_silence_over_its_own_span() {
    let clip = |start, source| Clip {
        start,
        in_frame: 0,
        out_frame: 30,
        source,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    let project = Project::from_parts(
        vec![
            Source::new(asset("test_av.mp4"), 0),
            // Video only, no audio track at all: the silent take.
            Source::new(asset("test_mismatch.mp4"), 0),
        ],
        vec![
            (LaneKind::Video, vec![clip(0, 0), clip(30, 1), clip(60, 0)]),
            (LaneKind::Audio, vec![clip(0, 0), clip(30, 1), clip(60, 0)]),
        ],
        Vec::new(),
        Vec::new(),
    )
    .expect("a project whose middle take is silent");

    let out = out_path("silent_clip", "wav");
    let handle = engine::export::start(
        project,
        meta(),
        &out,
        &ExportSettings {
            format: Format::Wav,
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(60)).expect("wav export");
    let (audio, samples) = decode(&out);
    let channels = usize::from(audio.channels);
    assert_eq!(
        samples.len(),
        (3.0 * f64::from(RATE)) as usize * channels,
        "three seconds of timeline, silent take included"
    );
    let heard = rms(&samples, channels, 0.05, 0.95);
    let quiet = rms(&samples, channels, 1.0, 2.0);
    let again = rms(&samples, channels, 2.05, 2.95);
    println!("rms: heard {heard:.4}  silent take {quiet:.6}  heard {again:.4}");
    assert!(heard > 0.01, "the first take is audible");
    assert!(again > 0.01, "the third take is audible");
    assert_eq!(quiet, 0.0, "the silent take is exact silence, not a stall");
    std::fs::remove_file(&out).unwrap();
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

/// An AC-3 source through the audio-only door: the mp4 export cannot
/// copy a syncframe into an `mp4a` track, but a WAV of the same timeline
/// decodes, downmixes and reopens through the engine's own reader.
#[test]
fn a_51_ac3_source_round_trips_through_a_wav() {
    let source = asset("test_ac3_51.mp4");
    let project = Project::from_parts(
        vec![Source::new(source.clone(), 0)],
        vec![(
            LaneKind::Audio,
            vec![Clip {
                start: 0,
                in_frame: 0,
                out_frame: 30,
                source: 0,
                link: None,
                eq: None,
                color: None,
                fit: FitPolicy::default(),
                speed: Speed::NORMAL,
            }],
        )],
        Vec::new(),
        Vec::new(),
    )
    .expect("a one-source AC-3 project");
    let video = engine::demux::Demuxer::open(&source).expect("open the fixture").0;

    let out = out_path("ac3", "wav");
    let handle = engine::export::start(
        project,
        video,
        &out,
        &ExportSettings {
            format: Format::Wav,
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(60)).expect("wav export of an AC-3 source");

    let (meta, samples) = decode(&out);
    assert_eq!(
        (meta.sample_rate, meta.channels),
        (48_000, 2),
        "the §7.8 downmix is what reaches the file"
    );
    let secs = (samples.len() / 2) as f64 / 48_000.;
    assert!((0.9..1.1).contains(&secs), "one second of clip, got {secs:.3}s");
    let energy = (samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>()
        / samples.len() as f64)
        .sqrt();
    assert!(energy > 0.005, "the exported AC-3 is silence: RMS {energy:.6}");
    std::fs::remove_file(&out).unwrap();
}
