//! Text subtitles that ffmpeg writes, read back through edith's own subtitle
//! call sites: an SRT, a WebVTT and an ASS muxed into Matroska with `-c:s copy`
//! (or the matching codec), and a WebVTT written beside the media as a sidecar.
//!
//! The source of every fixture is one of the hand-written files in `tests/data`,
//! so the cues the test expects are the ones those files state: start and end
//! in microseconds, and the text with its markup resolved. The fixtures are
//! generated at test time into a scratch directory, never checked in.
//!
//! Skipped loudly when ffmpeg is not on the box, the way the rest of the suite
//! skips its reference tools.
//!
//! ```text
//! cargo test -p engine --release --test text_subs_ffmpeg -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::scratch::Scratch;
use engine::subtitle::{self, Cue};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn s(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 path")
}

/// Runs ffmpeg with `args` and fails the test with its stderr when it refuses.
fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The three cues `data/test_subs.srt` and `data/test_subs.ass` both state, with
/// the markup resolved: the italics of the `.ass` third line are not text, and
/// the `\N` of its second one is a line break like the `.srt`'s.
fn expected() -> Vec<Cue> {
    vec![
        Cue {
            start_us: 500_000,
            end_us: 1_500_000,
            text: "first line".into(),
            image: None,
            words: Vec::new(),
            karaoke: Vec::new(),
        },
        Cue {
            start_us: 2_000_000,
            end_us: 3_250_000,
            text: "second line\nwith a break".into(),
            image: None,
            words: Vec::new(),
            karaoke: Vec::new(),
        },
        Cue {
            start_us: 4_000_000,
            end_us: 4_750_000,
            text: "third line".into(),
            image: None,
            words: Vec::new(),
            karaoke: Vec::new(),
        },
    ]
}

#[test]
fn an_srt_muxed_into_matroska_reads_back_its_cues_and_timing() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to mux the SRT with");
        return;
    }
    let src = data("test_subs.srt");
    let mkv = Scratch::file("ve_srt_in_mkv", "mkv");
    ffmpeg(&["-i", s(&src), "-c:s", "subrip", s(&mkv)]);

    let tracks = subtitle::of_matroska(&mkv).expect("the mkv is walked");
    assert_eq!(tracks.len(), 1, "one subtitle track");
    let track = &tracks[0];
    assert_eq!(track.refused, None, "the SRT track is readable");
    assert!(!track.is_bitmap(), "an SRT is text");
    assert_eq!(track.cues, expected(), "the cues and their timing survive");

    // The door a saved line comes back through reaches the same track.
    let one = subtitle::open(&mkv, track.track);
    assert_eq!(one.cues, expected());
}

#[test]
fn a_webvtt_sidecar_written_by_ffmpeg_reads_the_same_cues() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to write the WebVTT with");
        return;
    }
    let src = data("test_subs.srt");
    let vtt = Scratch::file("ve_sidecar", "vtt");
    ffmpeg(&["-i", s(&src), s(&vtt)]);

    let track = subtitle::open(&vtt, None);
    assert_eq!(track.refused, None, "the WebVTT parses");
    assert_eq!(track.cues, expected(), "the sidecar's cues and timing");
}

#[test]
fn a_webvtt_muxed_into_matroska_reads_back_its_cues() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to mux the WebVTT with");
        return;
    }
    let src = data("test_subs.srt");
    let mkv = Scratch::file("ve_vtt_in_mkv", "mkv");
    ffmpeg(&["-i", s(&src), "-c:s", "webvtt", s(&mkv)]);

    let tracks = subtitle::of_matroska(&mkv).expect("the mkv is walked");
    assert_eq!(tracks.len(), 1, "one subtitle track");
    let track = &tracks[0];
    assert_eq!(track.refused, None, "the WebVTT track is readable");
    assert_eq!(track.cues, expected(), "the cues and timing survive");
}

#[test]
fn an_ass_muxed_into_matroska_keeps_its_timing_and_resolves_its_markup() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to mux the ASS with");
        return;
    }
    let src = data("test_subs.ass");
    let mkv = Scratch::file("ve_ass_in_mkv", "mkv");
    ffmpeg(&["-i", s(&src), "-c:s", "copy", s(&mkv)]);

    let tracks = subtitle::of_matroska(&mkv).expect("the mkv is walked");
    assert_eq!(tracks.len(), 1, "one subtitle track");
    let track = &tracks[0];
    assert_eq!(track.refused, None, "the ASS track is readable");
    assert!(!track.is_bitmap(), "an ASS is text");
    assert_eq!(
        track.cues,
        expected(),
        "the override tags are resolved on the way out of the mkv"
    );
}
