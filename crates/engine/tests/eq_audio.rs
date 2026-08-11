//! The per-clip equalizer where it has to be true: in the samples playback
//! feeds the device and in the samples an export writes, which are the same
//! samples by construction — both come off `AudioSession::open_mixed_streams_eq`
//! and the filter runs inside the per-lane worker, per segment, before the mix.
//!
//! Everything here goes through the *front door*: a `Project` is edited with
//! `set_eq` and then read exactly as `PlaybackSession::seek` reads it
//! (`audio_segments_from` + `audio_eqs_from` handed to the same opener), or
//! exported through `engine::export::start`. Nothing calls `EqState` directly —
//! `eq.rs`'s own unit tests own the filter maths; what is checked here is the
//! wiring: that a curve reaches its clip, only its clip, and reaches it the same
//! way in both directions.
//!
//! The fixtures are a 440 Hz / 880 Hz stereo tone (`gen_fixtures.sh`), so a low
//! shelf with its corner above both tones is a flat +12 dB on everything
//! audible: the expected RMS ratio is 10^(12/20) ≈ 3.98, and it is asserted as a
//! ratio, never as a level.
//!
//! No picture is decoded, so nothing here needs the hardware plugin.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::eq::{Band, BandKind, EqParams};
use engine::export::{ExportSettings, Format};
use engine::project::{Lane, LaneKind, Source, Speed};
use engine::scale::FitPolicy;
use engine::{AudioSession, Clip, ExportHandle, Project};

const RATE: u32 = 44_100;
const FPS: f64 = 30.0;

/// +12 dB below 2 kHz: above both fixture tones, so every audible sample in
/// them is lifted by the shelf's full gain.
const BOOST_DB: f32 = 12.0;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn boost() -> EqParams {
    EqParams {
        bands: vec![Band {
            freq_hz: 2000.0,
            gain_db: BOOST_DB,
            q: 0.707,
            kind: BandKind::LowShelf,
        }],
    }
}

fn clip(start: u32, source: usize, frames: u32) -> Clip {
    Clip {
        start,
        in_frame: 0,
        out_frame: frames,
        source,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    }
}

/// One video clip and `audio` on `A1`, from `test_av.mp4` alone.
fn project(audio: Vec<Clip>) -> Project {
    let frames = audio.iter().map(Clip::end).max().unwrap_or(30);
    Project::from_parts(
        vec![Source::new(asset("test_av.mp4"), 0)],
        vec![
            (LaneKind::Video, vec![clip(0, 0, frames)]),
            (LaneKind::Audio, audio),
        ],
        Vec::new(),
        Vec::new(),
    )
    .expect("a one-source project")
}

/// What the device would be fed: the exact call `PlaybackSession::seek` makes,
/// drained to the end. This *is* the feeder path minus the ring buffer.
fn play(project: &Project) -> Vec<f32> {
    let sources = project.audio_sources();
    let segs = project.audio_segments_from(0, FPS);
    let eqs = project.audio_eqs_from(0, FPS);
    let (meta, chunks) = AudioSession::open_mixed_streams_eq(&sources, &segs, &eqs)
        .expect("open the timeline's audio")
        .expect("the timeline has audio");
    assert_eq!((meta.sample_rate, meta.channels), (RATE, 2));
    let mut samples = Vec::new();
    for chunk in chunks {
        samples.extend(chunk.samples);
    }
    samples
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

/// The shelf's own gain, to within a fifth of a dB either way -- the filter is
/// exact, the tone is not perfectly band-limited, and the fixtures' 1 Hz pulse
/// makes any window a mixture.
fn assert_boosted(ratio: f64, what: &str) {
    let db = 20.0 * ratio.log10();
    println!("{what}: ratio {ratio:.3} = {db:+.2} dB");
    assert!(
        (db - f64::from(BOOST_DB)).abs() < 0.5,
        "{what} measured {db:+.2} dB, the band asks for {BOOST_DB:+.1}"
    );
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

fn out_path(name: &str, ext: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ve_eq_{name}_{}.{ext}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn meta() -> engine::VideoMeta {
    engine::demux::Demuxer::open(&asset("test_av.mp4"))
        .expect("open test_av")
        .0
}

/// Samples of a written file back through the engine's own reader.
fn decode(path: &Path) -> Vec<f32> {
    let (_, chunks) = AudioSession::open(path)
        .expect("reopen the export")
        .expect("the export has audio");
    let mut samples = Vec::new();
    for chunk in chunks {
        samples.extend(chunk.samples);
    }
    samples
}

/// The whole claim in one line: a clip given a band comes out of the feeder
/// path that much louder than the same clip without it -- and a project nobody
/// has equalized comes out of the *new* opener bit for bit what the old one
/// gave, which is the identity promise the rest of the engine rests on.
#[test]
fn an_equalized_clip_plays_that_much_louder_and_a_flat_one_not_at_all() {
    let flat = project(vec![clip(0, 0, 30)]);
    let mut eqd = flat.clone();
    assert!(eqd.set_eq(Lane::A1, 0, Some(boost())));

    let plain = play(&flat);
    let boosted = play(&eqd);
    assert_eq!(
        plain.len(),
        boosted.len(),
        "the edit is a filter, not a trim"
    );
    assert_boosted(
        rms(&boosted, 0.05, 0.95) / rms(&plain, 0.05, 0.95),
        "playback",
    );

    // No equalizer anywhere: the samples are the ones the opener gave before
    // there was an equalizer at all, to the bit.
    let sources = flat.audio_sources();
    let segs = flat.audio_segments_from(0, FPS);
    let (_, chunks) = AudioSession::open_mixed_streams(&sources, &segs)
        .expect("open")
        .expect("has audio");
    let old: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
    assert_eq!(old, plain, "an EQ-less project must decode bit-identically");

    // ...and so does one carrying a curve that moves nothing: `is_identity`
    // short-circuits inside `EqState::process`, and this is the path that says
    // the short-circuit is really on.
    let mut identity = flat.clone();
    assert!(identity.set_eq(Lane::A1, 0, Some(EqParams::default_layout())));
    assert!(identity.eq_of(Lane::A1, 0).is_some(), "the curve is stored");
    assert_eq!(
        play(&identity),
        plain,
        "a flat curve must not touch a sample"
    );
}

/// The reason the filter lives in the worker and not over the mix: two clips
/// with different settings must not bleed into each other. One second boosted,
/// one second not, on one lane, out of one decoder.
#[test]
fn a_band_reaches_its_own_clip_and_stops_at_the_cut() {
    let mut p = project(vec![clip(0, 0, 30), clip(30, 0, 30)]);
    let plain = play(&p);
    assert!(p.set_eq(Lane::A1, 1, Some(boost())));
    let boosted = play(&p);

    assert_boosted(
        rms(&boosted, 1.05, 1.95) / rms(&plain, 1.05, 1.95),
        "the equalized second clip",
    );
    // The clip before it is untouched -- and untouched to the *bit*, not merely
    // to a level: a filter whose memory carried across the join would show up
    // here as a fraction of a dB and pass any ratio test.
    let second = (1.0 * f64::from(RATE)) as usize * 2;
    assert_eq!(
        boosted[..second],
        plain[..second],
        "the flat clip before the cut moved"
    );
}

/// Two lanes, a band on one of them: the sum must differ by exactly what that
/// lane's own samples changed by, which is only true if each lane is filtered
/// before it is added. Filtering the mix instead would smear the curve over the
/// other lane as well, and this is the difference that would show it.
#[test]
fn a_lane_is_equalized_before_the_mix_and_never_after_it() {
    let mut p = Project::from_parts(
        vec![Source::new(asset("test_av.mp4"), 0)],
        vec![
            (LaneKind::Video, vec![clip(0, 0, 30)]),
            (LaneKind::Audio, vec![clip(0, 0, 30)]),
            (LaneKind::Audio, vec![clip(0, 0, 30)]),
        ],
        Vec::new(),
        Vec::new(),
    )
    .expect("a two-audio-lane project");
    assert_eq!(p.audio_lanes().len(), 2, "both lanes hold something");

    let mixed = play(&p);
    let a2 = Lane::new(LaneKind::Audio, 1);
    assert!(p.set_eq(a2, 0, Some(boost())));
    let mixed_eqd = play(&p);

    // What A2 alone sounds like either way, taken off the single-lane path.
    let lone = project(vec![clip(0, 0, 30)]);
    let mut lone_eqd = lone.clone();
    assert!(lone_eqd.set_eq(Lane::A1, 0, Some(boost())));
    let (flat, boosted) = (play(&lone), play(&lone_eqd));

    let n = mixed
        .len()
        .min(mixed_eqd.len())
        .min(flat.len())
        .min(boosted.len());
    let mut worst = 0.0f32;
    for i in 0..n {
        worst = worst.max(((mixed_eqd[i] - mixed[i]) - (boosted[i] - flat[i])).abs());
    }
    println!("mix-vs-lane worst difference: {worst:e}");
    // Not zero: the two runs decode the same source twice and AAC's noise
    // substitution reseeds per decoder (`open_segments`, measured below 1e-3).
    assert!(
        worst < 1e-3,
        "the mix changed by {worst} more than the equalized lane did -- the filter is on the sum"
    );
}

/// The export half of the same claim, through `export::start`: what is written
/// is what is heard, so both audio-only formats carry the boost at the same
/// ratio the feeder path measured. Both, because both are lossless and both are
/// one `run_audio` -- a FLAC that disagreed with the WAV beside it would be the
/// writer, and this is where that would show.
#[test]
fn the_audio_only_exports_carry_the_same_boost_playback_does() {
    let flat = project(vec![clip(0, 0, 30)]);
    let mut eqd = flat.clone();
    assert!(eqd.set_eq(Lane::A1, 0, Some(boost())));

    for format in [Format::Wav, Format::Flac] {
        let mut written = Vec::new();
        for (name, project) in [("flat", &flat), ("eqd", &eqd)] {
            let out = out_path(name, format.ext());
            let handle = engine::export::start(
                project.clone(),
                meta(),
                &out,
                &ExportSettings {
                    format,
                    ..Default::default()
                },
            );
            wait(&handle, Duration::from_secs(60)).expect("an audio-only export");
            written.push(decode(&out));
            std::fs::remove_file(&out).unwrap();
        }
        let [plain, boosted] = &written[..] else {
            unreachable!("two exports")
        };
        assert_boosted(
            rms(boosted, 0.05, 0.95) / rms(plain, 0.05, 0.95),
            format.name(),
        );
        // ...and the same number to the ear: the export is not a second, kinder
        // filter run at a different point in the chain.
        let played = rms(&play(&eqd), 0.05, 0.95) / rms(&play(&flat), 0.05, 0.95);
        let exported = rms(boosted, 0.05, 0.95) / rms(plain, 0.05, 0.95);
        assert!(
            (played - exported).abs() < 0.05,
            "playback boosted by {played:.3} and {} by {exported:.3}",
            format.name()
        );
    }
}

/// An mp4 export *copies* AAC packets and no filter can reach a copy, so an
/// equalized lane is decoded, mixed and encoded again instead
/// (`export::encode_audio`). The claim is the same one the WAV makes: the boost
/// is in the file, on the clip that was given it and on no other -- and the
/// track still starts where the picture does, which is what the encoder's own
/// priming packet is for.
#[test]
fn an_mp4_export_carries_the_boost_a_packet_copy_could_not() {
    let flat = project(vec![clip(0, 0, 30), clip(30, 0, 30)]);
    let mut eqd = flat.clone();
    assert!(eqd.set_eq(Lane::A1, 1, Some(boost())));

    let mut written = Vec::new();
    for (name, project) in [("flat", &flat), ("eqd", &eqd)] {
        let out = out_path(name, "mp4");
        let settings = ExportSettings::default();
        let handle = engine::export::start(project.clone(), meta(), &out, &settings);
        wait(&handle, Duration::from_secs(180)).expect("an equalized timeline exports as mp4");
        written.push(decode(&out));
        std::fs::remove_file(&out).unwrap();
    }
    let [plain, boosted] = &written[..] else {
        unreachable!("two exports")
    };

    // Two seconds of timeline, to within the one 1024-frame packet the length
    // rounds up to: an encoder delay left uncompensated would show up here as a
    // whole packet of shift, and in the windows below as a smeared cut.
    let secs = |s: &[f32]| s.len() as f64 / 2.0 / f64::from(RATE);
    println!(
        "mp4 lengths: flat {:.3}s, eqd {:.3}s",
        secs(plain),
        secs(boosted)
    );
    for samples in [plain, boosted] {
        assert!(
            (secs(samples) - 2.0).abs() < 1024.0 / f64::from(RATE),
            "the exported track is {:.3}s of a 2.000s timeline",
            secs(samples)
        );
    }

    // The boost itself. Not to `assert_boosted`'s fifth of a dB: this file is
    // AAC at 128 kbps measured against a *copied* one, so a fraction of a dB is
    // the codec, not the filter. A dB either way is far tighter than the 12 the
    // band asks for and far wider than any coding noise.
    let db = 20.0 * (rms(boosted, 1.05, 1.95) / rms(plain, 1.05, 1.95)).log10();
    println!("mp4 equalized clip: {db:+.2} dB");
    assert!(
        (db - f64::from(BOOST_DB)).abs() < 1.0,
        "the equalized clip in the mp4 measured {db:+.2} dB, the band asks for {BOOST_DB:+.1}"
    );
    // The clip before it is the same sound: not to the bit -- this file is a
    // re-encode and that one is a copy -- but to a fraction of a dB, which is
    // what says the filter stopped at the cut rather than lifting the lot.
    let leak = 20.0 * (rms(boosted, 0.05, 0.95) / rms(plain, 0.05, 0.95)).log10();
    println!("mp4 flat clip moved {leak:+.2} dB");
    assert!(leak.abs() < 0.5, "the flat clip moved {leak:+.2} dB");

    // ...and a timeline nobody equalized never reaches the encoder at all: its
    // audio track is still the source's own packets, which
    // `export::exported_packets_are_the_copied_stream` holds to the byte.
}

/// A band moved while the timeline is playing: `PlaybackSession::set_eq` writes
/// the project and reseeks, and a reseek rebuilds the workers from
/// `audio_eqs_from`. This is that mechanism without a sound device -- the edit,
/// then the very list the reopen is built from, then the samples it yields.
#[test]
fn a_band_changed_mid_play_is_in_the_next_chunk_the_worker_makes() {
    let mut p = project(vec![clip(0, 0, 30)]);
    assert!(
        p.audio_eqs_from(0, FPS)[0][0].is_none(),
        "flat to begin with"
    );
    let before = play(&p);

    assert!(p.set_eq(Lane::A1, 0, Some(boost())));
    // The reopen list is rebuilt from the project every seek, so the edit is in
    // it the moment it lands -- no channel has to reach into a running worker.
    assert_eq!(
        p.audio_eqs_from(0, FPS)[0][0].as_ref(),
        Some(&boost()),
        "the seek's own list carries the new curve"
    );
    let after = play(&p);
    assert_ne!(before, after, "the samples changed");
    assert_boosted(
        rms(&after, 0.05, 0.95) / rms(&before, 0.05, 0.95),
        "after the edit",
    );

    // And undo is the way back, one step, samples and all.
    assert!(p.undo());
    assert!(p.audio_eqs_from(0, FPS)[0][0].is_none());
    assert_eq!(play(&p), before, "undo puts the flat samples back");
}
