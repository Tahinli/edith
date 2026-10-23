//! Still-picture exports and numbered runs: a PNG sequence, a poster frame, an animated GIF, and
//! a folder of numbered PNGs imported (and opened) as one clip.
//!
//! ```text
//! cargo test -p engine --test stills -- --test-threads=1 --nocapture
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format, GifSettings};
use engine::project::Lane;
use engine::scratch::Scratch;
use engine::{ExportHandle, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < limit, "export did not finish in {limit:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// Frame `k` of a generated run: a horizontal ramp, a vertical one, and a level that climbs with `k`.
fn ramp(k: u32, width: u32, height: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            px.extend_from_slice(&[(x * 255 / width) as u8, (k * 20) as u8, (y * 255 / height) as u8]);
        }
    }
    px
}

/// A numbered run of ten PNGs at 1280x720, written with the codec's own encoder.
fn ten_frames(dir: &std::path::Path) -> PathBuf {
    let (w, h) = (1280, 720);
    for k in 1..=10u32 {
        let bytes = ec_image::png_encode::encode_rgb8(w, h, &ramp(k, w, h)).expect("encode");
        std::fs::write(dir.join(format!("frame_{k:04}.png")), bytes).expect("write");
    }
    dir.join("frame_0004.png")
}

#[test]
fn a_png_sequence_and_a_still_are_pictures_at_project_size() {
    let session = PlaybackSession::open(asset("test_25fps.mp4")).expect("open the fixture");
    let (project, meta) = session.export_snapshot();
    let dir = Scratch::dir("ve_stills_sequence");
    let out = dir.join("shot");
    let settings = ExportSettings {
        format: Format::PngSequence,
        range: Some((0, 50)), // two seconds at 25 fps
        ..ExportSettings::default()
    };
    wait(&engine::export::start(project.clone(), meta, &out, &settings, None), Duration::from_secs(300))
        .expect("the sequence");

    let mut names: Vec<String> = std::fs::read_dir(&out)
        .expect("the folder exists")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 50, "one picture per frame: {names:?}");
    assert_eq!(names[0], "shot_00001.png");
    assert_eq!(names[49], "shot_00050.png");
    let first = ec_image::open(out.join("shot_00001.png")).expect("a PNG");
    assert_eq!(first.dimensions(), (meta.width, meta.height), "the project's own size");

    let poster = Scratch::file("ve_stills_poster", "png");
    let still = ExportSettings { format: Format::Still, range: Some((25, 26)), ..ExportSettings::default() };
    wait(&engine::export::start(project, meta, &poster, &still, None), Duration::from_secs(300))
        .expect("the still");
    let img = ec_image::open(&poster).expect("a PNG");
    assert_eq!(img.dimensions(), (meta.width, meta.height));
    assert!(!poster.with_extension("png.part").exists(), "no temporary left behind");
}

#[test]
fn a_gif_of_a_second_is_fifteen_frames_at_480_wide_and_a_wide_one_is_refused() {
    let session = PlaybackSession::open(asset("test_25fps.mp4")).expect("open the fixture");
    let (project, meta) = session.export_snapshot();
    let out = Scratch::file("ve_stills_gif", "gif");
    let settings = ExportSettings {
        format: Format::Gif,
        range: Some((0, 25)),
        gif: GifSettings::default(),
        ..ExportSettings::default()
    };
    wait(&engine::export::start(project.clone(), meta, &out, &settings, None), Duration::from_secs(300))
        .expect("the GIF");
    let bytes = std::fs::read(&out).expect("the GIF file");
    let frames = ec_image::gif::decode_animation(&bytes, ec_image::Limits::default()).expect("a GIF that decodes");
    assert_eq!(frames.len(), 15, "one second at 15 frames a second");
    assert_eq!(frames[0].image.dimensions().0, 480, "the default width");

    let refused = Scratch::file("ve_stills_gif_refused", "gif");
    let wide = ExportSettings {
        format: Format::Gif,
        range: Some((0, 25)),
        gif: GifSettings { width: 1280, ..GifSettings::default() },
        ..ExportSettings::default()
    };
    let err = wait(&engine::export::start(project, meta, &refused, &wide, None), Duration::from_secs(60))
        .expect_err("too wide");
    assert!(err.to_string().contains("pixels wide"), "{err}");
    assert!(!refused.exists());
}

#[test]
fn a_numbered_run_opens_and_imports_as_one_clip_of_its_frames() {
    let dir = Scratch::dir("ve_stills_run_import");
    let one = ten_frames(&dir);
    let spelled = dir.join("frame_[0001-0010].png");

    // Opened as the timeline's first file.
    let opened = PlaybackSession::open(&one).expect("a numbered run opens");
    assert_eq!(opened.sources()[0].path, spelled, "the run is the source, not one frame of it");
    assert_eq!(opened.lane_clips(Lane::V1).first().expect("placed on V1").len(), 10, "ten frames");

    // Imported into a timeline that already has footage.
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    let before = session.sources().len();
    let source = session.import(&one).expect("a numbered run imports");
    assert_eq!(session.sources().len(), before + 1, "one source for the whole run");
    assert_eq!(session.sources()[source].path, spelled);
    assert_eq!(session.file_frames(&spelled), 10, "ten frames, a clip of ten frames");
    assert_eq!(session.import(&dir.join("frame_0007.png")).expect("again"), source, "the same source");
    let end = session.timeline_duration();
    assert!(session.place_stream_at(end, &spelled, 0, Some(Lane::V1)).expect("placed"));
    let placed = session.lane_clips(Lane::V1).last().expect("the run's clip").clone();
    assert_eq!(placed.len(), 10, "the clip is ten frames long");
}
