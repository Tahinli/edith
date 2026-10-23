//! Embedded closed captions (CEA-608 CC1) come in as subtitle tracks, like a container's own.
//!
//! Synthetic clips only: ffmpeg encodes a 160x96 testsrc with B-frames, `ec_cc::synth` splices
//! cc_data with known CC1 text into the coded pictures (SEI for H.264, user data for MPEG-2),
//! and ffmpeg only re-wraps the bytes (mkv, mp4, ts). The engine must list a `CC1 (608)` track
//! and import it with the right texts and the times of the picture's own timeline.
//! Skipped without ffmpeg unless EC_REQUIRE_FIXTURES=1.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_cc::synth::*;
use ec_cc::{CcTriplet, TimedCc, decode_all};
use engine::demux::captions::{self, TRACK_BASE};
use engine::subtitle::{self, SubtitleTrack};

const FRAMES: usize = 90;
/// Millisecond container ticks and a 90 kHz clock, rounded.
const SLACK_US: i64 = 2_000;

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok_and(|o| o.status.success())
}

fn require() -> bool {
    if have("ffmpeg") && have("ffprobe") {
        return true;
    }
    assert!(std::env::var("EC_REQUIRE_FIXTURES").is_err(), "ffmpeg/ffprobe required");
    eprintln!("skipping: no ffmpeg");
    false
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("edith-cc-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn caption_frames() -> Vec<Vec<CcTriplet>> {
    let mut s = Script608::new();
    s.rcl().enm().pac(14, 4, false).text("HELLO").pac(15, 4, false).text("WORLD").eoc();
    s.idle(14);
    s.edm().idle(2);
    s.rcl().enm().pac(15, 4, false).text("SECOND CUE").eoc();
    s.idle(10);
    s.edm();
    schedule(FRAMES, 3, &s.pairs)
}

fn run(c: &mut Command) {
    assert!(c.status().unwrap().success(), "{c:?}");
}

/// Encode, return the raw elementary stream and the decode-order display indices.
fn encode(codec: &[&str], fmt: &str, bsf: Option<&str>, name: &str) -> (PathBuf, Vec<usize>) {
    let mkv = tmp(&format!("{name}.mkv"));
    run(Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=160x96:rate=30"])
        .args(["-frames:v", &FRAMES.to_string()])
        .args(codec)
        .arg(&mkv));
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts", "-of", "csv=p=0"])
        .arg(&mkv)
        .output()
        .unwrap();
    let pts: Vec<i64> =
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect();
    assert_eq!(pts.len(), FRAMES);
    let mut sorted = pts.clone();
    sorted.sort();
    let idx = pts.iter().map(|p| sorted.iter().position(|q| q == p).unwrap()).collect();
    let es = tmp(&format!("{name}.es"));
    let mut c = Command::new("ffmpeg");
    c.args(["-v", "error", "-y", "-i"]).arg(&mkv).args(["-c:v", "copy", "-an"]);
    if let Some(b) = bsf {
        c.args(["-bsf:v", b]);
    }
    run(c.args(["-f", fmt]).arg(&es));
    (es, idx)
}

/// The elementary stream in `out`'s container. A bare stream carries no presentation order for
/// B-frames (ffmpeg stamps its packets as if decode order were display order), so the real one --
/// `idx`, the display index of each packet in decode order -- is stamped on with `setts`, with
/// the dts two pictures behind, the way an encoder writes it. Through an mp4 first, because ffmpeg
/// gives a bare stream no timestamps a Matroska or ts writer takes.
fn wrap(es: &Path, input_fmt: &str, idx: &[usize], out: &str) -> PathBuf {
    let table: Vec<String> = idx.iter().enumerate().map(|(n, i)| format!("eq(N,{n})*{i}")).collect();
    let pts = format!("({})/30/TB", table.join("+"));
    let dts = "(N-2)/30/TB";
    let mp4 = tmp(&format!("{out}.step.mp4"));
    run(Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", input_fmt, "-framerate", "30", "-i"])
        .arg(es)
        .args(["-c", "copy", "-bsf:v"])
        .arg(format!("setts=pts='{pts}':dts='{dts}'"))
        .arg(&mp4));
    if out.ends_with(".mp4") {
        return mp4;
    }
    let path = tmp(out);
    run(Command::new("ffmpeg").args(["-v", "error", "-y", "-i"]).arg(&mp4).args(["-c", "copy"]).arg(&path));
    path
}

/// What the first CC1 cues must be, from the same triplets decoded in display order.
fn expected(caps: &[Vec<CcTriplet>]) -> Vec<(i64, i64, String)> {
    let timed: Vec<TimedCc> = caps
        .iter()
        .enumerate()
        .map(|(i, t)| TimedCc { time_us: i as i64 * 1_000_000 / 30, triplets: t.clone() })
        .collect();
    decode_all(&timed)
        .cea608
        .iter()
        .filter(|c| c.channel == 1)
        .map(|c| (c.cue.start_us, c.cue.end_us, ec_subs::plain_text(&c.cue.segments)))
        .collect()
}

fn check(path: &Path, caps: &[Vec<CcTriplet>]) {
    let want = expected(caps);
    assert_eq!(want.len(), 2);
    assert_eq!(want[0].2, "HELLO\nWORLD");
    assert_eq!(want[1].2, "SECOND CUE");
    // The cheap presence probe lists the track without cues.
    let present = captions::present(path);
    assert_eq!(present.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["CC1 (608)"]);
    // The import door.
    let tracks: Vec<SubtitleTrack> = engine::PlaybackSession::parse_subtitles(path).expect("imports");
    assert_eq!(tracks.len(), 1, "{:?}", tracks.iter().map(|t| &t.label).collect::<Vec<_>>());
    let t = &tracks[0];
    assert_eq!(t.track, Some(TRACK_BASE + 1));
    assert_eq!(t.label, "CC1 (608)");
    assert!(t.refused.is_none() && !t.bitmap);
    assert_eq!(t.cues.len(), want.len(), "{:?}", t.cues);
    for (c, (s, e, text)) in t.cues.iter().zip(&want) {
        assert_eq!(&c.text, text);
        assert!((c.start_us - s).abs() <= SLACK_US, "start {} vs {s}", c.start_us);
        assert!((c.end_us - e).abs() <= SLACK_US, "end {} vs {e}", c.end_us);
    }
    // A saved row names the track by number and finds it again, without the other tracks.
    let again = subtitle::open(path, Some(TRACK_BASE + 1));
    assert!(again.refused.is_none(), "{:?}", again.refused);
    assert_eq!(again.cues, t.cues);
}

fn h264(wrapper: &str, input_fmt: &str) {
    if !require() {
        return;
    }
    let (es, idx) = encode(
        &["-c:v", "libx264", "-bf", "2", "-g", "30", "-x264-params", "bframes=2:b-adapt=0"],
        "h264",
        Some("h264_mp4toannexb"),
        &format!("h264-{wrapper}"),
    );
    let caps = caption_frames();
    let order: Vec<_> = idx.iter().map(|&i| caps[i].clone()).collect();
    let with = insert_sei(&std::fs::read(&es).unwrap(), false, &order);
    std::fs::write(&es, with).unwrap();
    let file = wrap(&es, input_fmt, &idx, &format!("cc-h264.{wrapper}"));
    check(&file, &caps);
}

#[test]
fn h264_captions_in_matroska() {
    h264("mkv", "h264");
}

#[test]
fn h264_captions_in_mp4() {
    h264("mp4", "h264");
}

#[test]
fn h264_captions_in_transport_stream() {
    h264("ts", "h264");
}

#[test]
fn mpeg2_captions_in_transport_stream() {
    if !require() {
        return;
    }
    let (es, idx) = encode(&["-c:v", "mpeg2video", "-bf", "2", "-g", "12"], "mpeg2video", None, "m2v");
    let caps = caption_frames();
    let order: Vec<_> = idx.iter().map(|&i| caps[i].clone()).collect();
    let with = insert_mpeg2_user_data(&std::fs::read(&es).unwrap(), &order, false);
    std::fs::write(&es, with).unwrap();
    let file = wrap(&es, "mpegvideo", &idx, "cc-mpeg2.ts");
    check(&file, &caps);
}

#[test]
fn a_clip_without_captions_lists_none() {
    if !require() {
        return;
    }
    let (es, idx) = encode(&["-c:v", "libx264", "-bf", "2"], "h264", Some("h264_mp4toannexb"), "plain");
    let file = wrap(&es, "h264", &idx, "plain.mp4");
    assert!(captions::present(&file).is_empty());
    assert!(captions::tracks(&file).is_empty());
    assert!(subtitle::of_media(&file).unwrap().is_empty());
}
