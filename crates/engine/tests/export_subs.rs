//! Subtitles leave with the file: a Matroska export carries the picked track as
//! `S_TEXT/UTF8`, timed on the *exported* timeline -- so a cut moves the cues
//! with the pictures they belong to -- and an mp4 says it cannot rather than
//! dropping them in silence.
//!
//! Two witnesses for every cue: this project's own Matroska reader
//! (`demux::matroska_subtitles`, the way an exported file comes back into a
//! project) and, where the box has it, ffmpeg -- which is what "it plays
//! elsewhere" means and the only thing that can say this file is not merely
//! self-consistent.
//!
//! ```text
//! cargo test -p engine --release --test export_subs -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::Lane;
use engine::subtitle::{self, Cue, SubtitleTrack};
use engine::{ExportHandle, Project};

/// The media fixture: five seconds at 30 fps, and a picture this box can decode
/// with nothing installed.
const MEDIA: &str = "test_av.mp4";
const FRAMES: u32 = 150;
/// What a cue may be out by: the file's own tick, a millisecond. The mapping is
/// exact microseconds -- only the write rounds -- so nothing wider than this is
/// being allowed for.
const SLACK_US: i64 = 1_000;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn out_path(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("ve_subs_export_{name}_{}.mkv", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
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
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// The three cues the subtitle fixtures hold (`tests/subtitles.rs` pins them
/// against the real `.srt`, `.ass` and `.mkv`), on a timeline of the media they
/// are timed against.
///
/// The track says it came out of *this* media file, which is what an embedded
/// track is and what makes its cues the file's own clock rather than the
/// timeline's. Written out here rather than read out of `test_subs.mkv` because
/// that fixture's picture is AV1 and there is no software AV1 decoder to export
/// it through; the cues are the same three either way.
fn track() -> SubtitleTrack {
    SubtitleTrack {
        path: asset(MEDIA),
        track: Some(2),
        label: "eng".into(),
        cues: vec![
            Cue {
                start_us: 500_000,
                end_us: 1_500_000,
                text: "first line".into(),
            },
            Cue {
                start_us: 2_000_000,
                end_us: 3_250_000,
                text: "second line\nwith a break".into(),
            },
            Cue {
                start_us: 4_000_000,
                end_us: 4_750_000,
                text: "third line".into(),
            },
        ],
        refused: None,
    }
}

fn project() -> Project {
    Project::single(asset(MEDIA), FRAMES).with_subtitles(vec![track()])
}

/// Exports `project` as an HEVC Matroska carrying subtitle track `pick`, and
/// hands back what our own reader finds in the file.
fn exported(name: &str, project: Project, pick: Option<usize>) -> (PathBuf, Vec<Cue>) {
    pin_software();
    let out = out_path(name);
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let handle = engine::export::start(
        project,
        meta,
        &out,
        &ExportSettings {
            format: Format::Hevc,
            subtitles: pick,
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(300)).expect("the export finishes");
    let tracks = subtitle::of_matroska(&out).expect("read the export back");
    match pick {
        None => {
            assert!(tracks.is_empty(), "no pick, no track: {tracks:?}");
            (out, Vec::new())
        }
        Some(_) => {
            assert_eq!(tracks.len(), 1, "exactly the picked track: {tracks:?}");
            assert_eq!(tracks[0].refused, None, "the track reads back as text");
            (out, tracks[0].cues.clone())
        }
    }
}

/// What ffmpeg makes of the same file's subtitle track, as an SRT on stdout:
/// a second implementation reading the bytes, which is the whole point of it.
/// `None` where there is no ffmpeg on the box -- the test is not about this
/// machine's packages.
fn ffmpeg_srt(path: &Path) -> Option<String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:s:0", "-f", "srt", "-"])
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "ffmpeg refused the exported subtitles: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn near(got: i64, want: i64, what: &str) {
    assert!(
        (got - want).abs() <= SLACK_US,
        "{what}: {got} us is not within a frame of {want} us"
    );
}

/// The whole timeline, untouched: the file comes back with the very cues the
/// source's track holds, at the times it holds them -- and ffmpeg reads the
/// same three lines out of it.
#[test]
fn an_mkv_export_carries_the_picked_subtitle_track() {
    let (out, cues) = exported("plain", project(), Some(0));
    assert_eq!(cues.len(), 3, "three cues: {cues:?}");
    let want = [
        (500_000, 1_500_000, "first line"),
        (2_000_000, 3_250_000, "second line\nwith a break"),
        (4_000_000, 4_750_000, "third line"),
    ];
    for (cue, (start, end, text)) in cues.iter().zip(want) {
        near(cue.start_us, start, "cue start");
        near(cue.end_us, end, "cue end");
        assert_eq!(cue.text, text, "the text travels, line breaks and all");
    }
    // ...and the track is named the way it was named in the source, so the
    // subtitle menu of whatever plays this says `eng` too.
    let tracks = subtitle::of_matroska(&out).expect("read back");
    assert_eq!(tracks[0].label, "eng");
    let raw = engine::demux::matroska_subtitles(&out).expect("walk the tracks element");
    assert_eq!(raw[0].codec, "S_TEXT/UTF8");
    assert_eq!(raw[0].language, "eng", "the label went in as a language");

    match ffmpeg_srt(&out) {
        Some(srt) => {
            println!("ffmpeg reads the exported track back as:\n{srt}");
            assert!(srt.contains("first line"), "{srt}");
            assert!(srt.contains("with a break"), "{srt}");
            assert!(srt.contains("00:00:00,500 --> 00:00:01,500"), "{srt}");
            assert!(srt.contains("00:00:04,000 --> 00:00:04,750"), "{srt}");
        }
        None => println!("no ffmpeg on this box: our own reader is the only witness"),
    }
    std::fs::remove_file(&out).unwrap();
}

/// A cut timeline: `[0.5s, 2.5s)` rippled out of it, which is one cue gone
/// entirely, one that straddles the far edge and keeps the half still there,
/// and one that simply moves back with the picture.
#[test]
fn a_cut_moves_the_cues_with_the_pictures() {
    let mut project = project();
    assert!(project.ripple_delete(15, 60), "cut 0.5s..2.5s out");
    assert_eq!(project.timeline_frames(), 90, "three seconds are left");
    assert_eq!(
        project.lane(Lane::V1).len(),
        2,
        "the hole closed, over two placements of the one source"
    );

    let (out, cues) = exported("cut", project, Some(0));
    assert_eq!(
        cues.len(),
        2,
        "the cue inside the cut is gone, the other two are not: {cues:?}"
    );
    // 2.0s..3.25s of the source, of which 2.5s on survived, landing where the
    // cut left it: 0.5s..1.25s.
    near(
        cues[0].start_us,
        500_000,
        "the straddling cue starts at the cut",
    );
    near(
        cues[0].end_us,
        1_250_000,
        "...and keeps the rest of its length",
    );
    assert_eq!(cues[0].text, "second line\nwith a break");
    // 4.0s..4.75s of the source, two seconds back.
    near(
        cues[1].start_us,
        2_000_000,
        "the last cue moves back by the cut",
    );
    near(cues[1].end_us, 2_750_000, "...end with it");
    assert_eq!(cues[1].text, "third line");

    if let Some(srt) = ffmpeg_srt(&out) {
        println!("ffmpeg reads the cut export back as:\n{srt}");
        assert!(
            !srt.contains("first line"),
            "the cut cue is not there: {srt}"
        );
        assert!(srt.contains("00:00:00,500 --> 00:00:01,250"), "{srt}");
        assert!(srt.contains("00:00:02,000 --> 00:00:02,750"), "{srt}");
        assert!(srt.contains("third line"), "{srt}");
    }
    std::fs::remove_file(&out).unwrap();
}

/// Nothing picked is nothing written -- the file every export of a timeline
/// without subtitles has always been.
#[test]
fn an_export_with_no_pick_carries_no_subtitle_track() {
    // `exported` asserts the file has no subtitle track at all; this is the
    // pick being absent rather than the timeline having nothing to pick.
    let (out, cues) = exported("none", project(), None);
    assert_eq!(cues, Vec::new());
    std::fs::remove_file(&out).unwrap();
}

/// What the card says before the button is pressed, which is where an mp4's
/// refusal has to land: the file is not the place to find out.
#[test]
fn the_plan_says_what_each_container_does_with_them() {
    let project = project();
    let plan = |format| engine::export::planned_subtitles(&project, format, Some(0));
    assert_eq!(plan(Format::Hevc), "eng → embedded");
    assert_eq!(plan(Format::Av1), "eng → embedded");
    assert_eq!(plan(Format::Mp4), "mkv only — an mp4 carries none");
    assert_eq!(plan(Format::HevcMp4), "mkv only — an mp4 carries none");
    assert_eq!(plan(Format::Wav), "none — this format is the sound alone");
    // No pick, and a pick nothing answers, are both "none" rather than a panic
    // or a lie.
    assert_eq!(
        engine::export::planned_subtitles(&project, Format::Hevc, None),
        "none"
    );
    assert_eq!(
        engine::export::planned_subtitles(&project, Format::Hevc, Some(9)),
        "none"
    );
    // A timeline that never had subtitles says the same thing.
    let bare = Project::single(asset(MEDIA), FRAMES);
    assert_eq!(
        engine::export::planned_subtitles(&bare, Format::Hevc, Some(0)),
        "none"
    );
}
