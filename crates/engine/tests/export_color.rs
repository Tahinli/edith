//! What an exported file says its numbers mean, and what it does to a clip that
//! disagreed: the writing half of `engine::colorspace`.
//!
//! Two claims, both measured with `ffmpeg`/`ffprobe` rather than with this
//! project's own reader alone -- an export is read by other software, and a tag
//! only this engine agrees with is not a tag:
//!
//! 1. every export declares a space (BT.709 at 720 lines and up, BT.601 below),
//!    in the `Colour` element of a Matroska and the `colr` box of an mp4, *and*
//!    in the coded bitstream, which is the answer a decoder takes first;
//! 2. a clip coded against the *other* matrix is rewritten into the file's, so
//!    reading the export by its own tags gives back the picture that went in --
//!    a reconcile, not a second conversion; and
//! 3. a graded clip comes out of the export the way the preview showed it,
//!    which is what makes a grade a decision and not a guess; and
//! 4. an HDR source is *tone-mapped* into that space rather than tagged into it
//!    ([`engine::tonemap`]) -- once, before the grade, and never on top of a
//!    matrix conversion of the same frame.
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
use engine::tonemap::{self, ToneMapper};

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

/// What ffprobe says the first *coded frame* is, which is the stream's own
/// answer and not the container's: libavcodec fills a frame's colour in from the
/// bitstream, so a codec that signalled nothing reads back "unknown" here even
/// where the file is tagged. `None` when ffprobe is not installed.
fn probed_frame(path: &Path) -> Option<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_frames",
            "-read_intervals",
            "%+#1",
            "-show_entries",
            "frame=color_space",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string(),
    )
}

/// The session's first frame as BGRA, through this engine's own decode path --
/// the very path the preview draws with, so a preview frame and a re-imported
/// export frame are two measurements of one picture and nothing else.
fn frame0_bgra(session: &mut PlaybackSession) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Some(frame) = session.try_frame() {
            return frame.bgra;
        }
        assert!(Instant::now() < deadline, "no frame in 180 s");
        std::thread::sleep(Duration::from_millis(5));
    }
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

/// A one-second 720p fixture whose samples are ordinary 8-bit BT.709 pictures
/// but whose *tags* say BT.2020 PQ -- which is all the colour path reads, so it
/// exercises the tone map without an HDR encoder anywhere near the test. The
/// codes therefore mean something far brighter than they were drawn as, which is
/// the point: an HDR file handed to an SDR display is exactly that mistake.
///
/// The tags go in through `-x264-params`, not ffmpeg's own `-color_trc`: the
/// latter writes the mp4's `colr` box alone here (measured: ffprobe reads the
/// transfer back as "unknown"), and the bitstream is the answer any reader takes
/// first. `None` where ffmpeg is not installed.
fn pq_fixture() -> Option<Scratch> {
    let path = out_path("pq_source", "mp4");
    let made = Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=30:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc",
        ])
        .arg(&*path)
        .output()
        .ok()?;
    made.status.success().then_some(path)
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

/// The tag a *decoder* reads, on every format there is. A container's colour
/// element is the second answer libavcodec looks at and the bitstream's is the
/// first, so a stream that signals nothing is rendered BT.601 whatever the file
/// says about itself -- which is a visible shift on a 709 export and was
/// measured as one (9.78 mean codes off the right render against 2.27 off the
/// wrong one, HEVC before its SPS carried a VUI).
///
/// All five video formats, because the gap was per-codec and a container-level
/// check could not see it: the mp4 and Matroska tags were right the whole time.
#[test]
fn every_format_signals_its_space_in_the_bitstream() {
    if probed_frame(&asset("test_bt601.mp4")).is_none() {
        eprintln!("no ffprobe: skipping the frame-level colour check");
        return;
    }
    for (format, ext, name) in [
        (Format::Mp4, "mp4", "h264-mp4"),
        (Format::Hevc, "mkv", "hevc-mkv"),
        (Format::HevcMp4, "mp4", "hevc-mp4"),
        (Format::Av1, "mkv", "av1-mkv"),
        (Format::Av1Mp4, "mp4", "av1-mp4"),
    ] {
        let session = two_frames(&asset("test_bt601.mp4"));
        let out = out_path(&format!("frametag_{name}"), ext);
        export(&session, &out, format);
        let got = probed_frame(&out).expect("ffprobe is installed");
        eprintln!("{name}: frame colour_space {got}");
        assert_eq!(got, "bt709", "{name}: what a decoder makes of the frame");
        std::fs::remove_file(&out).unwrap();
    }
}

/// An HDR source comes out as an SDR file that *says* it is one, in the
/// container and in the bitstream both. The tags are the half a player reads
/// before it reads a pixel: a file of tone-mapped samples still labelled PQ is
/// shown through a second, inverse curve and is worse than either honest answer.
#[test]
fn an_hdr_source_exports_as_a_tagged_sdr_file() {
    let Some(source) = pq_fixture() else {
        eprintln!("no ffmpeg: skipping the HDR export tags");
        return;
    };
    let (meta, _) = Demuxer::open(&source).expect("open the PQ fixture");
    assert_eq!(meta.color.transfer, Transfer::Pq, "the fixture's own curve");
    assert_eq!(meta.color.matrix, Matrix::Bt2020Ncl, "...and its matrix");

    let session = two_frames(&source);
    let out = out_path("hdr_tags", "mp4");
    export(&session, &out, Format::Mp4);

    let (meta, _) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(
        meta.color,
        ColorDescription {
            matrix: Matrix::Bt709,
            transfer: Transfer::Sdr,
            full_range: false,
        },
        "what the export says about itself"
    );
    match probed(&out) {
        Some(tags) => {
            eprintln!("PQ source -> mp4: ffprobe {tags:?}");
            let want: Vec<String> = ["tv", "bt709", "bt709", "bt709"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            assert_eq!(tags, want, "range, matrix, transfer, primaries");
            assert_eq!(
                probed_frame(&out).expect("ffprobe is installed"),
                "bt709",
                "what a decoder makes of the coded frame"
            );
        }
        None => eprintln!("no ffprobe: skipping the external read"),
    }
    std::fs::remove_file(&out).unwrap();
}

/// The order, measured on the samples: tone-map first, grade the SDR picture
/// that comes out. Playback renders in that order, so an export that graded the
/// *HDR* codes and tone-mapped afterwards would be a different picture than the
/// canvas showed -- and with a brightness lift on a curve this steep, a very
/// different one, which is what the swapped-order control here measures.
///
/// It is also where "converted once" is checked: the reference below is the tone
/// map and the grade and nothing else, no matrix step, so an export that also
/// ran the BT.2020 matrix over those samples could not land on it.
#[test]
fn an_hdr_clip_is_tone_mapped_before_it_is_graded() {
    let Some(source) = pq_fixture() else {
        eprintln!("no ffmpeg: skipping the tone-map order check");
        return;
    };
    let Some(src) = raw_yuv(&source) else {
        eprintln!("no ffmpeg: skipping the tone-map order check");
        return;
    };
    // A lift, because the curve it is composed with is steepest where the codes
    // are: swap the two and the picture is visibly elsewhere.
    let grade = engine::color::ColorParams {
        brightness: 0.25,
        ..Default::default()
    };
    // Under both the default rendition and a picked one: an export that built
    // its tables from anything but the project's own preset would sit on the
    // wrong reference for the second pass, which is the whole preview-equals-
    // export claim ([`tonemap::Preset`]).
    for preset in [tonemap::Preset::default(), tonemap::Preset::Vivid] {
        let mut session = two_frames(&source);
        assert!(
            session.set_color(engine::project::Lane::V1, 0, Some(grade)),
            "the grade went on the clip"
        );
        assert_eq!(
            session.set_tone(preset),
            preset != tonemap::Preset::default(),
            "picking {preset:?}"
        );
        let out = out_path("hdr_order", "mp4");
        export(&session, &out, Format::Mp4);
        let got = raw_yuv(&out).expect("ffmpeg read the export");

        let (width, height) = (1280usize, 720usize);
        // No declared peak: the fixture's tags say PQ and nothing about how
        // bright it is, so both sides are on the rendition's own number.
        let mapper = ToneMapper::new(tonemap::Transfer::Pq, preset, None);
        // The same bytes twice, through the two orders. Whichever preset is in
        // force, both sides move with it -- what is asserted is which of them
        // the export sits on, not where either lands.
        let both = [true, false].map(|tone_first| {
            let mut planes = src.clone();
            let (y, chroma) = planes.split_at_mut(width * height);
            let (u, v) = chroma.split_at_mut(width * height / 4);
            if tone_first {
                mapper.map(y, u, v, width, height);
                engine::color::apply_yuv(&grade, y, u, v);
            } else {
                engine::color::apply_yuv(&grade, y, u, v);
                mapper.map(y, u, v, width, height);
            }
            planes
        });
        let [ordered, swapped] = both.map(|planes| mean_abs_diff(&got, &planes));
        eprintln!(
            "HDR export ({preset:?}): {ordered:.2} off tone-map-then-grade, {swapped:.2} off the swapped order"
        );
        assert!(
            ordered < 3.0,
            "{preset:?}: the export is {ordered:.2} codes from tone-map-then-grade"
        );
        assert!(
            swapped > 3.0 * ordered,
            "{preset:?}: ordered {ordered:.2} vs swapped {swapped:.2}: the two orders are not far enough apart to have measured anything"
        );
        std::fs::remove_file(&out).unwrap();
    }
}

/// The real thing, where this machine has it: five seconds of a 4K HDR10 film,
/// ten minutes in (the opening two are black and would measure nothing),
/// decoded by the VA-API plugin -- 10-bit HEVC has no software decoder here --
/// and exported. Skipped, loudly, on a machine without the film.
///
/// What is asserted is what the synthetic fixture cannot be: that the tone map
/// survives a real grade of real HDR10 pixels. A picture that came out grey
/// (chroma collapsed onto 128) or crushed to black is what a mistuned or
/// double-applied conversion produces, and both are one mean away.
#[test]
fn a_real_hdr_film_exports_as_a_picture() {
    let film = Path::new(
        "/path/to/a-real-4k-hdr10-film.mkv",
    );
    if !film.exists() {
        eprintln!("skipped: {} is not on this machine", film.display());
        return;
    }
    let mut session = PlaybackSession::open(film).expect("open the film");
    assert_eq!(session.meta().color.transfer, Transfer::Pq, "an HDR10 film");
    // What this film declares about itself, and therefore what the reference
    // rendition converts it at: 1759 cd/m^2 of MaxCLL, which its Matroska never
    // says and its HEVC SEI does. Both the funnel and the export table below
    // build their tables off this same number.
    let (_, demuxer) = Demuxer::open(film).expect("reopen the film");
    let peak = demuxer.light().peak();
    assert_eq!(peak, Some(1759.0), "the film's declared peak");
    let mapper = ToneMapper::new(tonemap::Transfer::Pq, session.tone(), peak);
    assert_eq!(
        mapper.peak(),
        1759.0,
        "the reference rendition converts this film at its own peak"
    );
    // A five-second window out of the middle: cut both ends, drop them, and the
    // delete closes the gap ahead of what is left, so the export is those five
    // seconds and no leading black.
    assert!(session.cut_at(605.0), "the tail cut");
    assert!(session.cut_at(600.0), "the head cut");
    assert!(session.delete_clip(engine::project::Lane::V1, 2), "the tail");
    assert!(session.delete_clip(engine::project::Lane::V1, 0), "the head");
    let out = out_path("hdr_film", "mp4");
    export(&session, &out, Format::Mp4);

    let (meta, _) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(
        meta.color,
        ColorDescription {
            matrix: Matrix::Bt709,
            transfer: Transfer::Sdr,
            full_range: false,
        },
        "a 4K HDR film exported as tagged SDR"
    );
    let Some(frame) = raw_yuv(&out) else {
        eprintln!("no ffmpeg: skipping the pixel half of the film check");
        return;
    };
    let (width, height) = (meta.width as usize, meta.height as usize);
    let (y, chroma) = frame.split_at(width * height);
    let luma = y.iter().map(|s| f64::from(*s)).sum::<f64>() / y.len() as f64;
    let colour = chroma
        .iter()
        .map(|s| f64::from(s.abs_diff(128)))
        .sum::<f64>()
        / chroma.len() as f64;
    eprintln!("5 s of the film at 600 s: mean luma {luma:.1}, mean chroma distance {colour:.1}");
    // Ranges, not anchors: the exposure the tone map is tuned to is a constant
    // in that module and may move. What may not move is that the picture is
    // still a picture -- neither crushed to black nor washed to white, and in
    // colour.
    assert!(
        (25.0..200.0).contains(&luma),
        "mean luma {luma:.1}: that is not a picture"
    );
    assert!(colour > 4.0, "mean chroma distance {colour:.1}: grey");
    std::fs::remove_file(&out).unwrap();
}

/// Preview and export are the same picture, on the one path where they used to
/// differ: a graded clip whose own space is not the file's. Playback grades in
/// the source's space and converts for the screen, so an export that remapped
/// *first* graded different numbers and came out 21.04 mean codes away from what
/// the canvas had shown -- a saturation the user set against one picture,
/// applied to another.
///
/// Measured through this engine's own decode of its own export, because that is
/// the comparison the invariant is about; the ungraded control is what says the
/// number belongs to the grade and not to the encoder.
#[test]
fn a_graded_clip_exports_the_picture_the_preview_showed() {
    for (grade, label, ceiling) in [
        (
            Some(engine::color::ColorParams {
                brightness: 0.0,
                contrast: 1.0,
                saturation: 0.0,
                tint: 0.0,
            }),
            "saturation 0",
            2.0,
        ),
        (None, "ungraded control", 2.0),
    ] {
        let mut session = two_frames(&asset("test_bt601.mp4"));
        assert!(
            session.set_color(engine::project::Lane::V1, 0, grade),
            "{label}: the grade went on the clip"
        );
        let preview = frame0_bgra(&mut session);
        let out = out_path("preview_vs_export", "mp4");
        export(&session, &out, Format::Mp4);

        let mut reopened = PlaybackSession::open(&*out).expect("reopen the export");
        let exported = frame0_bgra(&mut reopened);
        let diff = mean_abs_diff(&preview, &exported);
        eprintln!("{label}: preview vs export {diff:.2} mean codes");
        assert!(
            diff <= ceiling,
            "{label}: the export is {diff:.2} codes from what the preview showed"
        );
        drop(reopened);
        std::fs::remove_file(&out).unwrap();
    }
}

/// Preview and export agree on the film's own peak, not merely on the preset:
/// `test_hdr_bright.mkv` declares 4000 cd/m^2, and the two sides read it in
/// different places -- the decode funnel off the demuxer it already opened, the
/// export table off `source_rate`'s. One of them left on the 1000 an undeclared
/// file is assumed at would be a preview the export does not match, which is
/// exactly the failure this file exists to refuse.
///
/// Measured as everywhere else here: this engine's own decode of its own export
/// against the canvas the preview showed.
#[test]
fn a_declared_peak_exports_the_picture_the_preview_showed() {
    let mut session = two_frames(&asset("test_hdr_bright.mkv"));
    assert_eq!(session.meta().color.transfer, Transfer::Pq, "an HDR fixture");
    let preview = frame0_bgra(&mut session);
    let out = out_path("declared_peak", "mp4");
    export(&session, &out, Format::Mp4);

    let mut reopened = PlaybackSession::open(&*out).expect("reopen the export");
    let exported = frame0_bgra(&mut reopened);
    let diff = mean_abs_diff(&preview, &exported);
    eprintln!("declared peak 4000: preview vs export {diff:.2} mean codes");
    assert!(
        diff <= 2.0,
        "the export is {diff:.2} codes from what the preview showed"
    );
    drop(reopened);
    std::fs::remove_file(&out).unwrap();
}
