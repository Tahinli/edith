//! Which decoder and which encoder a job really runs on -- the introspection a
//! front-end shows before an operation and while it runs.
//!
//! The point of every assertion here is *truthfulness*, not a particular seat:
//! this box may or may not have a working plugin, and a test that demanded
//! hardware would only be measuring the machine. What must hold is that the
//! answer given before an operation is the answer the operation then gives, and
//! that neither of them ever names a decoder that could not have run (software
//! HEVC, for one, does not exist here).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::Codec;
use engine::decode::{Backend, probe};
use engine::export::{EncoderSeat, ExportSettings, Format};
use engine::scratch::Scratch;
use engine::{ExportHandle, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn out_path(name: &str, ext: &str) -> Scratch {
    Scratch::file(&format!("ve_backend_{name}"), ext)
}

/// A still has no coded stream and no decoder to pick, and it must say so
/// rather than borrowing the picture path's answer.
#[test]
fn a_still_probes_as_a_still() {
    let (codec, backend) = probe(&asset("test_still.png")).expect("probe the still");
    assert_eq!(codec, None, "an image has no coded stream to name");
    assert_eq!(backend, Backend::Still);
}

/// H.264 has both seats, so the probe may legitimately answer either -- but it
/// must answer one of them, and it must name the codec.
#[test]
fn an_h264_source_probes_as_one_of_the_two_seats() {
    let (codec, backend) = probe(&asset("test_baseline.mp4")).expect("probe the baseline fixture");
    assert_eq!(codec, Some(Codec::H264));
    assert!(
        matches!(backend, Backend::Hardware | Backend::Software),
        "a probe that returned must have picked a seat, got {backend:?}"
    );
}

/// HEVC has no software decoder here at all. A box with the plugin answers
/// hardware; a box without it is refused by name -- and *never* told that
/// `rusty_h264` will take it, which is the one answer that would be a lie.
#[test]
fn hevc_probes_as_hardware_or_as_a_refusal() {
    match probe(&asset("test_hevc.mkv")) {
        Ok((codec, backend)) => {
            assert_eq!(codec, Some(Codec::Hevc));
            assert_eq!(backend, Backend::Hardware, "there is no software HEVC here");
        }
        Err(e) => assert!(
            e.to_string().contains("plugin"),
            "a refusal names the plugin, got {e}"
        ),
    }
}

/// The running session reports the decoder its *worker* opened, and it agrees
/// with what the probe said before anything played. This is the whole point of
/// the cell: the worker writes it where the fallback happens, so a hardware
/// session that dropped to software could only be seen here.
#[test]
fn a_playing_session_reports_the_seat_the_probe_named() {
    let path = asset("test_baseline.mp4");
    let (_codec, probed) = probe(&path).expect("probe the baseline fixture");
    let session = PlaybackSession::open(&path).expect("open the fixture");
    let started = Instant::now();
    loop {
        let live = session.decode_backend();
        if live != Backend::Opening {
            assert_eq!(live, probed, "the running seat is not the probed one");
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the worker never published a decode backend"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// What the export card says before the button and what the progress line says
/// during the job are the same two answers, from the same two functions: the
/// picture's seat is probed by opening the very encoder the export opens, and
/// the sound's is decided outright. A drift between them is a front-end lying
/// about the machine, which is exactly what this whole surface is for.
///
/// Every picture format, because each one is a different pair of seats: H.264
/// through `rusty_h264` or the plugin, and AV1 through `rav1e` in either
/// container -- a container is not an encoder, and the card must not say it is.
#[test]
fn the_planned_encoders_are_the_ones_the_job_opens() {
    for (format, ext) in [
        (Format::Mp4, "mp4"),
        (Format::Av1, "mkv"),
        (Format::Av1Mp4, "mp4"),
        // Both HEVC containers, whose seat is the plugin's where the picture
        // clears the driver's floor and the software intra encoder where it does
        // not -- which of the two is this box's business and never this test's.
        (Format::Hevc, "mkv"),
        (Format::HevcMp4, "mp4"),
    ] {
        let settings = ExportSettings {
            format,
            ..ExportSettings::default()
        };
        let session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
        let planned = format!(
            "{} · {}",
            engine::export::planned_video(session.meta(), &settings)
                .expect("these formats carry picture"),
            session.planned_audio(format)
        );
        let out = out_path("planned", ext);
        let handle = session.export_to_with(&out, &settings);
        let opened = published_encoders(&handle, ext);
        // Nothing here wants the file, only the seat it was going to be written
        // with: cancelling deletes the `.part` the worker had started.
        handle.cancel();
        wind_down(&handle);
        assert_eq!(opened, planned, "{ext}");
        // ...and the sound is named on both, AV1 included: an AV1 export
        // carries the timeline's audio now, so a line that said nothing about
        // it would be the old picture-only lie in a new place.
        assert!(planned.contains("AAC"), "{ext}: {planned}");
        if matches!(format, Format::Hevc | Format::HevcMp4) {
            assert!(
                planned.contains("oxideav-h265 intra") || planned.contains("HW encode"),
                "{ext}: an HEVC job names one of the two HEVC seats: {planned}"
            );
        }
    }
}

/// The same pin, swept over the two things that *move* the seat under a user's
/// hand: the picture size (the plugin refuses HEVC below the driver's 384x384
/// floor and the software intra encoder takes the file) and the seat picked on
/// the card ([`engine::export::EncoderSeat`]). The export card reads
/// [`engine::export::planned_video`] for every one of these combinations, so a
/// size that flips the seat and a card that kept the old line is the exact lie
/// this sweep exists to catch.
///
/// Not swept: the hardware AV1 seat, which is what `Hardware` means for the two
/// AV1 rows. Opening it hung this box's GPU into a driver reset (2026-08-10) and
/// a test suite is not the place to find out whether that is fixed -- `av1.rs`
/// keeps that one `#[ignore]`d and by name -- so `Hardware` is swept over the
/// formats whose seat is safe to open, and the AV1 pair only over the seats that
/// open no plugin at all.
#[test]
fn the_planned_seat_follows_the_size_and_the_pin() {
    for (format, ext) in [
        (Format::Mp4, "mp4"),
        (Format::Av1, "mkv"),
        (Format::Av1Mp4, "mp4"),
        (Format::Hevc, "mkv"),
        (Format::HevcMp4, "mp4"),
    ] {
        for (width, height) in [(320, 180), (1280, 720), (1920, 1080)] {
            for seat in EncoderSeat::ALL {
                if seat == EncoderSeat::Hardware && matches!(format, Format::Av1 | Format::Av1Mp4) {
                    continue;
                }
                let settings = ExportSettings {
                    format,
                    seat,
                    ..ExportSettings::default()
                };
                let mut session =
                    PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
                // The project resolution is what an export writes, so this is
                // the very size the card's probe is asked about.
                session.set_resolution(width, height);
                let planned = engine::export::planned_video(session.meta(), &settings)
                    .expect("these formats carry picture");
                // A pinned job is software by definition, whatever the machine
                // has -- the one seat this file may assert outright.
                if seat == EncoderSeat::Software {
                    assert!(
                        planned.starts_with("SW encode"),
                        "{ext} {width}x{height}: a pinned job is software, got {planned}"
                    );
                }
                let out = out_path("seat", ext);
                let handle = session.export_to_with(&out, &settings);
                // `Hardware` on a box with no seat for this size is the one
                // combination that opens nothing: the job refuses in words and
                // the card said the same before the button ([`NO_HW_SEAT`]),
                // which is the claim -- a refusal that fires while a seat *is*
                // there would be caught by the branch below, where the export
                // then has to open one.
                if planned == engine::export::NO_HW_SEAT {
                    // The verdict is read here rather than through `wind_down`,
                    // which takes it out of the handle on its way past.
                    let started = Instant::now();
                    let err = loop {
                        if let Some(result) = handle.result() {
                            break result
                                .expect_err("hardware was demanded and refused")
                                .to_string();
                        }
                        assert!(
                            started.elapsed() < Duration::from_secs(60),
                            "{ext}: a refused export never settled"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    };
                    assert!(
                        err.contains("no VA-API seat"),
                        "{ext} {width}x{height}: the refusal names the missing seat, got {err}"
                    );
                    assert!(
                        !out.exists(),
                        "{ext} {width}x{height}: a refused export wrote a file"
                    );
                    continue;
                }
                let opened = published_encoders(&handle, &format!("{ext} {width}x{height}"));
                handle.cancel();
                wind_down(&handle);
                assert!(
                    opened.starts_with(planned),
                    "{ext} {width}x{height} seat={seat:?}: the card said {planned}, the job opened {opened}"
                );
                if seat == EncoderSeat::Hardware {
                    assert!(
                        opened.starts_with("HW encode"),
                        "{ext} {width}x{height}: hardware was demanded and not refused, so the \
                         job runs on it -- got {opened}"
                    );
                }
            }
        }
    }
}

/// A WAV job has no picture at all and still names what writes it, so an audio
/// export is not a progress line with nothing to say.
#[test]
fn an_audio_only_job_names_its_encoder() {
    for format in [Format::Wav, Format::Flac, Format::Mp3, Format::Ogg] {
        audio_only_job_names_its_encoder(format);
    }
}

fn audio_only_job_names_its_encoder(format: Format) {
    let settings = ExportSettings {
        format,
        ..ExportSettings::default()
    };
    assert_eq!(
        engine::export::planned_video(
            &engine::demux::Demuxer::open(&asset("test_av.mp4"))
                .expect("open the fixture")
                .0,
            &settings
        ),
        None,
        "an audio-only format has no video seat to name"
    );
    let session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    let planned = session.planned_audio(format);
    let out = out_path("audio", format.ext());
    let handle = session.export_to_with(&out, &settings);
    let started = Instant::now();
    let opened = loop {
        if let Some(line) = handle.encoders() {
            break line;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "no encoder was published"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    handle.cancel();
    wind_down(&handle);
    let _ = std::fs::remove_file(&out);
    assert_eq!(opened, planned, "{}", format.ext());
}

/// A mixed timeline cannot be copied, and the card must say so *before* the
/// export: a fader and a limiter both live where the lanes are summed, and a
/// copied AAC packet was never summed at all -- so an mp4 of such a timeline is
/// decoded and re-encoded, and the line names the encoder that will do it.
///
/// The truthfulness rule of this whole file, applied to the one edit that used
/// to be invisible to it.
#[test]
fn a_mixed_timeline_says_it_re_encodes_and_then_does() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    assert_eq!(
        session.planned_audio(Format::Mp4),
        "AAC copy",
        "a timeline nobody has mixed is still a copy"
    );
    // A fader off unity...
    assert!(session.set_lane_gain_db(engine::project::Lane::A1, -3.0));
    let planned = session.planned_audio(Format::Mp4);
    assert!(planned.contains("encode"), "a faded lane: {planned}");
    // ...and back at unity it is a copy again: the answer follows the edit, it
    // is not a latch.
    assert!(session.set_lane_gain_db(engine::project::Lane::A1, 0.0));
    assert_eq!(session.planned_audio(Format::Mp4), "AAC copy");
    // ...and the limiter alone does it too.
    assert!(session.set_limiter(engine::limiter::Limiter {
        ceiling_db: -1.0,
        on: true,
    }));
    assert_eq!(session.planned_audio(Format::Mp4), planned);

    // ...and the job opens exactly that, which is the pin the whole file is
    // about: the encoders line published by the running export.
    let out = out_path("mixed", "mp4");
    let handle = session.export_to_with(
        &out,
        &ExportSettings {
            format: Format::Mp4,
            ..ExportSettings::default()
        },
    );
    let opened = published_encoders(&handle, "a mixed timeline");
    assert!(
        opened.ends_with(&planned),
        "{opened} does not end in {planned}"
    );
    // ...and it is the line the job *settles* on, which is why this one runs to
    // the end instead of cancelling: an mp4 mixes its sound beside the picture,
    // and a hardware picture is through first, so the measured codec reaches the
    // line where the pass is collected under the muxer rather than in the frame
    // loop. A card left saying "working out the sound…" over a written file is
    // the same lie as one naming a codec that never ran.
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the export did not finish in 300 s"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.result().expect("an outcome").expect("the export");
    let settled = handle.encoders().expect("a finished job still names its seats");
    assert!(
        settled.ends_with(&planned),
        "the export settled on {settled}, which does not end in {planned}"
    );
}

/// The half-line an export shows while its sound is still being mixed on the
/// thread beside the picture (`engine::export::publish_seats`). An mp4 job
/// overlaps the two passes, so this is what the *first* line out of one says
/// about a codec that has not run yet.
const MIXING: &str = "working out the sound…";

/// The encoders line a running job settles on: published as soon as the picture
/// seat is open, and completed once the sound has been measured. Waiting for the
/// second half is what makes this the line a card shows rather than a snapshot
/// of an export mid-decision -- and, because a job that ends without ever
/// completing it fails here, it is also the pin on that completion happening at
/// all, whichever of the two passes finishes first.
fn published_encoders(handle: &ExportHandle, what: &str) -> String {
    let started = Instant::now();
    loop {
        match handle.encoders() {
            Some(line) if !line.ends_with(MIXING) => return line,
            _ => {}
        }
        assert!(
            !handle.is_finished(),
            "{what}: the export settled without naming its encoders ({:?}): {:?}",
            handle.encoders(),
            handle.result().map(|r| r.err())
        );
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{what}: no encoder was published"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits for a cancelled job to settle, so its `.part` is gone before the test
/// ends -- and so no worker outlives the process holding a VA-API session.
fn wind_down(handle: &ExportHandle) {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "a cancelled export did not settle"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = handle.result();
}
