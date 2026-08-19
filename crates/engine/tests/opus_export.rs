//! Opus on the way *out*: an edited Opus source leaves a Matroska export as
//! Opus instead of being turned into AAC, and comes back in through this
//! project's own door -- symphonia's mkv reader for the container, `ruopus` for
//! the packets, which is the pair a film already plays through.
//!
//! Three claims, and each is measured rather than asserted from the code:
//!
//! * the file really carries an `A_OPUS` track with an `OpusHead` in its
//!   `CodecPrivate`, and what comes back out of it correlates with the mix that
//!   went in -- sample-aligned, so the pre-skip in that header is the encoder's
//!   real lookahead and not a number somebody hoped for;
//! * a timeline nobody has touched still *copies* its AAC packets rather than
//!   taking the new encoder: Opus is what an edit costs, never what a passthrough
//!   costs;
//! * and the envelope itself -- 48 kHz, stereo, at most
//!   `OPUS_MAX_KBPS` -- is a measurement of `opus-rs 0.1.26` and is pinned here,
//!   so the day the crate is bumped past its high-rate bug this suite says so.
//!
//! Nothing here needs a GPU: the picture is 320x180 and HEVC's intra encoder is
//! software by construction, so the whole file runs anywhere. Add
//! `EDITH_OPUS_FILM=<path to a real-library Opus mkv>` to run the film case too
//! -- it is skipped without it, since no film lives in this repo.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::{LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::{AudioSession, Clip, ExportHandle, Project};

/// The picture: 320x180 at 24 fps, 30 s (`scripts/gen_fixtures.sh`). Its own
/// sound is mono Opus and is deliberately left off the audio lane -- see
/// [`a_mono_mix_keeps_the_aac_path`].
const VIDEO: &str = "test_seek_chirp.mkv";
/// The sound: 5.1 Opus, 440 Hz in FL and 880 Hz in BR, folded to stereo on the
/// way to the timeline -- so the mix this export encodes *is* an Opus source's,
/// at the only rate Opus has.
const OPUS: &str = "test_opus_51.mka";
/// One Opus frame, in samples per channel: what the tail of an export may be
/// padded by.
const OPUS_FRAME_SAMPLES: usize = 960;
/// The AAC source whose packets a copy must still win with.
const AAC: &str = "test_av.mp4";

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn out_path(name: &str) -> Scratch {
    Scratch::file(&format!("ve_opus_{name}"), "mkv")
}

fn pin_software() {
    static PIN: Once = Once::new();
    PIN.call_once(|| unsafe {
        std::env::set_var("VE_SW", "1");
        std::env::set_var("VE_SW_ENC", "1");
    });
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

/// One second of picture with one second of `sound` under it, cut from
/// `sound_in` seconds into that file: an edit, so nothing here can be a copy.
fn project(video: &Path, sound: &Path, sound_in: u32) -> Project {
    let clip = |source, in_frame, out_frame| Clip {
        fade_in: 0,
        fade_out: 0,
        start: 0,
        in_frame,
        out_frame,
        source,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    // The sound is source **0** on purpose: the timeline takes its rate and its
    // width from the first source that has any, so a project whose first source
    // is a mono-track video file is a mono timeline whatever plays on its audio
    // lane -- which is the engine's rule, not this test's, and the one that
    // decides whether Opus is reachable at all.
    Project::from_parts(
        vec![Source::new(sound, 0), Source::new(video, 0)],
        vec![
            (LaneKind::Video, vec![clip(1, 0, 24)]),
            (
                LaneKind::Audio,
                vec![clip(0, sound_in * 24, sound_in * 24 + 24)],
            ),
        ],
        Vec::new(),
        Vec::new(),
    )
    .expect("a picture lane and an audio lane")
}

fn export(name: &str, project: Project, video: &Path) -> Scratch {
    pin_software();
    let out = out_path(name);
    let (meta, _) = engine::demux::Demuxer::open(video).expect("probe the picture");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Hevc,
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(600)).expect("the export finishes");
    out
}

/// Interleaved samples of a written file through the engine's own reader: the
/// door an import comes in through, and for an Opus track that is symphonia's
/// mkv reader handing `CodecPrivate` and blocks to `ruopus`.
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

/// The best normalised correlation of `got` against `want` over the first
/// `lags` samples of offset, and the offset that reached it: a lossy codec
/// changes the samples, so what says "this is the same sound in the same place"
/// is the shape, and where the shape sits.
fn align(want: &[f32], got: &[f32], lags: usize) -> (f64, usize) {
    let window = 12_000.min(want.len()).min(got.len().saturating_sub(lags));
    assert!(window > 1000, "not enough sound to correlate");
    let (mut best, mut at) = (-2.0, 0);
    for lag in 0..lags {
        let c = corr(&want[..window], &got[lag..lag + window]);
        if c > best {
            best = c;
            at = lag;
        }
    }
    (best, at)
}

fn corr(a: &[f32], b: &[f32]) -> f64 {
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        num += f64::from(*x) * f64::from(*y);
        da += f64::from(*x) * f64::from(*x);
        db += f64::from(*y) * f64::from(*y);
    }
    num / (da.sqrt() * db.sqrt()).max(1e-12)
}

/// One channel of an interleaved buffer.
fn chan(samples: &[f32], channels: usize, which: usize) -> Vec<f32> {
    samples
        .iter()
        .skip(which)
        .step_by(channels)
        .copied()
        .collect()
}

/// The claim: a cut of an Opus source, exported as a Matroska file, carries an
/// Opus track -- and that track is this project's own mix, in the right place.
#[test]
fn an_edited_opus_source_leaves_a_matroska_export_as_opus() {
    let out = export(
        "cut",
        project(&asset(VIDEO), &asset(OPUS), 1),
        &asset(VIDEO),
    );

    // The container, before any decoder: `A_OPUS` with an `OpusHead` in its
    // `CodecPrivate`, and no AAC track anywhere near it.
    let bytes = std::fs::read(&*out).expect("read the export back");
    let head = find(&bytes, b"OpusHead").expect("an OpusHead in the CodecPrivate");
    assert!(find(&bytes, b"A_OPUS").is_some(), "the track says A_OPUS");
    assert!(find(&bytes, b"A_AAC").is_none(), "and nothing says A_AAC");
    assert_eq!(bytes[head + 9], 2, "OpusHead: stereo");
    // The pre-skip: the encoder's lookahead plus the warm-up frame it throws
    // away, which is what `export::tests` measures the sound starting at.
    assert_eq!(
        u16::from_le_bytes([bytes[head + 10], bytes[head + 11]]),
        1080,
        "OpusHead: pre-skip"
    );
    assert_eq!(
        u32::from_le_bytes(bytes[head + 12..head + 16].try_into().unwrap()),
        48_000,
        "OpusHead: 48 kHz"
    );
    assert_eq!(bytes[head + 18], 0, "OpusHead: mapping family 0");

    // ...and what a decoder makes of it, against the very second of the source
    // the timeline cut out. The reader drops the pre-skip off the front, so a
    // correct header lands the sound at zero: any leftover offset here is
    // exactly the codec delay this file failed to declare.
    let (meta, got) = decode(&out);
    assert_eq!(meta.sample_rate, 48_000, "Opus reads back at 48 kHz");
    assert_eq!(meta.channels, 2);
    let (_, want) = {
        let (m, chunks) = AudioSession::open_segments(asset(OPUS), &[(1.0, 2.0)])
            .expect("open the source")
            .expect("the source has sound");
        let mut s = Vec::new();
        for chunk in chunks {
            s.extend(chunk.samples);
        }
        (m, s)
    };
    // The fixture's two channels are *pure* sines, so a lag search on them is
    // ambiguous by whole periods (440 Hz is 109.09 samples) and cannot be the
    // alignment check -- that one is unambiguous in `export::tests`, on a signal
    // with no period, and lands on the declared pre-skip exactly. What is
    // checked here is the pair a periodic signal *can* answer: the sound is the
    // source's, and it is in the right place to within a sample at lag zero.
    assert!(
        got.len() >= want.len() - 2 && got.len() <= want.len() + OPUS_FRAME_SAMPLES * 2,
        "the track is {} samples where the timeline is {}",
        got.len(),
        want.len()
    );
    for (which, name) in [(0, "left"), (1, "right")] {
        let (w, g) = (chan(&want, 2, which), chan(&got, 2, which));
        let span = w.len().min(g.len());
        let aligned = corr(&w[..span], &g[..span]);
        assert!(
            aligned >= 0.98,
            "{name}: correlation {aligned:.4} against the source at lag zero -- \
             the export is either not this sound or not in this place"
        );
        // Past the first frame, where the encoder's cold start is behind it:
        // 0.997 left and 0.993 right, measured, against 0.9948/0.9825 with the
        // head in. That gap *is* the ramp the pre-skip cannot cover, and it is
        // asserted from both sides so neither can drift unnoticed.
        let settled = corr(&w[OPUS_FRAME_SAMPLES..span], &g[OPUS_FRAME_SAMPLES..span]);
        assert!(
            settled >= 0.99,
            "{name}: correlation {settled:.4} past the first frame"
        );
        // ...and the head specifically, which is what the pre-skip and the
        // warm-up frame are for. Measured at 128 kbps: 0.990 left, 0.953 right,
        // where the same export with no warm-up frame scored 0.9788 left and a
        // mis-declared pre-skip would score far less than either. The gap to the
        // 0.99 above is one frame of cold start -- `opus-rs 0.1.26` opens about
        // 6 dB down whatever is fed in ahead of it (two warm-up frames change
        // this in no decimal place) -- so this threshold is that ramp stated
        // rather than a shrug.
        let head = corr(&w[..12_000], &g[..12_000]);
        assert!(head >= 0.94, "{name}: the first 250 ms correlate {head:.4}");
    }

    // A second implementation on the same bytes, where the box has one: ffmpeg
    // decoding what we wrote is the check that the file is Opus by the format's
    // rules and not merely by ours -- pre-skip included, since ffmpeg drops it
    // off the front exactly as this project's reader does.
    if let Some(pcm) = ffmpeg_decode(&out) {
        let (w, g) = (chan(&want, 2, 0), chan(&pcm, 2, 0));
        let span = w.len().min(g.len());
        assert!(
            span + 4 >= w.len(),
            "ffmpeg read {span} samples of a {} sample track",
            w.len()
        );
        let c = corr(&w[..span], &g[..span]);
        assert!(
            c >= 0.99,
            "ffmpeg's decode correlates {c:.4} with the source"
        );
    }
}

/// The other half of the seat: an untouched AAC timeline still *copies*, even
/// into the container that would otherwise encode Opus. A copy is bit-exact and
/// an encode is a generation of loss, so the new encoder must never be reached
/// where the old passthrough still applies.
#[test]
fn an_untouched_timeline_still_copies_its_aac_into_an_mkv() {
    let source = asset(AAC);
    let out = export("copy", project(&source, &source, 0), &source);
    let bytes = std::fs::read(&*out).expect("read the export back");
    assert!(find(&bytes, b"A_AAC").is_some(), "the copy stays AAC");
    assert!(
        find(&bytes, b"A_OPUS").is_none(),
        "an untouched lane took the Opus encoder instead of copying"
    );
}

/// A mono mix keeps AAC, and that is a *measurement* and not a preference:
/// `opus-rs 0.1.26` mis-encodes mono at rates it is asked for here (see the
/// envelope test in `export.rs`), so the file that would play back wrong is
/// never written. `test_seek_chirp.mkv` carries mono Opus, which is exactly the
/// source that would tempt this path.
#[test]
fn a_mono_mix_keeps_the_aac_path() {
    let source = asset(VIDEO);
    let out = export("mono", project(&source, &source, 1), &source);
    let bytes = std::fs::read(&*out).expect("read the export back");
    assert!(
        find(&bytes, b"A_AAC").is_some(),
        "a mono mix is written as AAC"
    );
    assert!(find(&bytes, b"A_OPUS").is_none());
}

/// His own library, not a fixture: a real Opus film, cut, exported, read back.
/// The path comes from the environment because no film's name belongs in this
/// repository; without it the test says so and passes.
#[test]
fn a_real_library_opus_film_exports_as_opus() {
    let Some(film) = std::env::var_os("EDITH_OPUS_FILM").map(PathBuf::from) else {
        eprintln!("EDITH_OPUS_FILM unset: the film case is skipped");
        return;
    };
    // The fixture's picture under the film's *sound*: what is on trial here is
    // an Opus track off his own library -- 5.1, folded to stereo on the way in
    // -- and decoding a 1080p AV1 film would drag a VA-API plugin into a test
    // about audio.
    let out = export("film", project(&asset(VIDEO), &film, 600), &asset(VIDEO));
    let bytes = std::fs::read(&*out).expect("read the export back");
    assert!(
        find(&bytes, b"A_OPUS").is_some(),
        "the film's sound stays Opus"
    );
    let (meta, got) = decode(&out);
    assert_eq!((meta.sample_rate, meta.channels), (48_000, 2));
    let (_, want) = {
        let (_, chunks) = AudioSession::open_segments(&film, &[(600.0, 601.0)])
            .expect("open the film")
            .expect("the film has sound");
        let mut s = Vec::new();
        for chunk in chunks {
            s.extend(chunk.samples);
        }
        ((), s)
    };
    // Film sound is broadband and has no period, so unlike the tone fixture the
    // lag here is unambiguous: the peak is where the sound is.
    let (c, lag) = align(&chan(&want, 2, 0), &chan(&got, 2, 0), 400);
    // What ffprobe makes of the same file, printed rather than parsed -- run
    // with `--nocapture` and this is the line that says `Audio: opus`.
    if let Ok(probe) = Command::new("ffprobe")
        .args(["-hide_banner"])
        .arg(&*out)
        .output()
    {
        eprint!("{}", String::from_utf8_lossy(&probe.stderr));
    }
    eprintln!("the film's span: correlation {c:.4} at lag {lag}");
    assert!(c >= 0.99, "the film's span correlates {c:.4} at lag {lag}");
    assert!(lag <= 2, "the film's span starts {lag} samples late");
}

/// Where `needle` first sits in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// ffmpeg's own decode of a file, interleaved f32 at 48 kHz stereo. `None`
/// where the box has no ffmpeg -- the test is about the file, not about this
/// machine's packages.
fn ffmpeg_decode(path: &Path) -> Option<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-f", "f32le", "-ar", "48000", "-ac", "2", "-",
        ])
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "ffmpeg refused the exported Opus track: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        out.stdout
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// The one line the card shows while an export runs, where the picture is
/// copied and the sound is not. Two lanes of work meet in that sentence -- the
/// video copy path writes it, the Opus seat decides its second half -- and the
/// half that matters is *measured*: the copy branch used to publish the
/// prediction the card already shows, which would have named Opus on an export
/// that quietly fell back to AAC.
///
/// The picture is `test_hevc.mkv` untouched, so it is copied block for block;
/// the sound is the 5.1 Opus source folded to the stereo mix Opus is reachable
/// at, so it is encoded. Nothing in the file is decoded to prove this -- the
/// container is read back for the track ids, which is what the two paths
/// actually wrote.
#[test]
fn a_copied_picture_names_the_sound_it_was_written_with() {
    pin_software();
    let video = asset("test_hevc.mkv");
    let sound = asset(OPUS);
    let (meta, _) = engine::demux::Demuxer::open(&video).expect("probe the picture");
    let frames = meta.frame_count;
    let clip = |source, out_frame| Clip {
        fade_in: 0,
        fade_out: 0,
        start: 0,
        in_frame: 0,
        out_frame,
        source,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    // The sound is source 0, for the reason [`project`] gives: the timeline
    // takes its rate and its width from the first source that has any.
    let project = Project::from_parts(
        vec![Source::new(&sound, 0), Source::new(&video, 0)],
        vec![
            (LaneKind::Video, vec![clip(1, frames)]),
            (LaneKind::Audio, vec![clip(0, frames)]),
        ],
        Vec::new(),
        Vec::new(),
    )
    .expect("a picture lane and an audio lane");
    let out = out_path("copy_line");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Hevc,
            ..Default::default()
        },
    );
    let line = {
        wait(&handle, Duration::from_secs(600)).expect("the export finishes");
        handle.encoders().unwrap_or_default()
    };
    println!("the card's line: {line}");
    assert!(
        line.starts_with("copy · "),
        "the untouched picture was not copied: {line}"
    );
    assert!(
        line.contains("Opus"),
        "the copied picture's line does not name the sound it was written with: {line}"
    );
    // ...and the file agrees with the line, on both halves.
    let bytes = std::fs::read(&*out).expect("read the export back");
    assert!(find(&bytes, b"A_OPUS").is_some(), "the track says A_OPUS");
    assert!(find(&bytes, b"A_AAC").is_none(), "and nothing says A_AAC");
}
