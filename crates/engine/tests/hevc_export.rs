//! HEVC export: the codec this project could read and not write until OxideAV's
//! pure-Rust H.265 gave it an encoder -- an **intra-only** one, every frame a
//! self-contained IDR, which is the only shape of it fast enough to wait for
//! (the inter modes code 1080p at 0.81 fps across 12 cores; the intra path does
//! 4.30 fps on the same picture).
//!
//! Three things are under test here: that the *containers* say HEVC in the
//! words their readers expect (this project's own demuxer, and ffprobe where it
//! is installed); that a picture whose height is not a multiple of the 64-sample
//! coding tree block -- 1080, the resolution -- comes back out at its own size,
//! coded padded to 1088 and cropped by the SPS conformance window; and, for the
//! hardware seat only, that what it codes is the *picture that went in* rather
//! than a stream that merely decodes. That last one is not a nicety: a seat
//! whose parameter sets disagree with its coded syntax hands back the right
//! number of frames and the wrong pixels, and nothing else here would see it.
//!
//! ```text
//! cargo test -p engine --release --test hevc_export -- --test-threads=1
//! ```
//!
//! Release, always: a debug intra encode is minutes a frame. The 1080p twin
//! needs the `ffmpeg` CLI to make its source and `ffprobe` to read the coded
//! size back; without them it says so and passes, exactly as the hardware tests
//! here do.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::demux::{Codec, Demuxer};
use engine::export::{ExportSettings, Format};
use engine::scratch::Scratch;
use engine::{AudioSession, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn out_path(name: &str, ext: &str) -> Scratch {
    Scratch::file(&format!("ve_hevc_{name}"), ext)
}

/// Runs an export to completion, failing on the deadline rather than hanging.
fn export(session: &PlaybackSession, out: &Path, format: Format) {
    let settings = ExportSettings {
        format,
        ..Default::default()
    };
    let handle = session.export_to_with(out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the export did not finish in 300 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle
        .result()
        .expect("an outcome")
        .expect("an HEVC export");
    assert_eq!(handle.progress(), 1.0, "finished at full progress");
}

/// Two frames of the A/V fixture: what is under test is the wiring from the
/// format to the muxer, not how long an encoder takes.
fn two_frames() -> PlaybackSession {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av.mp4");
    assert!(session.cut_at(2.0 / 30.0), "cut two frames off the front");
    assert!(session.delete_clip(engine::project::Lane::V1, 1));
    session
}

/// The container half, in both boxes: an HEVC export is read back as HEVC, at
/// the size and the count it was written at, with the timeline's sound beside
/// it. The `hvcC` record this writes is the one `demux` parses -- Matroska in
/// `CodecPrivate`, mp4 in the sample entry -- so this is where the writer and
/// the reader either agree or do not.
#[test]
fn an_hevc_export_reopens_through_our_own_demuxer_in_either_container() {
    for (format, ext) in [(Format::Hevc, "mkv"), (Format::HevcMp4, "mp4")] {
        let session = two_frames();
        let out = out_path("container", ext);
        export(&session, &out, format);

        let (meta, mut demuxer) = Demuxer::open(&out).expect("reopen the export");
        assert_eq!(meta.codec, Codec::Hevc, "{ext}: an HEVC export is HEVC");
        assert_eq!((meta.width, meta.height), (1280, 720), "{ext}");
        assert_eq!(meta.frame_count, 2, "{ext}: every timeline frame is there");
        // Every access unit comes back, and every one of them is a sync point:
        // that is what intra-only means, and it is what makes any frame of the
        // file a cut point.
        let mut count = 0;
        while demuxer.next_access_unit().expect("read").is_some() {
            count += 1;
        }
        assert_eq!(count, 2, "{ext}: every coded picture comes back out");
        assert_eq!(
            demuxer.seek_to_sync_at_or_before(1),
            1,
            "{ext}: an intra-only file syncs on every frame"
        );

        // ...and the sound with it, through the reader an import opens it by.
        let (audio, chunks) = AudioSession::open(&out)
            .expect("reopen for its sound")
            .expect("an HEVC export has an audio track");
        let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
        eprintln!(
            "{ext}: {} Hz x{}, rms {rms:.4}, {} bytes",
            audio.sample_rate,
            audio.channels,
            std::fs::metadata(&out).unwrap().len()
        );
        assert!(rms > 0.001, "{ext}: the sound is in the file (rms {rms})");
        std::fs::remove_file(&out).unwrap();
    }
}

/// The %16 half, and the reason the vendored encoder carries a patch at all:
/// 1080 is not a multiple of the 16-sample CTB, so the picture is coded at
/// 1920x1088 and the SPS crops it back with a §7.4.3.2.1 conformance window.
/// ffprobe is what says whether that worked -- it reads the *bitstream*, not
/// the container's own claim -- and ffmpeg decoding the file without a
/// complaint is what says the stream is legal.
#[test]
fn a_1080p_export_states_1920x1080_through_the_conformance_window() {
    let Some(source) = fixture_1080p() else {
        eprintln!("no ffmpeg: skipping the 1080p conformance-window twin");
        return;
    };
    for (format, ext) in [(Format::Hevc, "mkv"), (Format::HevcMp4, "mp4")] {
        let mut session = PlaybackSession::open(&source).expect("open the 1080p fixture");
        assert!(session.cut_at(2.0 / 30.0));
        assert!(session.delete_clip(engine::project::Lane::V1, 1));
        let out = out_path("1080p", ext);
        export(&session, &out, format);

        // The container's own claim first: this project's reader.
        let (meta, _) = Demuxer::open(&out).expect("reopen the 1080p export");
        assert_eq!((meta.width, meta.height), (1920, 1080), "{ext}");

        // ...and the coded size, out of the SPS, by a reader that is not ours.
        let probed = probe(&out);
        assert_eq!(
            probed.as_deref(),
            Some("hevc,1920,1080"),
            "{ext}: ffprobe reads the cropped size out of the bitstream"
        );
        // ...and the whole file decoded, with ffmpeg's own stderr as the test:
        // a stream a decoder complains about is not an export.
        let decoded = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-f", "null", "-"])
            .output()
            .expect("run ffmpeg");
        let complaints = String::from_utf8_lossy(&decoded.stderr);
        assert!(
            decoded.status.success() && complaints.is_empty(),
            "{ext}: ffmpeg complained: {complaints}"
        );
        std::fs::remove_file(&out).unwrap();
    }
    let _ = std::fs::remove_file(&source);
}

/// The loop closed where it matters to a user: the file this wrote is opened
/// again *by edith*, picture and all. HEVC has no software decoder here at all,
/// so the way back in is the plugin -- which is what makes this the twin that
/// needs one, and what makes the test above (the containers, read by the
/// demuxer) the one that does not.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with an HEVC entrypoint"]
fn an_hevc_export_decodes_back_into_pictures_through_edith() {
    for (format, ext) in [(Format::Hevc, "mkv"), (Format::HevcMp4, "mp4")] {
        let session = two_frames();
        let out = out_path("roundtrip", ext);
        export(&session, &out, format);

        let (meta, frames) = engine::DecodeSession::open(&out).expect("decode the export");
        assert_eq!(meta.codec, Codec::Hevc, "{ext}");
        let frames: Vec<_> = frames.into_iter().collect();
        assert_eq!(frames.len(), 2, "{ext}: every written frame decodes back");
        // The picture is the fixture's colour pattern, not black: a conformance
        // window read wrong would show as a shifted or empty picture.
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!((frame.width, frame.height), (1280, 720), "{ext} frame {i}");
            assert!(
                frame.bgra.iter().any(|&p| p > 32),
                "{ext} frame {i} carries picture"
            );
        }
        std::fs::remove_file(&out).unwrap();
    }
}

/// The same 1080p picture, coded by the **GPU**, and the test that would have
/// caught what shipped: the hardware seat's file decoded frame for frame while
/// every frame of it was a band of noise over flat green, because the parameter
/// sets promised coding tools the driver was not using. Nothing that counts
/// frames sees that. Three things do, and all three are asserted here --
/// ffmpeg's decoder saying *nothing at all* on its error channel, the coded
/// size still cropping back to 1920x1080, and the picture being the source's
/// rather than a fill (SSIM against the very frames that went in; a desynced
/// stream scores about 0.2, this one about 0.98).
///
/// `testsrc2` is what makes it bite: 1080p of it splits transform trees deep
/// enough and pushes the rate controller hard enough to need the syntax the
/// parameter sets have to declare. The two-frame fixtures above are flat and
/// pass either way, which is exactly how the defect got through.
#[test]
#[ignore = "needs libengine_hw.so and a driver with an HEVC encode entrypoint"]
fn a_1080p_hardware_export_leaves_a_decoder_with_nothing_to_say() {
    let Some(source) = fixture_1080p() else {
        eprintln!("no ffmpeg: skipping the 1080p hardware twin");
        return;
    };
    let session = PlaybackSession::open(&source).expect("open the 1080p fixture");
    let settings = ExportSettings {
        format: Format::Hevc,
        ..Default::default()
    };
    let planned = engine::export::planned_video(session.meta(), &settings);
    assert!(
        planned.is_some_and(|seat| seat.contains("HW encode")),
        "this twin is the GPU's, and this box named {planned:?}"
    );

    let out = out_path("1080p_hw", "mkv");
    export(&session, &out, Format::Hevc);

    let probed = probe(&out);
    assert_eq!(
        probed.as_deref(),
        Some("hevc,1920,1080"),
        "ffprobe reads the cropped size out of the bitstream"
    );

    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    let complaints = String::from_utf8_lossy(&decoded.stderr);
    assert!(
        decoded.status.success() && complaints.is_empty(),
        "a decoder complained about the GPU's stream: {complaints}"
    );

    let ssim = ssim(&source, &out).expect("ffmpeg reports an SSIM");
    assert!(
        ssim >= 0.90,
        "the GPU coded the source's picture, not a fill (SSIM {ssim:.4})"
    );
    std::fs::remove_file(&out).unwrap();
    let _ = std::fs::remove_file(&source);
}

/// An audio-only timeline has no picture to code, and an HEVC export says so by
/// name exactly as the mp4 and AV1 ones do -- and writes nothing.
#[test]
fn an_audio_only_timeline_is_refused_an_hevc_export_by_name() {
    let session = PlaybackSession::open(asset("test_tone.wav")).expect("open the tone");
    let out = out_path("refused", "mkv");
    let settings = ExportSettings {
        format: Format::Hevc,
        ..Default::default()
    };
    let handle = session.export_to_with(&out, &settings);
    while !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let refused = handle
        .result()
        .expect("an outcome")
        .expect_err("an audio-only timeline has no picture to code")
        .to_string();
    assert!(refused.contains("no picture"), "{refused}");
    assert!(refused.contains("HEVC"), "{refused}");
    assert!(!out.exists(), "nothing is written for a refusal");
}

/// A cancelled HEVC export leaves nothing behind -- not the file and not the
/// `.part` the worker was writing. An intra-only export of a whole fixture is
/// long enough to catch mid-flight, and the frame loop checks for a cancel
/// every picture, so this stops in a frame or two rather than at the end.
#[test]
fn a_cancelled_hevc_export_leaves_no_file_behind() {
    let session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av.mp4");
    let out = out_path("cancelled", "mkv");
    let settings = ExportSettings {
        format: Format::Hevc,
        ..Default::default()
    };
    let handle = session.export_to_with(&out, &settings);
    handle.cancel();
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(60), "cancel hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    let stopped = handle
        .result()
        .expect("an outcome")
        .expect_err("a cancelled export is an error");
    assert!(stopped.to_string().contains("cancelled"), "{stopped}");
    assert!(!out.exists(), "no output for a cancelled export");
    assert!(
        !PathBuf::from(format!("{}.part", out.display())).exists(),
        "no partial file either"
    );
}

/// `codec,width,height` of the file's video stream as ffprobe reads it, or
/// `None` where ffprobe is not installed.
fn probe(path: &Path) -> Option<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Mean SSIM of `out` against `source`, over the frames the two have in common,
/// as ffmpeg's own filter measures it. `None` where ffmpeg is not installed or
/// said nothing -- the caller decides whether that is a skip or a failure.
fn ssim(source: &Path, out: &Path) -> Option<f64> {
    let measured = Command::new("ffmpeg")
        .args(["-v", "info", "-i"])
        .arg(source)
        .arg("-i")
        .arg(out)
        .args(["-filter_complex", "ssim", "-f", "null", "-"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&measured.stderr)
        .rsplit("All:")
        .next()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// A two-second 1920x1080 H.264 fixture in the temp directory -- the assets are
/// all 720p, which is a multiple of 16 in both directions and would leave the
/// crop path untested. `None` where ffmpeg is not installed.
fn fixture_1080p() -> Option<Scratch> {
    let path = out_path("source1080", "mp4");
    let made = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=30:duration=1",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path)
        .output()
        .ok()?;
    made.status.success().then_some(path)
}
