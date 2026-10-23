//! Transition kinds on a real timeline: a red clip then a blue one, 20 frames
//! of overlap, the composed picture at the quarter, middle and three-quarter
//! frames of it, per kind. Silent (`VE_NO_AO`), headless.
//!
//! ```text
//! cargo test -p engine --release --test transitions
//! ```

use std::path::{Path, PathBuf};

use engine::PlaybackSession;
use engine::decode::Frame;
use engine::project::Lane;
use engine::scratch::Scratch;
use engine::transitions::{Dir, TransitionKind};

const W: u32 = 160;
const H: u32 = 90;
/// A is frames 0..60, B 60..120; the transition is A's last 20 frames.
const WINDOW: u32 = 20;
const QUARTER: u32 = 45;
const MIDDLE: u32 = 50;
const LATE: u32 = 55;

fn make(dir: &Path, colour: &str) -> PathBuf {
    let path = dir.join(format!("{colour}.mp4"));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=c={colour}:s=160x90:r=30:d=2"))
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "5"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg makes {colour}.mp4");
    path
}

/// Red then blue on V1, with `kind` over the last `WINDOW` frames of red.
fn session(dir: &Path, kind: TransitionKind) -> PlaybackSession {
    engine::ao::set_silent_output();
    let red = make(dir, "red");
    let blue = make(dir, "blue");
    let mut s = PlaybackSession::open(&red).expect("open red");
    s.import(&blue).expect("import blue");
    s.place_stream_at(2.0, &blue, 0, None).expect("blue after red");
    assert_eq!(s.lane_clips(Lane::V1).len(), 2, "red, then blue");
    assert!(s.set_transition(Lane::V1, 0, kind, WINDOW), "the clips abut");
    s
}

/// What a picture shows at (`fx`, `fy`) as shares of its size: `(r, g, b)`.
fn at(f: &Frame, fx: f32, fy: f32) -> (u8, u8, u8) {
    let x = ((f.width as f32 * fx) as usize).min(f.width as usize - 1);
    let y = ((f.height as f32 * fy) as usize).min(f.height as usize - 1);
    let p = &f.bgra[(y * f.width as usize + x) * 4..];
    (p[2], p[1], p[0])
}

fn red((r, g, b): (u8, u8, u8)) -> bool {
    r > 180 && g < 70 && b < 70
}

fn blue((r, g, b): (u8, u8, u8)) -> bool {
    b > 180 && g < 70 && r < 70
}

fn frame(s: &PlaybackSession, n: u32) -> Frame {
    s.compose_at(n, W, H).expect("compose")
}

#[test]
fn dissolve_mixes_the_two_everywhere() {
    let dir = Scratch::dir("ve_tr_dissolve");
    let s = session(&dir, TransitionKind::Dissolve);
    let (r, _, b) = at(&frame(&s, MIDDLE), 0.5, 0.5);
    assert!(r > 80 && b > 80, "both show: r {r} b {b}");
    let (qr, _, qb) = at(&frame(&s, QUARTER), 0.5, 0.5);
    assert!(qr > qb, "a quarter in is mostly red: {qr} {qb}");
}

#[test]
fn dip_to_black_is_black_in_the_middle_and_the_clips_either_side() {
    let dir = Scratch::dir("ve_tr_dipb");
    let s = session(&dir, TransitionKind::DipBlack);
    let (r, g, b) = at(&frame(&s, MIDDLE), 0.5, 0.5);
    assert!(r.max(g).max(b) < 40, "near black: {r} {g} {b}");
    assert!(red(at(&frame(&s, 30), 0.5, 0.5)), "before the window is plain red");
    assert!(blue(at(&frame(&s, 70), 0.5, 0.5)), "after it is plain blue");
    let q = at(&frame(&s, QUARTER), 0.5, 0.5);
    assert!(q.0 > 60 && q.0 < 200 && q.2 < 40, "a quarter in is red going dark: {q:?}");
}

#[test]
fn dip_to_white_is_white_in_the_middle() {
    let dir = Scratch::dir("ve_tr_dipw");
    let s = session(&dir, TransitionKind::DipWhite);
    let (r, g, b) = at(&frame(&s, MIDDLE), 0.5, 0.5);
    assert!(r.min(g).min(b) > 200, "near white: {r} {g} {b}");
}

#[test]
fn wipes_reveal_blue_from_the_side_they_move_toward() {
    let dir = Scratch::dir("ve_tr_wipe");
    // (direction, a point that is still red at the middle, one that is blue)
    let cases = [
        (Dir::Left, (0.1, 0.5), (0.9, 0.5)),
        (Dir::Right, (0.9, 0.5), (0.1, 0.5)),
        (Dir::Up, (0.5, 0.1), (0.5, 0.9)),
        (Dir::Down, (0.5, 0.9), (0.5, 0.1)),
    ];
    for (d, still_red, now_blue) in cases {
        let s = session(&dir, TransitionKind::Wipe(d));
        let f = frame(&s, MIDDLE);
        assert!(red(at(&f, still_red.0, still_red.1)), "wipe {d:?}: {:?}", at(&f, still_red.0, still_red.1));
        assert!(blue(at(&f, now_blue.0, now_blue.1)), "wipe {d:?}: {:?}", at(&f, now_blue.0, now_blue.1));
    }
    // Wipe-left a quarter in: only the far right is blue; late: only the far left is red.
    let s = session(&dir, TransitionKind::Wipe(Dir::Left));
    let q = frame(&s, QUARTER);
    assert!(red(at(&q, 0.5, 0.5)) && blue(at(&q, 0.95, 0.5)), "quarter");
    let l = frame(&s, LATE);
    assert!(red(at(&l, 0.1, 0.5)) && blue(at(&l, 0.5, 0.5)), "late");
    // The edge is soft: somewhere across the middle row is neither pure red nor pure blue.
    let soft = (0..W)
        .map(|x| at(&frame(&s, MIDDLE), x as f32 / W as f32, 0.5))
        .any(|p| !red(p) && !blue(p));
    assert!(soft, "a soft edge");
}

#[test]
fn slides_and_pushes_bring_blue_in_from_the_side_they_move_from() {
    let dir = Scratch::dir("ve_tr_slide");
    for make_kind in [TransitionKind::Slide, TransitionKind::Push] {
        let cases = [
            (Dir::Left, (0.1, 0.5), (0.9, 0.5)),
            (Dir::Right, (0.9, 0.5), (0.1, 0.5)),
            (Dir::Up, (0.5, 0.1), (0.5, 0.9)),
            (Dir::Down, (0.5, 0.9), (0.5, 0.1)),
        ];
        for (d, still_red, now_blue) in cases {
            let kind = make_kind(d);
            let s = session(&dir, kind);
            let f = frame(&s, MIDDLE);
            assert!(red(at(&f, still_red.0, still_red.1)), "{}: {:?}", kind.name(), at(&f, still_red.0, still_red.1));
            assert!(blue(at(&f, now_blue.0, now_blue.1)), "{}: {:?}", kind.name(), at(&f, now_blue.0, now_blue.1));
        }
    }
}

#[test]
fn zoom_mixes_the_two_and_ends_on_each_clip() {
    let dir = Scratch::dir("ve_tr_zoom");
    let s = session(&dir, TransitionKind::Zoom);
    let (r, _, b) = at(&frame(&s, MIDDLE), 0.5, 0.5);
    assert!(r > 80 && b > 80, "both show: r {r} b {b}");
    assert!(red(at(&frame(&s, 30), 0.5, 0.5)));
    assert!(blue(at(&frame(&s, 70), 0.5, 0.5)));
}

#[test]
fn the_kind_saves_loads_and_undoes() {
    let dir = Scratch::dir("ve_tr_save");
    let mut s = session(&dir, TransitionKind::Push(Dir::Up));
    assert_eq!(s.transition_kind_of(Lane::V1, 0), TransitionKind::Push(Dir::Up));
    let before = frame(&s, MIDDLE).bgra;
    let path = dir.join("t.edith");
    s.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.starts_with("edith 37\n") && text.contains("transition video 1 0 push-up"), "{text}");
    let loaded = PlaybackSession::open_project(&path).expect("load");
    assert_eq!(loaded.transition_kind_of(Lane::V1, 0), TransitionKind::Push(Dir::Up));
    assert_eq!(loaded.transition_out_of(Lane::V1, 0), WINDOW);
    assert!(frame(&loaded, MIDDLE).bgra == before, "the loaded project composes the same picture");

    // Undo puts the old kind back (the kind and the length are one step).
    assert!(s.set_transition(Lane::V1, 0, TransitionKind::Zoom, 10));
    assert_eq!(s.transition_kind_of(Lane::V1, 0), TransitionKind::Zoom);
    assert!(s.undo());
    assert_eq!(s.transition_kind_of(Lane::V1, 0), TransitionKind::Push(Dir::Up));
    assert_eq!(s.transition_out_of(Lane::V1, 0), WINDOW);
    assert!(s.redo());
    assert_eq!(s.transition_kind_of(Lane::V1, 0), TransitionKind::Zoom);

    // A project with only dissolves writes the dialect it always did.
    assert!(s.set_transition(Lane::V1, 0, TransitionKind::Dissolve, WINDOW));
    s.save_project(&path).expect("save again");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(!text.contains("transition video") && !text.starts_with("edith 37"), "{text}");
}

/// A 10-bit solid-colour clip of `colour`, 2 s at 30 fps, encoded by whatever
/// 10-bit-capable encoder ffmpeg has (libx265, else libaom-av1).
fn make_ten_bit(dir: &Path, colour: &str) -> Option<PathBuf> {
    let list = std::process::Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output().ok()?;
    let list = String::from_utf8_lossy(&list.stdout).into_owned();
    let codec: Vec<&str> = if list.contains("libx265") {
        vec!["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=error"]
    } else if list.contains("libaom-av1") {
        vec!["-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "20", "-b:v", "0"]
    } else {
        return None;
    };
    let path = dir.join(format!("{colour}10.mkv"));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=c={colour}:s=160x96:r=30:d=2"))
        .args(["-pix_fmt", "yuv420p10le"])
        .args(codec)
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(path)
}

/// Exports red-then-blue 10-bit with `kind` over 20 frames, software seat, 10-bit
/// output, and returns the middle frame of the overlap as (y, u, v) 16-bit planes.
fn ten_bit_middle(dir: &Path, kind: TransitionKind) -> Option<(Vec<u16>, Vec<u16>, Vec<u16>)> {
    use engine::export::{EncoderSeat, ExportSettings, Format};
    let (red, blue) = (make_ten_bit(dir, "red")?, make_ten_bit(dir, "blue")?);
    engine::ao::set_silent_output();
    let mut s = PlaybackSession::open(&red).expect("open red");
    s.import(&blue).expect("import blue");
    s.place_stream_at(2.0, &blue, 0, None).expect("blue after red");
    assert!(s.set_transition(Lane::V1, 0, kind, WINDOW));
    let out = dir.join(format!("{}.mkv", kind.name()));
    let settings = ExportSettings {
        format: Format::Av1,
        video_bit_depth: Some(10),
        seat: EncoderSeat::Software,
        ..Default::default()
    };
    let handle = s.export_to_with(&out, &settings);
    let started = std::time::Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed().as_secs() < 600, "the export did not finish");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt", "-of", "csv=p=0"])
        .arg(&out)
        .output()
        .expect("ffprobe");
    assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "yuv420p10le", "a 10-bit file");
    let raw = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-vf", &format!("select=eq(n\\,{MIDDLE})"), "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .expect("ffmpeg");
    let words: Vec<u16> = raw.stdout.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
    let (w, h) = (160usize, 96usize);
    assert_eq!(words.len(), w * h * 3 / 2, "one 10-bit frame: chroma planes are half size each way");
    let (y, rest) = words.split_at(w * h);
    let (u, v) = rest.split_at(w * h / 4);
    Some((y.to_vec(), u.to_vec(), v.to_vec()))
}

#[test]
fn ten_bit_export_dips_to_white_and_wipes_in_ten_bit_codes() {
    let dir = Scratch::dir("ve_tr_ten");
    let Some((y, u, v)) = ten_bit_middle(&dir, TransitionKind::DipWhite) else {
        eprintln!("no 10-bit capable ffmpeg encoder: skipping");
        return;
    };
    let near = |x: u16, want: i32, tol: i32| (i32::from(x) - want).abs() <= tol;
    let mean = |p: &[u16]| p.iter().map(|&x| f64::from(x)).sum::<f64>() / p.len() as f64;
    // Frame 50 is the 11th of 20: t = 11/21, so ~4% of blue shows through white.
    assert!(near(mean(&y).round() as u16, 940, 45), "luma {} is 10-bit white", mean(&y));
    assert!(y.iter().all(|&x| x > 800), "every luma sample is near white");
    assert!(near(mean(&u).round() as u16, 512, 40) && near(mean(&v).round() as u16, 512, 40),
        "chroma ~512: u {} v {}", mean(&u), mean(&v));

    let (y, u, v) = ten_bit_middle(&dir, TransitionKind::Wipe(Dir::Left)).expect("encoder found above");
    let (w, h) = (160usize, 96usize);
    let col = |p: &[u16], pw: usize, x0: usize, x1: usize| {
        let mut sum = 0f64;
        let mut n = 0f64;
        for row in p.chunks(pw) {
            for &s in &row[x0..x1] {
                sum += f64::from(s);
                n += 1.0;
            }
        }
        sum / n
    };
    // Red (left, still the outgoing clip) is brighter than blue (right) in luma.
    let (l, r) = (col(&y, w, 8, 56), col(&y, w, 104, 152));
    assert!(l > 250.0 && l < 450.0, "left half is red luma: {l}");
    assert!(r > 60.0 && r < 200.0, "right half is blue luma: {r}");
    // Blue has the high U, red the high V.
    assert!(col(&u, w / 2, 40, 76) > 600.0 && col(&u, w / 2, 4, 28) < 450.0, "u: blue right, red left");
    assert!(col(&v, w / 2, 4, 28) > 600.0 && col(&v, w / 2, 40, 76) < 450.0, "v: red left, blue right");
    assert_eq!(h, 96);
}
