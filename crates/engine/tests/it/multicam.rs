//! Multicam clips through the session: cameras synced by the sound they share (a tone burst heard
//! 1.3 s later by one of them), the angle switched at the playhead, the picture the render shows
//! being the active angle's own, flattening, the file (v46) and undo.
//!
//! ```text
//! cargo test -p engine --release --test multicam
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::project::{Angle, Lane};
use engine::scratch::Scratch;
use engine::{AngleSync, MulticamSync, PlaybackSession};

/// A 5 s clip of `pattern` whose sound is a quiet hum and a 0.6 s tone burst at `burst` seconds
/// (and a second, shorter one 2.5 s later, so the match is not a single event).
fn camera(dir: &Path, name: &str, pattern: &str, burst: f64) -> PathBuf {
    let out = dir.join(name);
    let sound = format!(
        "aevalsrc='0.02*sin(2*PI*220*t)+if(between(t,{burst},{}),0.5*sin(2*PI*1000*t),0)+if(between(t,{},{}),0.4*sin(2*PI*600*t),0)':s=44100:d=5",
        burst + 0.6,
        burst + 2.5,
        burst + 2.8
    );
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("{pattern}=s=320x180:r=25:d=5"), "-f", "lavfi", "-i", &sound])
        .args(["-ac", "2", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "20", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&out)
        .status()
        .expect("ffmpeg runs")
        .success();
    assert!(ok, "ffmpeg made {name}");
    out
}

/// Two cameras, the second hearing the burst 1.3 s after the first: a session with both imported.
fn rig(dir: &Path) -> (PlaybackSession, PathBuf, PathBuf) {
    let a = camera(dir, "cam_a.mp4", "testsrc2", 1.0);
    let b = camera(dir, "cam_b.mp4", "smptebars", 2.3);
    let mut s = PlaybackSession::open(&a).expect("open");
    s.set_gain(0.0);
    assert_eq!(s.import(&b).expect("import"), 1);
    (s, a, b)
}

fn clips(s: &PlaybackSession, lane: Lane) -> Vec<(u32, u32, u32, usize, Angle)> {
    s.lane_clips(lane).iter().map(|c| (c.start, c.in_frame, c.out_frame, c.source, c.angle)).collect()
}

#[test]
fn the_sync_finds_the_delay_of_a_tone_burst() {
    let dir = Scratch::dir("ve_mcam_sync");
    let (mut s, ..) = rig(&dir);
    let r = s.make_multicam_at(&[0, 1], MulticamSync::Audio, Some("Take"), Lane::V1, 0.0).expect("multicam");
    assert_eq!(r.how[0], AngleSync::Reference);
    assert!(matches!(r.how[1], AngleSync::Audio { score } if score > 0.8), "{:?}", r.how);
    assert!((r.offsets_secs[1] - 1.3).abs() < 0.04, "{:?}", r.offsets_secs);
    // The camera that heard the burst later started earlier: it sits at the start, the other 1.3 s in.
    assert_eq!(r.starts[1], 0);
    assert!((i64::from(r.starts[0]) - 33).abs() <= 1, "{:?}", r.starts);
    assert_eq!(s.lane_clips(Lane::V1).len(), 1);
    assert_eq!(s.sequence_name(r.made.sequence).as_deref(), Some("Take"));
    assert!(s.undo());
    assert_eq!(s.sequences().len(), 1, "one undo takes the whole multicam back");
    assert!(!s.lane_clips(Lane::V1).iter().any(|c| engine::project::is_sequence_source(c.source)));
}

#[test]
fn start_sync_lines_the_cameras_up_at_the_start() {
    let dir = Scratch::dir("ve_mcam_start");
    let (mut s, ..) = rig(&dir);
    let r = s.make_multicam_at(&[0, 1], MulticamSync::Start, None, Lane::V1, 0.0).expect("multicam");
    assert_eq!(r.starts, vec![0, 0]);
    assert_eq!(r.how, vec![AngleSync::Start; 2]);
    assert!(s.make_multicam_at(&[0, 0], MulticamSync::Start, None, Lane::V1, 0.0).is_err(), "one camera twice");
    assert!(s.make_multicam_at(&[0], MulticamSync::Start, None, Lane::V1, 0.0).is_err(), "one camera");
}

#[test]
fn the_render_shows_the_active_angle_and_a_switch_is_one_undo_step() {
    let dir = Scratch::dir("ve_mcam_switch");
    let (mut s, a, b) = rig(&dir);
    let only_a = PlaybackSession::open(&a).expect("open a");
    only_a.set_gain(0.0);
    let only_b = PlaybackSession::open(&b).expect("open b");
    only_b.set_gain(0.0);
    let made = s.make_multicam_at(&[0, 1], MulticamSync::Audio, None, Lane::V1, 0.0).expect("multicam");
    // Camera A joins about 1.3 s in: timeline frame 40 shows its frame 40 - start.
    let start_a = made.starts[0];
    let first = s.compose_at(80, 160, 90).expect("frame");
    let pre = s.undo_depth();
    let touched = s.switch_angle_at(None, 2.0, Some(1), None).expect("switch");
    assert_eq!(touched.len(), 2, "the picture and its sound");
    assert_eq!(s.undo_depth(), pre + 1, "one undo step");
    assert_eq!(s.lane_clips(Lane::V1).len(), 2);
    assert_eq!(s.lane_clips(Lane::A1).len(), 2);
    assert_eq!(s.lane_clips(Lane::V1)[1].angle, Angle { video: 1, audio: None });
    // Before the cut camera A's frame, after it camera B's -- by pixels, against single-camera sessions.
    let before = s.compose_at(40, 160, 90).expect("frame");
    let ref_a = only_a.compose_at(40 - start_a, 160, 90).expect("frame");
    assert_eq!(before.bgra, ref_a.bgra, "frame 40 is camera A's own");
    let after = s.compose_at(80, 160, 90).expect("frame");
    let ref_b = only_b.compose_at(80, 160, 90).expect("frame");
    assert_eq!(after.bgra, ref_b.bgra, "frame 80 is camera B's own");
    assert_ne!(first.bgra, after.bgra, "and the switch changed what frame 80 shows");
    assert!(s.undo());
    assert_eq!(s.lane_clips(Lane::V1).len(), 1);
    assert_eq!(s.compose_at(80, 160, 90).expect("frame").bgra, first.bgra);
    assert!(s.redo());
    assert_eq!(s.compose_at(80, 160, 90).expect("frame").bgra, after.bgra);
}

#[test]
fn flattening_keeps_the_render_and_leaves_plain_clips() {
    let dir = Scratch::dir("ve_mcam_flatten");
    let (mut s, ..) = rig(&dir);
    s.make_multicam_at(&[0, 1], MulticamSync::Audio, None, Lane::V1, 0.0).expect("multicam");
    s.switch_angle_at(None, 2.0, Some(1), None).expect("switch");
    s.switch_angle_at(None, 3.0, Some(0), Some(Some(1))).expect("switch back, sound pinned to B");
    let frames = [0u32, 20, 40, 52, 60, 74, 75, 90, 110, 130];
    let want: Vec<Vec<u8>> = frames.iter().map(|&f| s.compose_at(f, 160, 90).expect("frame").bgra).collect();
    let (p, _) = s.export_snapshot();
    let fps = s.meta().frame_rate;
    let sound = p.audio_segments_from(0, fps);
    s.flatten_multicam(Lane::V1, 0).expect("flatten");
    assert!(s.lane_clips(Lane::V1).iter().chain(s.lane_clips(Lane::A1)).all(|c| !engine::project::is_sequence_source(c.source)));
    assert_eq!(s.lane_clips(Lane::V1).len(), 3);
    for (f, w) in frames.iter().zip(&want) {
        assert_eq!(&s.compose_at(*f, 160, 90).expect("frame").bgra, w, "frame {f} after flatten");
    }
    let (p, _) = s.export_snapshot();
    assert_eq!(p.audio_segments_from(0, fps), sound, "and the sound is the same segments");
    assert!(s.flatten_multicam(Lane::V1, 0).is_err(), "nothing multicam left");
    assert!(s.undo());
    assert!(s.lane_clips(Lane::V1).iter().all(|c| engine::project::is_sequence_source(c.source)));
}

#[test]
fn a_multicam_project_round_trips_as_v46_and_a_plain_one_stays_v45() {
    let dir = Scratch::dir("ve_mcam_file");
    let (mut s, ..) = rig(&dir);
    // Several sequences but no multicam: still the v45 dialect.
    s.new_sequence(Some("Spare")).expect("new");
    let plain = dir.join("plain.edith");
    s.save_project(&plain).expect("save");
    assert!(std::fs::read_to_string(&plain).expect("read").starts_with("edith 45\n"));
    let r = s.make_multicam_at(&[0, 1], MulticamSync::Audio, Some("Take"), Lane::V1, 0.0).expect("multicam");
    s.switch_angle_at(None, 2.0, Some(1), None).expect("switch");
    s.switch_angle_at(None, 3.0, Some(0), Some(Some(1))).expect("switch");
    let path = dir.join("m.edith");
    s.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.starts_with("edith 47\n"), "{}", text.lines().next().unwrap_or(""));
    assert!(text.contains(&format!("\nmcam {} cam_a\n", r.made.sequence)) && text.contains("\nangle video 1 1 2 -\n"), "{text}");
    let back = PlaybackSession::open_project(&path).expect("reopen");
    back.set_gain(0.0);
    assert_eq!(clips(&back, Lane::V1), clips(&s, Lane::V1));
    assert_eq!(clips(&back, Lane::A1), clips(&s, Lane::A1));
    assert_eq!(back.angle_names(r.made.sequence), s.angle_names(r.made.sequence));
    for f in [10u32, 60, 100] {
        assert_eq!(back.compose_at(f, 160, 90).expect("frame").bgra, s.compose_at(f, 160, 90).expect("frame").bgra, "frame {f}");
    }
    // A v45 reader's file with an angle line, and a bad angle, are refused by name.
    let old = text.replacen("edith 47", "edith 46", 1);
    let p45 = dir.join("v45.edith");
    std::fs::write(&p45, old).expect("write");
    assert!(PlaybackSession::open_project(&p45).is_err(), "a v45 file has no angle lines");
    let bad = text.replace("angle video 1 1 2 -", "angle video 1 1 12 -");
    let pbad = dir.join("bad.edith");
    std::fs::write(&pbad, bad).expect("write");
    let err = PlaybackSession::open_project(&pbad).err().expect("refused").to_string();
    assert!(err.contains("angle"), "{err}");
    // Reopening and saving again writes the same bytes.
    let again = dir.join("m2.edith");
    back.save_project(&again).expect("save");
    assert_eq!(std::fs::read_to_string(&again).expect("read"), text);
}
