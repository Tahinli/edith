//! What an exported file says its numbers mean, and what it does to a clip that
//! disagreed: the writing half of `engine::colorspace`.
//!
//! Two claims, both measured with `ffmpeg`/`ffprobe` rather than with this
//! project's own reader alone -- an export is read by other software, and a tag
//! only this engine agrees with is not a tag:
//!
//! 1. every export declares a space (BT.709 at 720 lines and up, BT.601 below),
//!    in the `Colour` element of a Matroska and the `colr` box of an mp4, and
//! 2. a clip coded against the *other* matrix is rewritten into the file's, so
//!    reading the export by its own tags gives back the picture that went in --
//!    a reconcile, not a second conversion.
//!
//! ```text
//! cargo test -p engine --release --test export_color
//! ```
//!
//! Release: the Matroska half goes through the intra HEVC encoder, which is
//! minutes a frame in debug. Without `ffmpeg` on the machine the external half
//! says so and passes, exactly as the hardware tests elsewhere here do.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::colorspace::{ColorDescription, Matrix, Transfer, remap};
use engine::demux::Demuxer;
use engine::export::{ExportSettings, Format};
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn out_path(name: &str, ext: &str) -> Scratch {
    Scratch::file(&format!("ve_color_{name}"), ext)
}

/// The first two frames of `source`, so what is measured is the colour path and
/// not how long an encoder takes.
fn two_frames(source: &Path) -> PlaybackSession {
    let mut session = PlaybackSession::open(source).expect("open the fixture");
    assert!(session.cut_at(2.0 / 30.0), "cut two frames off the front");
    assert!(session.delete_clip(engine::project::Lane::V1, 1));
    session
}

fn export(session: &PlaybackSession, out: &Path, format: Format) {
    let handle = session.export_to_with(
        out,
        &ExportSettings {
            format,
            ..Default::default()
        },
    );
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the export did not finish in 300 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
}

/// What ffprobe says the stream declares: matrix, primaries, transfer, range.
/// `None` when ffprobe is not installed.
fn probed(path: &Path) -> Option<Vec<String>> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=color_space,color_primaries,color_transfer,color_range",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .collect(),
    )
}

/// Frame 0 of `path` as planar 4:2:0 bytes, straight off the decoder with no
/// colour conversion at all -- the samples as they are in the file, which is
/// what a remap is measured on. `None` when ffmpeg is not installed.
fn raw_yuv(path: &Path) -> Option<Vec<u8>> {
    let raw = Scratch::file("ve_color_raw", "yuv");
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .arg(&*raw)
        .status()
        .ok()?;
    assert!(ok.success(), "ffmpeg could not read {}", path.display());
    Some(std::fs::read(&*raw).expect("the raw frame"))
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "frame sizes differ");
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum::<f64>()
        / a.len() as f64
}

/// Every export says which space it is in, in the words its container spells
/// them and in the words ffprobe reads back. `test_bt601.mp4` is 1280x720 and
/// the export of it is therefore BT.709 whatever the source said; the 640x480
/// fixture is below the line and comes out BT.601 whatever *its* bitstream said.
/// Both containers, because the two tags are written by different code.
#[test]
fn an_export_declares_the_space_it_was_written_in() {
    for (fixture, matrix, probe) in [
        ("test_bt601.mp4", Matrix::Bt709, "bt709"),
        ("test_vui_h264.mp4", Matrix::Bt601, "smpte170m"),
    ] {
        for (format, ext) in [(Format::Mp4, "mp4"), (Format::Hevc, "mkv")] {
            let session = two_frames(&asset(fixture));
            let out = out_path(&format!("tags_{ext}"), ext);
            export(&session, &out, format);

            let (meta, _) = Demuxer::open(&out).expect("reopen the export");
            assert_eq!(
                meta.color,
                ColorDescription {
                    matrix,
                    transfer: Transfer::Sdr,
                    full_range: false,
                },
                "{fixture} -> {ext}: what the file says about itself"
            );
            match probed(&out) {
                Some(tags) => {
                    eprintln!("{fixture} -> {ext}: ffprobe {tags:?}");
                    // ffprobe prints these in its own field order, range first,
                    // whatever order they were asked for in.
                    let want: Vec<String> = ["tv", probe, probe, probe]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    assert_eq!(
                        tags, want,
                        "{fixture} -> {ext}: range, matrix, primaries, transfer"
                    );
                }
                None => eprintln!("no ffprobe: skipping the external read of {ext}"),
            }
            std::fs::remove_file(&out).unwrap();
        }
    }
}

/// The reconcile itself, on the samples: a BT.601 clip on a 720-line canvas is
/// written as BT.709, and the numbers in the file are the source's *remapped* --
/// within what the encoder rounds off. The second measurement is the one that
/// makes the first mean something: the untouched source is far further away
/// than the remapped one, so this cannot pass by doing nothing.
#[test]
fn a_clip_in_another_space_is_remapped_into_the_files() {
    let source = asset("test_bt601.mp4");
    let Some(src) = raw_yuv(&source) else {
        eprintln!("no ffmpeg: skipping the sample-level remap check");
        return;
    };
    let session = two_frames(&source);
    let out = out_path("remap", "mp4");
    export(&session, &out, Format::Mp4);
    let got = raw_yuv(&out).expect("ffmpeg read the export");

    let (width, height) = (1280usize, 720usize);
    let mut planes = src.clone();
    let (y, chroma) = planes.split_at_mut(width * height);
    let (u, v) = chroma.split_at_mut(width * height / 4);
    remap(Matrix::Bt601, Matrix::Bt709, y, u, v, width);

    let remapped = mean_abs_diff(&got, &planes);
    let untouched = mean_abs_diff(&got, &src);
    eprintln!(
        "601 clip on a 709 canvas: {remapped:.2} off the remapped source, {untouched:.2} off the raw one"
    );
    assert!(
        remapped < 3.0,
        "the export is {remapped:.2} away from the remapped source"
    );
    assert!(
        untouched > 3.0 * remapped,
        "remapped {remapped:.2} vs untouched {untouched:.2}: nothing was reconciled"
    );
    std::fs::remove_file(&out).unwrap();
}

/// ...and the other half of that claim, which is the regression one: a clip
/// already in the file's space is not remapped at all, so an ordinary
/// single-space project comes out exactly as it did before any of this existed.
/// `test_baseline.mp4` is 1280x720 and untagged, which resolves to BT.709 --
/// the very space its export declares.
#[test]
fn a_same_space_clip_is_left_alone() {
    let source = asset("test_baseline.mp4");
    let Some(src) = raw_yuv(&source) else {
        eprintln!("no ffmpeg: skipping the same-space regression check");
        return;
    };
    let (meta, _) = Demuxer::open(&source).expect("open the fixture");
    assert_eq!(meta.color.matrix, Matrix::Bt709, "the fixture's own space");
    let session = two_frames(&source);
    let out = out_path("same_space", "mp4");
    export(&session, &out, Format::Mp4);
    let got = raw_yuv(&out).expect("ffmpeg read the export");

    let diff = mean_abs_diff(&got, &src);
    eprintln!("same-space export: {diff:.2} off the source samples");
    assert!(
        diff < 3.0,
        "a same-space export moved the samples by {diff:.2}"
    );
    std::fs::remove_file(&out).unwrap();
}
