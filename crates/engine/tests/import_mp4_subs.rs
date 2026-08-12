//! An mp4's subtitles come back *in*: what `mux` writes as `tx3g` timed text,
//! `subtitle::of_mp4` reads, so the default export format round-trips -- a file
//! edith wrote is a file edith opens with its subtitle tracks still on it.
//!
//! Two witnesses again, the other way round from `export_subs.rs`: our own
//! writer's file read by our own reader (the round trip), and a file **ffmpeg**
//! muxed read by the same reader -- so what is pinned here is the format and
//! not this project agreeing with itself.
//!
//! ```text
//! cargo test -p engine --release --test import_mp4_subs -- --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::scratch::Scratch;
use engine::subtitle::{self, SubtitleTrack};
use engine::{ExportHandle, Project};

/// The media fixture: five seconds at 30 fps, a picture this box decodes and
/// encodes with nothing installed.
const MEDIA: &str = "test_av.mp4";
const FRAMES: u32 = 150;
/// What a cue may be out by: the track's own tick, a millisecond.
const SLACK_US: i64 = 1_000;

/// The three cues both fixtures carry, and the times an export puts them at.
const WANT: [(i64, i64, &str); 3] = [
    (500_000, 1_500_000, "first line"),
    (2_000_000, 3_250_000, "second line\nwith a break"),
    (4_000_000, 4_750_000, "third line"),
];

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
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

fn near(got: i64, want: i64, what: &str) {
    assert!(
        (got - want).abs() <= SLACK_US,
        "{what}: {got} us is not within a millisecond of {want} us"
    );
}

/// The two tracks `assets/test_subs.mkv` carries -- `eng` as `S_TEXT/UTF8` and
/// `fra` titled `Signs` as `S_TEXT/ASS`, the same three cues twice -- repointed
/// at the media this box can encode, exactly as `export_subs.rs` does it.
fn both_tracks() -> Vec<SubtitleTrack> {
    let mut tracks = subtitle::of_matroska(&asset("test_subs.mkv")).expect("read the fixture");
    assert_eq!(
        tracks.len(),
        2,
        "the fixture carries two tracks: {tracks:?}"
    );
    for track in &mut tracks {
        track.path = asset(MEDIA);
    }
    tracks
}

/// An mp4 export of both tracks, on disk.
fn exported_mp4(name: &str) -> Scratch {
    pin_software();
    let out = Scratch::file(&format!("ve_import_mp4_subs_{name}"), "mp4");
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let handle = engine::export::start(
        Project::single(asset(MEDIA), FRAMES).with_subtitles(both_tracks()),
        meta,
        &out,
        &ExportSettings {
            format: Format::Mp4,
            subtitles: vec![0, 1],
            ..Default::default()
        },
    );
    wait(&handle, Duration::from_secs(300)).expect("the export finishes");
    out
}

fn check_pair(tracks: &[SubtitleTrack], what: &str) {
    for track in tracks {
        println!(
            "{what}: {} track {:?} {:?}",
            track.label,
            track.track,
            track
                .cues
                .iter()
                .map(|c| (c.start_us, c.end_us, &c.text))
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(tracks.len(), 2, "{what}: two tracks: {tracks:?}");
    // The language and the title, apart: `eng` is a language alone, `Signs` is
    // a French track's own name -- the pair `mdhd` and `trak/udta/name` hold.
    assert_eq!(tracks[0].label, "eng", "{what}");
    assert_eq!(tracks[1].label, "fra — Signs", "{what}");
    for track in tracks {
        assert_eq!(track.refused, None, "{what}: the track reads");
        assert!(!track.is_bitmap(), "{what}: timed text is not pictures");
        assert_eq!(
            track.cues.len(),
            3,
            "{what}: the empty samples between the cues are not cues: {:?}",
            track.cues
        );
        for (cue, (start, end, text)) in track.cues.iter().zip(WANT) {
            near(cue.start_us, start, &format!("{what}: cue start"));
            near(cue.end_us, end, &format!("{what}: cue end"));
            assert_eq!(cue.text, text, "{what}: the words travel, breaks and all");
            assert!(cue.image.is_none(), "{what}: text, not a picture");
        }
    }
}

/// The round trip: two tracks out as `tx3g`, the same two back in -- languages,
/// titles, words and times.
#[test]
fn an_mp4_export_comes_back_with_the_subtitles_it_left_with() {
    let out = exported_mp4("round");
    let tracks = subtitle::of_mp4(&out).expect("read our own mp4 back");
    println!(
        "our mp4 reads back as: {:?}",
        tracks
            .iter()
            .map(|t| (&t.label, t.track, t.cues.len()))
            .collect::<Vec<_>>()
    );
    check_pair(&tracks, "our own writer");
    // The picture is track 1 and the sound 2, so the text is 3 and 4 -- the
    // numbers a `.edith` row names, which is what makes the row openable.
    assert_eq!(
        tracks.iter().map(|t| t.track).collect::<Vec<_>>(),
        vec![Some(3), Some(4)]
    );

    // ...and through the door a saved project comes back in by, which is the
    // point of the numbers above.
    let second = subtitle::open(&out, Some(4));
    assert_eq!(second.refused, None, "the saved row opens: {second:?}");
    assert_eq!(second.label, "fra — Signs");
    assert_eq!(second.cues.len(), 3);
    std::fs::remove_file(&out).unwrap();
}

/// The same file read by the same door the *import button* goes through, which
/// is what makes this reachable rather than merely written.
#[test]
fn the_import_door_walks_an_mp4_for_its_subtitles() {
    let out = exported_mp4("import");
    let tracks =
        engine::PlaybackSession::parse_subtitles(&out).expect("the import door walks the mp4");
    check_pair(&tracks, "the import door");
    std::fs::remove_file(&out).unwrap();
}

/// A file with no timed-text track: no tracks, no refusal, nothing invented.
#[test]
fn an_mp4_without_subtitles_reads_as_no_tracks_at_all() {
    let tracks = subtitle::of_mp4(&asset(MEDIA)).expect("the fixture opens");
    assert!(tracks.is_empty(), "nothing was invented: {tracks:?}");
    let raw = engine::demux::mp4_subtitles(&asset(MEDIA)).expect("the walk itself");
    assert!(raw.is_empty(), "{raw:?}");
}

/// The other implementation's file: what `ffmpeg -c:s mov_text` muxes, read by
/// this reader. Skipped where there is no ffmpeg on the box -- the test is not
/// about this machine's packages.
#[test]
fn an_ffmpeg_muxed_mov_text_track_reads_the_same_way() {
    let out = Scratch::file("ve_import_mp4_subs_ffmpeg", "mp4");
    let made = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(asset("test_subs.mkv"))
        .args([
            "-map", "0:v", "-map", "0:s", "-c:v", "copy", "-c:s", "mov_text",
        ])
        .arg(&*out)
        .status();
    let Ok(status) = made else {
        println!("no ffmpeg on this box: the round trip is the only witness");
        return;
    };
    assert!(status.success(), "ffmpeg refused to mux the fixture");

    let tracks = subtitle::of_mp4(&out).expect("read ffmpeg's mp4");
    println!(
        "ffmpeg's mp4 reads back as: {:?}",
        tracks
            .iter()
            .map(|t| (&t.label, t.track, t.cues.len()))
            .collect::<Vec<_>>()
    );
    // ffmpeg's timed-text tracks are timed in *microseconds* where ours are in
    // milliseconds, and it writes the same `trak/udta/name` for the title: the
    // two things a reader of one muxer's quirks would get wrong.
    check_pair(&tracks, "ffmpeg's muxer");
    let raw = engine::demux::mp4_subtitles(&out).expect("the walk itself");
    assert_eq!(
        raw[1].language, "fra",
        "the language is the file's: {raw:?}"
    );
    assert_eq!(raw[1].name, "Signs", "...and the title is beside it");
    std::fs::remove_file(&out).unwrap();
}
