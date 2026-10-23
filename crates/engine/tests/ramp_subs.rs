//! A speed ramp carries the subtitle cues of its clip with the picture: a cue timed at
//! source time `s` is written where the ramp puts source time `s` on the timeline (the
//! inverse of `ramp_source_at`), not where a constant speed would, in Matroska and mp4.
//!
//! `test_av.mp4` is 150 frames at 30 fps; a 1x-to-2x ramp over its first 60 timeline
//! frames covers 90 source frames in those 60, so the file is 90 frames long.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::{Lane, Speed, SpeedKey, ramp_source_at};
use engine::scratch::Scratch;
use engine::subtitle::{self, Cue, SubtitleTrack};
use engine::{ExportHandle, Project};

const MEDIA: &str = "test_av.mp4";
const FPS: f64 = 30.0;
/// One frame, in microseconds, which is what a cue may be out by.
const FRAME_US: i64 = 33_334;
// The tests check cues and tracks, not pictures, so the picture is kept small.
const SMALL: (u32, u32) = (64, 48);

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

fn wait(handle: &ExportHandle) {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(1800), "export hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome").expect("export");
}

fn keys() -> [SpeedKey; 2] {
    [SpeedKey { at: 0, speed: Speed::NORMAL }, SpeedKey { at: 60, speed: Speed::from_permille(2000) }]
}

/// Timeline microseconds at which the ramp reaches source time `us` (bisection).
fn ramped_us(us: i64) -> i64 {
    let want = us as f64 / 1e6 * FPS;
    let (mut lo, mut hi) = (0.0f64, 1000.0f64);
    for _ in 0..80 {
        let mid = (lo + hi) / 2.0;
        if ramp_source_at(&keys(), mid) < want { lo = mid } else { hi = mid }
    }
    ((lo + hi) / 2.0 / FPS * 1e6).round() as i64
}

fn cue(a: i64, b: i64, text: &str) -> Cue {
    Cue { start_us: a, end_us: b, text: text.into(), image: None, words: Vec::new(), karaoke: Vec::new() }
}

fn project(reverse: bool, ramp: bool) -> Project {
    let track = SubtitleTrack {
        path: asset(MEDIA),
        track: Some(2),
        language: "eng".into(),
        name: String::new(),
        label: "eng".into(),
        bitmap: false,
        cues: vec![
            cue(500_000, 1_500_000, "one"),
            cue(2_000_000, 3_250_000, "two"),
            cue(4_000_000, 4_750_000, "three"),
        ],
        refused: None,
    };
    let mut p = Project::single(asset(MEDIA), 150).with_subtitles(vec![track]);
    if ramp {
        p.set_speed_ramp(Lane::V1, 0, &keys()).expect("the ramp sets");
    }
    if reverse {
        p.set_reverse(Lane::V1, 0, true).expect("the clip reverses");
    }
    p
}

fn export(name: &str, format: Format, ext: &str, reverse: bool, ramp: bool) -> Scratch {
    engine::envcfg::set("VE_SW", "1");
    engine::envcfg::set("VE_SW_ENC", "1");
    let out = Scratch::file(&format!("ve_ramp_subs_{name}"), ext);
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe");
    let handle = engine::export::start(
        project(reverse, ramp),
        meta,
        &out,
        &ExportSettings { format, subtitles: vec![0], size: Some(SMALL), ..Default::default() },
        None,
    );
    wait(&handle);
    out
}

fn check(got: &[(i64, i64, String)], reverse: bool, ramp: bool) {
    let want = [(500_000, 1_500_000, "one"), (2_000_000, 3_250_000, "two"), (4_000_000, 4_750_000, "three")];
    assert_eq!(got.len(), want.len(), "cues: {got:?}");
    let mut want = want.to_vec();
    if reverse {
        want.reverse(); // mirrored, so the last cue plays first
    }
    for (g, w) in got.iter().zip(want) {
        // The clip is 5 s of file. Reversed, a cue [s, e] is mirrored to [5 - e, 5 - s]
        // before the ramp (if any) carries it onto the timeline.
        let map = |us: i64| if ramp { ramped_us(us) } else { us };
        let (a, b) = if reverse { (map(5_000_000 - w.1), map(5_000_000 - w.0)) } else { (map(w.0), map(w.1)) };
        assert_eq!(g.2, w.2);
        assert!((g.0 - a).abs() <= FRAME_US, "{} starts {} want {a}", w.2, g.0);
        assert!((g.1 - b).abs() <= FRAME_US, "{} ends {} want {b}", w.2, g.1);
        // And it is not where a constant 1x would leave it.
        if ramp && !reverse && w.0 >= 2_000_000 {
            assert!((g.0 - w.0).abs() > 4 * FRAME_US, "{} unmapped", w.2);
        }
    }
}

fn srt_times(path: &Path) -> Vec<(i64, i64, String)> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:s:0", "-f", "srt", "-"])
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let t = |s: &str| {
        let (hms, ms) = s.trim().split_once(',').unwrap();
        let p: Vec<i64> = hms.split(':').map(|x| x.parse().unwrap()).collect();
        ((p[0] * 3600 + p[1] * 60 + p[2]) * 1000 + ms.parse::<i64>().unwrap()) * 1000
    };
    text.split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(|b| {
            let mut l = b.lines();
            l.next();
            let (a, e) = l.next().unwrap().split_once("-->").unwrap();
            (t(a), t(e), l.collect::<Vec<_>>().join("\n"))
        })
        .collect()
}

fn matroska(name: &str, reverse: bool, ramp: bool) {
    let out = export(name, Format::Hevc, "mkv", reverse, ramp);
    let tracks = subtitle::of_matroska(&out).expect("read back");
    assert_eq!(tracks.len(), 1);
    let got: Vec<_> = tracks[0].cues.iter().map(|c| (c.start_us, c.end_us, c.text.clone())).collect();
    check(&got, reverse, ramp);
}

fn mp4(name: &str, reverse: bool, ramp: bool) {
    let out = export(name, Format::Mp4, "mp4", reverse, ramp);
    check(&srt_times(&out), reverse, ramp);
}

#[test]
fn a_ramped_clips_cues_follow_the_ramp_in_matroska() {
    matroska("ramp_mkv", false, true);
}

#[test]
fn a_ramped_clips_cues_follow_the_ramp_in_mp4() {
    mp4("ramp_mp4", false, true);
}

#[test]
fn a_reversed_clips_cues_are_mirrored_in_matroska() {
    matroska("rev_mkv", true, false);
}

#[test]
fn a_reversed_clips_cues_are_mirrored_in_mp4() {
    mp4("rev_mp4", true, false);
}

#[test]
fn a_reversed_ramped_clips_cues_are_mirrored_then_ramped() {
    matroska("revramp_mkv", true, true);
    mp4("revramp_mp4", true, true);
}

/// A cue's per-word times go through the same ramp as the cue, each word where its own
/// sound lands; on a reversed clip the words are dropped and the cue shows whole.
#[test]
fn a_cues_words_follow_the_ramp_and_drop_on_a_reversed_clip() {
    let words = vec![
        (2_000_000, 2_500_000, "a".to_string()),
        (2_500_000, 3_000_000, "b".to_string()),
        (3_000_000, 3_250_000, "c".to_string()),
    ];
    let with_words = |reverse: bool| {
        let mut p = project(reverse, true);
        let mut track = p.subtitles()[0].clone();
        track.cues[1].words = words.clone();
        p = p.with_subtitles(vec![track.clone()]);
        engine::export::timeline_cues(&p, &track, FPS)
    };
    let cues = with_words(false);
    let two = cues.iter().find(|c| c.text == "two").expect("cue two");
    assert_eq!(two.words.len(), 3, "{:?}", two.words);
    for ((s, e, w), (gs, ge, gw)) in words.iter().zip(&two.words) {
        assert_eq!(w, gw);
        assert!((gs - ramped_us(*s)).abs() <= FRAME_US, "{w} starts {gs} want {}", ramped_us(*s));
        assert!((ge - ramped_us(*e)).abs() <= FRAME_US, "{w} ends {ge} want {}", ramped_us(*e));
    }
    let reversed = with_words(true);
    let two = reversed.iter().find(|c| c.text == "two").expect("cue two");
    assert!(two.words.is_empty(), "a reversed clip keeps no words: {:?}", two.words);
}
