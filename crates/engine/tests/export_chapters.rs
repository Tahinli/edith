//! Chapter marks an export carries: an mp4 gets them as a Nero `chpl` and a QuickTime chapter
//! track, a Matroska file as a `Chapters` element. ffprobe, the one reader that is not this
//! project's own, must list the same starts (to the millisecond) and titles, a timeline with
//! no chapters must list none, and the last chapter must run to the end of the file as ffprobe
//! reads it, for a whole timeline and for a ranged export.
//!
//! The sources are made here with ffmpeg in this test's scratch directory, one per test so that
//! tests running in parallel never write or read the same file. Where there is no ffmpeg or
//! ffprobe on the box, the test says so and passes.
//!
//! ```text
//! cargo test -p engine --release --test export_chapters -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::scratch::Scratch;
use engine::{ExportHandle, Project};

/// The frame rate of the generated sources.
const FPS: u32 = 30;

/// Thirty frames: one second of picture, short enough that the software encoder is not the cost.
const FRAMES: u32 = FPS;

/// Ten seconds of picture, for the exports that must end at a film's end well past the first
/// chapter.
const TEN_SECONDS: u32 = 10 * FPS;

/// The chapters as given: `(start in ns, title)`. The second one is not a whole millisecond
/// from the first, so the rounding ffprobe reads back is the one a whole-ms start gives.
const CHAPTERS: [(u64, &str); 3] = [
    (0, "Opening"),
    (300_400_000, "Middle, with ünïcode"),
    (700_000_000, "Closing"),
];

fn pin_software() {
    static PIN: Once = Once::new();
    PIN.call_once(|| {
        engine::envcfg::set("VE_SW", "1");
        engine::envcfg::set("VE_SW_ENC", "1");
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

/// A H.264 and AAC Matroska source of `frames` frames, made for the test called `name` and no
/// other, or `None` when ffmpeg cannot make one here.
fn source(name: &str, frames: u32) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("chapters_source_{name}-{}.mkv", std::process::id()));
    let seconds = format!("{}", f64::from(frames) / f64::from(FPS));
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc=size=320x240:rate={FPS}"))
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
        .args(["-t", &seconds, "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&path)
        .status()
        .ok()?
        .success();
    ok.then_some(path)
}

fn have_ffprobe() -> bool {
    Command::new("ffprobe").arg("-version").output().is_ok()
}

/// `(start in ms, end in ms, title)` per chapter as ffprobe reads them from `path`.
fn ffprobe_chapters(path: &Path) -> Vec<(i64, i64, String)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_chapters", "-of", "default"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe failed on {}", path.display());
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut chapters = Vec::new();
    let mut start = None;
    let mut end = None;
    let mut title = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("start_time=") {
            start = Some((value.parse::<f64>().unwrap() * 1000.0).round() as i64);
        } else if let Some(value) = line.strip_prefix("end_time=") {
            end = Some((value.parse::<f64>().unwrap() * 1000.0).round() as i64);
        } else if let Some(value) = line.strip_prefix("TAG:title=") {
            title = Some(value.to_string());
        } else if line == "[/CHAPTER]" {
            chapters.push((
                start.take().unwrap(),
                end.take().unwrap(),
                title.take().unwrap_or_default(),
            ));
        }
    }
    chapters
}

/// The file's duration in ms as ffprobe's format section reads it.
fn ffprobe_duration_ms(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe failed on {}", path.display());
    let seconds: f64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("ffprobe prints a duration");
    seconds * 1000.0
}

/// What an export wrote, as ffprobe reads it back.
struct Exported {
    chapters: Vec<(i64, i64, String)>,
    duration_ms: f64,
}

/// Exports the `frames`-long `source` to `format` with `chapters` and `range` on the settings,
/// and returns what ffprobe reads back.
fn export_and_read(
    name: &str,
    source: &Path,
    frames: u32,
    format: Format,
    chapters: Vec<(u64, String)>,
    range: Option<(u32, u32)>,
) -> Exported {
    pin_software();
    let (meta, _) = engine::demux::Demuxer::open(source).expect("probe the source");
    let ext = match format {
        Format::Mp4 => "mp4",
        _ => "mkv",
    };
    let out = Scratch::file(&format!("ve_export_chapters_{name}"), ext);
    let handle = engine::export::start(
        Project::single(source, frames),
        meta,
        &out,
        &ExportSettings {
            format,
            chapters,
            range,
            ..Default::default()
        },
        None,
    );
    wait(&handle, Duration::from_secs(900)).expect("the export finishes");
    Exported {
        chapters: ffprobe_chapters(&out),
        duration_ms: ffprobe_duration_ms(&out),
    }
}

fn given() -> Vec<(u64, String)> {
    CHAPTERS.iter().map(|(ns, t)| (*ns, t.to_string())).collect()
}

/// The starts and titles as given, to the millisecond.
fn starts_and_titles(read: &[(i64, i64, String)]) -> Vec<(i64, String)> {
    read.iter().map(|(s, _, t)| (*s, t.clone())).collect()
}

/// Every chapter has a length, each runs to the next one's start, and the last runs to the end
/// of the file ffprobe reports, to within a frame.
fn check_ends(read: &[(i64, i64, String)], duration_ms: f64) {
    for (start, end, title) in read {
        assert!(end > start, "chapter {title:?} has no length: {start} to {end}");
    }
    for pair in read.windows(2) {
        assert_eq!(pair[0].1, pair[1].0, "a chapter ends where the next one starts");
    }
    let (_, last_end, _) = read.last().expect("chapters");
    let frame_ms = 1000.0 / f64::from(FPS);
    assert!(
        (*last_end as f64 - duration_ms).abs() <= frame_ms + 1.0,
        "the last chapter ends at {last_end} ms, but the file is {duration_ms:.1} ms long"
    );
}

fn expected() -> Vec<(i64, String)> {
    CHAPTERS
        .iter()
        .map(|(ns, t)| (((ns + 500_000) / 1_000_000) as i64, t.to_string()))
        .collect()
}

fn have_tools() -> bool {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("no ffmpeg on this box: skipped");
        return false;
    }
    if !have_ffprobe() {
        eprintln!("no ffprobe on this box: skipped");
        return false;
    }
    true
}

#[test]
fn an_mp4_export_lists_its_chapters_in_ffprobe() {
    if !have_tools() {
        return;
    }
    let name = "an_mp4_export_lists_its_chapters_in_ffprobe";
    let Some(src) = source(name, FRAMES) else {
        eprintln!("ffmpeg could not make a source: skipped");
        return;
    };
    let read = export_and_read(name, &src, FRAMES, Format::Mp4, given(), None);
    assert_eq!(starts_and_titles(&read.chapters), expected());
    check_ends(&read.chapters, read.duration_ms);
    assert!((read.duration_ms - 1000.0).abs() <= 40.0, "a one second file");
}

#[test]
fn a_matroska_export_lists_its_chapters_in_ffprobe() {
    if !have_tools() {
        return;
    }
    let name = "a_matroska_export_lists_its_chapters_in_ffprobe";
    let Some(src) = source(name, FRAMES) else {
        eprintln!("ffmpeg could not make a source: skipped");
        return;
    };
    let read = export_and_read(name, &src, FRAMES, Format::Hevc, given(), None);
    assert_eq!(starts_and_titles(&read.chapters), expected());
    check_ends(&read.chapters, read.duration_ms);
    assert!((read.duration_ms - 1000.0).abs() <= 40.0, "a one second file");
}

#[test]
fn an_export_with_no_chapters_lists_none() {
    if !have_tools() {
        return;
    }
    let name = "an_export_with_no_chapters_lists_none";
    let Some(src) = source(name, FRAMES) else {
        eprintln!("ffmpeg could not make a source: skipped");
        return;
    };
    assert!(export_and_read(&format!("{name}_mp4"), &src, FRAMES, Format::Mp4, Vec::new(), None)
        .chapters
        .is_empty());
    assert!(export_and_read(&format!("{name}_mkv"), &src, FRAMES, Format::Hevc, Vec::new(), None)
        .chapters
        .is_empty());
}

/// Ten seconds with chapters at 1, 3 and 6 s: the last chapter must end at the file's end, 10 s.
fn whole_ten_second_export_ends_its_last_chapter_at_the_end(name: &str, format: Format) {
    if !have_tools() {
        return;
    }
    let Some(src) = source(name, TEN_SECONDS) else {
        eprintln!("ffmpeg could not make a source: skipped");
        return;
    };
    let chapters = vec![
        (1_000_000_000, "One".to_string()),
        (3_000_000_000, "Three".to_string()),
        (6_000_000_000, "Six".to_string()),
    ];
    let read = export_and_read(name, &src, TEN_SECONDS, format, chapters, None);
    assert_eq!(
        starts_and_titles(&read.chapters),
        vec![(1000, "One".into()), (3000, "Three".into()), (6000, "Six".into())]
    );
    assert!((read.duration_ms - 10_000.0).abs() <= 40.0, "a ten second file");
    check_ends(&read.chapters, read.duration_ms);
}

#[test]
fn a_ten_second_mp4_export_ends_its_last_chapter_at_the_end() {
    whole_ten_second_export_ends_its_last_chapter_at_the_end(
        "a_ten_second_mp4_export_ends_its_last_chapter_at_the_end",
        Format::Mp4,
    );
}

#[test]
fn a_ten_second_matroska_export_ends_its_last_chapter_at_the_end() {
    whole_ten_second_export_ends_its_last_chapter_at_the_end(
        "a_ten_second_matroska_export_ends_its_last_chapter_at_the_end",
        Format::Hevc,
    );
}

/// Ten seconds exported from 2 s to 7 s. The chapters arrive from the range start, so the ones
/// at 3 s and 6 s of the timeline come in as 1 s and 4 s, and the film is 5 s long.
fn ranged_export_ends_its_last_chapter_at_the_range_end(name: &str, format: Format) {
    if !have_tools() {
        return;
    }
    let Some(src) = source(name, TEN_SECONDS) else {
        eprintln!("ffmpeg could not make a source: skipped");
        return;
    };
    let chapters = vec![
        (1_000_000_000, "Three".to_string()),
        (4_000_000_000, "Six".to_string()),
    ];
    let range = Some((2 * FPS, 7 * FPS));
    let read = export_and_read(name, &src, TEN_SECONDS, format, chapters, range);
    assert_eq!(
        starts_and_titles(&read.chapters),
        vec![(1000, "Three".into()), (4000, "Six".into())]
    );
    assert!((read.duration_ms - 5000.0).abs() <= 40.0, "a five second file");
    check_ends(&read.chapters, read.duration_ms);
}

#[test]
fn a_ranged_mp4_export_ends_its_last_chapter_at_the_range_end() {
    ranged_export_ends_its_last_chapter_at_the_range_end(
        "a_ranged_mp4_export_ends_its_last_chapter_at_the_range_end",
        Format::Mp4,
    );
}

#[test]
fn a_ranged_matroska_export_ends_its_last_chapter_at_the_range_end() {
    ranged_export_ends_its_last_chapter_at_the_range_end(
        "a_ranged_matroska_export_ends_its_last_chapter_at_the_range_end",
        Format::Hevc,
    );
}
