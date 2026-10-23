//! Keyframes on opacity, colour and volume, and the easings on every key type.
//! Headless (`VE_NO_AO`), through the same session API the preview and the
//! MCP use. Run with `cargo test --release -p engine --test anim_keys`.

use std::path::{Path, PathBuf};

use engine::PlaybackSession;
use engine::color::{ColorKey, ColorParams};
use engine::project::{GainKey, Lane};
use engine::transform::{KeyEase, OpacityKey, TransformKey, TransformParams};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn open(path: impl AsRef<Path>) -> PlaybackSession {
    engine::ao::set_silent_output();
    PlaybackSession::open(path).expect("open")
}

/// `test_av.mp4` is 1280x720, 30 fps, 150 frames.
const W: u32 = 320;
const H: u32 = 180;

/// The mean of the green-ish brightness of a BGRA frame, a cheap luma.
fn mean_luma(bgra: &[u8]) -> f64 {
    let (mut sum, mut n) = (0.0, 0.0);
    for px in bgra.as_chunks::<4>().0 {
        sum += 0.114 * f64::from(px[0]) + 0.587 * f64::from(px[1]) + 0.299 * f64::from(px[2]);
        n += 1.0;
    }
    sum / n
}

fn luma_at(s: &PlaybackSession, frame: u32) -> f64 {
    mean_luma(&s.compose_at(frame, W, H).expect("compose").bgra)
}

fn bright(brightness: f32) -> ColorParams {
    ColorParams { brightness, ..ColorParams::default() }
}

fn ckey(at: u32, brightness: f32, ease: KeyEase) -> ColorKey {
    ColorKey { at, params: bright(brightness), ease }
}

fn okey(at: u32, opacity: f32, ease: KeyEase) -> OpacityKey {
    OpacityKey { at, opacity, ease }
}

fn near(a: f64, b: f64, tol: f64, what: &str) {
    assert!((a - b).abs() <= tol, "{what}: {a} vs {b} (tol {tol})");
}

#[test]
fn colour_keys_grade_each_frame_like_a_constant_grade_of_the_interpolated_value() {
    // Brightness -0.5 -> 0.5 over source frames 0..100, per easing: the frame at
    // 25 is graded exactly as a constant brightness of the eased value is.
    for (ease, shaped) in [
        (KeyEase::Linear, 0.25),
        (KeyEase::EaseIn, 0.015625),
        (KeyEase::EaseOut, 0.578125),
        (KeyEase::Ease, 0.15625),
        (KeyEase::Hold, 0.0),
    ] {
        let mut keyed = open(asset("test_av.mp4"));
        assert!(keyed.set_color_keys(Lane::V1, 0, vec![ckey(0, -0.5, ease), ckey(100, 0.5, ease)]));
        let mut fixed = open(asset("test_av.mp4"));
        assert!(fixed.set_color(Lane::V1, 0, Some(bright(-0.5 + shaped))));
        let (k, f) = (luma_at(&keyed, 25), luma_at(&fixed, 25));
        near(k, f, 1.0, &format!("{ease:?} at frame 25"));
    }
}

#[test]
fn colour_keys_are_exact_at_their_keys_and_held_outside() {
    let mut keyed = open(asset("test_av.mp4"));
    assert!(keyed.set_color_keys(
        Lane::V1,
        0,
        vec![ckey(20, -0.5, KeyEase::Linear), ckey(80, 0.5, KeyEase::Linear)]
    ));
    let at = |b: f32| {
        let mut s = open(asset("test_av.mp4"));
        assert!(s.set_color(Lane::V1, 0, Some(bright(b))));
        s
    };
    near(luma_at(&keyed, 20), luma_at(&at(-0.5), 20), 0.6, "first key");
    near(luma_at(&keyed, 80), luma_at(&at(0.5), 80), 0.6, "last key");
    near(luma_at(&keyed, 5), luma_at(&at(-0.5), 5), 0.6, "held before");
    near(luma_at(&keyed, 120), luma_at(&at(0.5), 120), 0.6, "held after");
    // And the picture really brightens between them.
    assert!(luma_at(&keyed, 70) > luma_at(&keyed, 30) + 10.0);
}

#[test]
fn opacity_keys_multiply_with_the_fade() {
    // Opacity 1.0 -> 0.5 over 0..100, with a 20 frame fade-in: at frame 10 the
    // clip is at 0.95 opacity and half faded, so it shows at 0.475.
    let mut keyed = open(asset("test_av.mp4"));
    assert!(keyed.set_opacity_keys(
        Lane::V1,
        0,
        vec![okey(0, 1.0, KeyEase::Linear), okey(100, 0.5, KeyEase::Linear)]
    ));
    assert!(keyed.set_fade_in(Lane::V1, 0, 20));
    let mut fixed = open(asset("test_av.mp4"));
    assert!(fixed.set_transform(
        Lane::V1,
        0,
        Some(TransformParams { opacity: 0.475, ..TransformParams::default() })
    ));
    let (k, f) = (luma_at(&keyed, 10), luma_at(&fixed, 10));
    near(k, f, 1.5, "opacity times fade");
    // Neither alone: the fade without opacity keys is brighter, the opacity
    // without the fade is brighter still.
    let mut fade_only = open(asset("test_av.mp4"));
    assert!(fade_only.set_fade_in(Lane::V1, 0, 20));
    assert!(luma_at(&fade_only, 10) > k + 1.0, "the opacity key darkens it further");
    let mut opacity_only = open(asset("test_av.mp4"));
    assert!(opacity_only.set_opacity_keys(
        Lane::V1,
        0,
        vec![okey(0, 1.0, KeyEase::Linear), okey(100, 0.5, KeyEase::Linear)]
    ));
    assert!(luma_at(&opacity_only, 10) > k + 1.0, "the fade darkens it further");
}

#[test]
fn opacity_keys_follow_the_curve_between_keys() {
    let mut keyed = open(asset("test_av.mp4"));
    assert!(keyed.set_opacity_keys(
        Lane::V1,
        0,
        vec![okey(0, 0.0, KeyEase::Ease), okey(100, 1.0, KeyEase::Ease)]
    ));
    let full = luma_at(&open(asset("test_av.mp4")), 50);
    let mid = luma_at(&keyed, 50);
    let quarter = luma_at(&keyed, 25);
    near(mid / full, 0.5, 0.03, "midpoint of an S-curve");
    near(quarter / full, 0.15625, 0.03, "quarter point of an S-curve is under a quarter");
}

#[test]
fn key_setters_are_one_undo_step_and_refuse_what_they_must() {
    let mut s = open(asset("test_av.mp4"));
    let o = vec![okey(0, 0.0, KeyEase::Linear), okey(50, 1.0, KeyEase::Hold)];
    let c = vec![ckey(0, 0.0, KeyEase::EaseIn), ckey(50, 0.5, KeyEase::Linear)];
    assert!(s.set_opacity_keys(Lane::V1, 0, o.clone()));
    assert!(!s.set_opacity_keys(Lane::V1, 0, o.clone()), "already so");
    assert!(s.set_color_keys(Lane::V1, 0, c.clone()));
    assert_eq!(s.opacity_keys_of(Lane::V1, 0), o.as_slice());
    assert_eq!(s.color_keys_of(Lane::V1, 0), c.as_slice());
    assert!(s.undo());
    assert!(s.color_keys_of(Lane::V1, 0).is_empty(), "one undo takes the colour keys off");
    assert_eq!(s.opacity_keys_of(Lane::V1, 0), o.as_slice(), "and leaves the opacity keys");
    assert!(s.redo());
    assert_eq!(s.color_keys_of(Lane::V1, 0), c.as_slice());
    assert!(s.undo() && s.undo());
    assert!(s.opacity_keys_of(Lane::V1, 0).is_empty());

    // Refusals.
    let a1 = Lane::A1;
    assert!(!s.set_opacity_keys(a1, 0, o.clone()), "audio lane");
    assert!(!s.set_color_keys(a1, 0, c.clone()), "audio lane");
    assert!(!s.set_opacity_keys(Lane::V1, 0, vec![okey(5, 0.0, KeyEase::Linear), okey(5, 1.0, KeyEase::Linear)]));
    assert!(!s.set_color_keys(Lane::V1, 0, vec![ckey(5, f32::NAN, KeyEase::Linear)]));
    // Out-of-range values are clamped.
    assert!(s.set_opacity_keys(Lane::V1, 0, vec![okey(0, 4.0, KeyEase::Linear)]));
    assert_eq!(s.opacity_keys_of(Lane::V1, 0)[0].opacity, 1.0);
    assert!(s.set_color_keys(Lane::V1, 0, vec![ckey(0, 9.0, KeyEase::Linear)]));
    assert_eq!(s.color_keys_of(Lane::V1, 0)[0].params.brightness, 1.0);
}

#[test]
fn a_split_divides_opacity_and_colour_keys_and_a_trim_keeps_them_on_the_footage() {
    let mut s = open(asset("test_av.mp4"));
    assert!(s.set_opacity_keys(
        Lane::V1,
        0,
        vec![okey(0, 0.0, KeyEase::Linear), okey(120, 1.0, KeyEase::Linear)]
    ));
    assert!(s.set_color_keys(
        Lane::V1,
        0,
        vec![ckey(0, -0.5, KeyEase::Linear), ckey(120, 0.5, KeyEase::Linear)]
    ));
    assert!(s.cut_at(2.0), "a cut at frame 60");
    let head = s.opacity_keys_of(Lane::V1, 0).to_vec();
    let tail = s.opacity_keys_of(Lane::V1, 1).to_vec();
    assert_eq!(head.last().map(|k| k.at), Some(60));
    assert_eq!(tail.first().map(|k| k.at), Some(60));
    assert_eq!(head.last().unwrap().opacity, 0.5);
    assert_eq!(tail.first().unwrap().opacity, 0.5);
    assert_eq!(s.color_keys_of(Lane::V1, 1).first().map(|k| k.at), Some(60));
    assert!((s.color_keys_of(Lane::V1, 0).last().unwrap().params.brightness).abs() < 1e-6);
}

#[test]
fn eased_gain_and_placement_keys_round_trip_and_older_files_keep_their_version() {
    let dir = std::env::temp_dir().join(format!("anim_keys_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("keys.edith");

    let mut s = open(asset("test_av.mp4"));
    let t = vec![
        TransformKey {
            at: 0,
            params: TransformParams { scale: 1.0, ..TransformParams::default() },
            ease: KeyEase::EaseOut,
        },
        TransformKey {
            at: 100,
            params: TransformParams { scale: 1.3, opacity: 0.6, ..TransformParams::default() },
            ease: KeyEase::Hold,
        },
    ];
    let o = vec![okey(10, 0.0, KeyEase::EaseIn), okey(90, 1.0, KeyEase::Ease), okey(120, 0.25, KeyEase::Linear)];
    let c = vec![
        ColorKey {
            at: 0,
            params: ColorParams { brightness: -0.25, contrast: 1.5, saturation: 0.5, tint: 0.125, ..Default::default() },
            ease: KeyEase::Hold,
        },
        ckey(100, 0.75, KeyEase::Linear),
    ];
    let a1 = Lane::A1;
    let g = vec![
        GainKey { at: 0, db: 0.0, ease: KeyEase::EaseOut },
        GainKey { at: 60, db: -12.5, ease: KeyEase::Linear },
        GainKey { at: 90, db: -3.0, ease: KeyEase::Hold },
    ];
    assert!(s.set_transform_keys(Lane::V1, 0, t.clone()));
    assert!(s.set_opacity_keys(Lane::V1, 0, o.clone()));
    assert!(s.set_color_keys(Lane::V1, 0, c.clone()));
    assert!(s.set_gain_keys(a1, 0, g.clone()));
    s.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.starts_with("edith 39\n"), "new keys are v39:\n{text}");
    for line in ["tkeys video 1 0 ", "okeys video 1 0 ", "ckeys video 1 0 ", "gainkeys audio 1 0 "] {
        assert!(text.contains(line), "{line}:\n{text}");
    }
    let back = PlaybackSession::open_project(&path).expect("reopen");
    assert_eq!(back.transform_keys_of(Lane::V1, 0), t.as_slice());
    assert_eq!(back.opacity_keys_of(Lane::V1, 0), o.as_slice());
    assert_eq!(back.color_keys_of(Lane::V1, 0), c.as_slice());
    assert_eq!(back.gain_keys_of(a1, 0), g.as_slice());
    // Saving what came back is the same file.
    let again = dir.join("again.edith");
    back.save_project(&again).expect("save again");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(&again).unwrap(),
        "a round trip is byte-stable"
    );

    // Linear and `ease` keys alone are the dialects they always were.
    let mut old = open(asset("test_av.mp4"));
    assert!(old.set_transform_keys(
        Lane::V1,
        0,
        vec![
            TransformKey { at: 0, params: TransformParams::default(), ease: KeyEase::Ease },
            TransformKey { at: 50, params: TransformParams { scale: 2.0, ..TransformParams::default() }, ease: KeyEase::Linear },
        ]
    ));
    assert!(old.set_gain_keys(a1, 0, vec![GainKey::new(0, 0.0), GainKey::new(30, -6.0)]));
    old.save_project(&path).expect("save old");
    let text = std::fs::read_to_string(&path).expect("read old");
    assert!(text.starts_with("edith 32\n"), "linear and ease keys are v32:\n{text}");
    assert!(text.contains("gainkeys audio 1 0 0:0 30:-6\n"), "linear gain points keep their bytes:\n{text}");
    assert!(text.contains(":ease:") && text.contains(":lin:"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_stopwatch_adds_a_key_that_holds_the_picture_and_removes_it_again() {
    use engine::project::KeyKind;
    let mut s = open(asset("test_av.mp4"));
    // An animated clip shows its interpolated value; a key at 50 holds that value.
    assert!(s.set_color_keys(Lane::V1, 0, vec![ckey(0, -0.5, KeyEase::Linear), ckey(100, 0.5, KeyEase::Linear)]));
    let before = luma_at(&s, 50);
    assert_eq!(s.toggle_key(Lane::V1, 0, KeyKind::Color, 50), Some(true));
    assert_eq!(s.color_keys_of(Lane::V1, 0).len(), 3);
    near(s.color_keys_of(Lane::V1, 0)[1].params.brightness as f64, 0.0, 1e-6, "the held value");
    near(luma_at(&s, 50), before, 0.5, "adding a key does not move the picture");
    assert_eq!(s.key_ease_at(Lane::V1, 0, KeyKind::Color, 50), Some(KeyEase::Linear));
    assert_eq!(s.key_ease_at(Lane::V1, 0, KeyKind::Color, 51), None, "only on the key's own frame");

    // The ease of that key, then undo of each step.
    assert!(s.set_key_ease_at(Lane::V1, 0, KeyKind::Color, 50, KeyEase::Hold));
    assert!(!s.set_key_ease_at(Lane::V1, 0, KeyKind::Color, 50, KeyEase::Hold), "already so");
    assert!(!s.set_key_ease_at(Lane::V1, 0, KeyKind::Color, 51, KeyEase::Hold), "no key there");
    assert_eq!(s.key_ease_at(Lane::V1, 0, KeyKind::Color, 50), Some(KeyEase::Hold));
    assert!(s.undo());
    assert_eq!(s.key_ease_at(Lane::V1, 0, KeyKind::Color, 50), Some(KeyEase::Linear));

    assert_eq!(s.toggle_key(Lane::V1, 0, KeyKind::Color, 50), Some(false), "again removes it");
    assert_eq!(s.color_keys_of(Lane::V1, 0).len(), 2);
    assert!(s.undo());
    assert_eq!(s.color_keys_of(Lane::V1, 0).len(), 3, "the removal is one undo step");

    // A clip with no keys gets its first one from its single value; opacity and placement likewise.
    assert!(s.is_keyed(Lane::V1, 0, KeyKind::Color));
    assert!(!s.is_keyed(Lane::V1, 0, KeyKind::Opacity));
    assert_eq!(s.toggle_key(Lane::V1, 0, KeyKind::Opacity, 10), Some(true));
    assert_eq!(s.opacity_keys_of(Lane::V1, 0)[0].opacity, 1.0);
    assert_eq!(s.toggle_key(Lane::V1, 0, KeyKind::Placement, 10), Some(true));
    assert_eq!(s.transform_keys_of(Lane::V1, 0)[0].params, TransformParams::default());
    assert_eq!(s.toggle_key(Lane::V1, 0, KeyKind::Placement, 10), Some(false));
    assert!(!s.is_keyed(Lane::V1, 0, KeyKind::Placement), "the last key off leaves the clip unkeyed");
    // Outside the clip, and on an audio lane, nothing is keyed.
    assert_eq!(s.toggle_key(Lane::V1, 0, KeyKind::Opacity, 100_000), None);
    assert_eq!(s.toggle_key(Lane::A1, 0, KeyKind::Opacity, 10), None);
}

#[test]
fn a_slider_write_on_a_keyed_clip_lands_on_the_key_at_the_playhead() {
    let mut s = open(asset("test_av.mp4"));
    assert!(s.set_color_keys(Lane::V1, 0, vec![ckey(0, 0.0, KeyEase::Linear), ckey(100, 0.0, KeyEase::Linear)]));
    // Frame 40 gets a key of its own, then its value moves; a live sample is no undo step.
    assert!(s.put_color_key_at(Lane::V1, 0, 40, bright(0.5), true));
    assert!(s.put_color_key_at(Lane::V1, 0, 40, bright(0.75), false));
    let keys = s.color_keys_of(Lane::V1, 0).to_vec();
    assert_eq!(keys.iter().map(|k| k.at).collect::<Vec<_>>(), vec![0, 40, 100]);
    assert_eq!(keys[1].params.brightness, 0.75);
    assert!(s.undo());
    assert_eq!(s.color_keys_of(Lane::V1, 0).len(), 2, "the drag was one undo step");
    // The same for opacity and placement, and a replaced key keeps its ease.
    assert!(s.put_opacity_key_at(Lane::V1, 0, 20, 0.5, true));
    assert!(s.set_key_ease_at(Lane::V1, 0, engine::project::KeyKind::Opacity, 20, KeyEase::EaseOut));
    assert!(s.put_opacity_key_at(Lane::V1, 0, 20, 0.25, true));
    assert_eq!(s.opacity_keys_of(Lane::V1, 0), &[okey(20, 0.25, KeyEase::EaseOut)]);
    assert!(s.put_transform_key_at(Lane::V1, 0, 30, TransformParams { scale: 1.5, ..TransformParams::default() }, true));
    assert_eq!(s.transform_keys_of(Lane::V1, 0)[0].at, 30);
    assert!(!s.put_color_key_at(Lane::V1, 0, 30, bright(f32::NAN), true));
}

/// A typed opacity reads back as typed when the playhead sits between two source
/// frames: at 1.5x the playhead is on source 10.5, the key is written at the
/// rounded frame 11, and the neighbouring key at 3 would blend a fractional read
/// into 41%. The read must give the typed 40% exactly.
#[test]
fn a_typed_opacity_reads_back_exact_on_a_fractional_source_frame() {
    let mut s = open(asset("test_av.mp4"));
    s.set_speed(Lane::V1, 0, engine::project::Speed::from_permille(1500)).expect("speed");
    assert!(s.put_opacity_key_at(Lane::V1, 0, 2, 1.0, true));
    assert!(s.put_opacity_key_at(Lane::V1, 0, 7, 0.4, true));
    assert_eq!(s.opacity_keys_of(Lane::V1, 0).iter().map(|k| k.at).collect::<Vec<_>>(), vec![3, 11]);
    assert_eq!(s.opacity_at(Lane::V1, 0, 7), 0.4);
}

/// With no opacity keys the opacity is its input to the bit, and a clip whose
/// keys were set and then cleared composes to the same bytes as one never keyed:
/// nothing in a keyless clip goes through an interpolation.
#[test]
fn a_keyless_clip_is_untouched_by_the_key_machinery() {
    let clip = engine::Clip::source_range(0, 0, 100);
    for base in [0.0_f32, 0.1, 1.0 / 3.0, 0.999_999_9, 1.0] {
        assert_eq!(engine::project::clip_opacity_at(&clip, base, 7).to_bits(), base.to_bits());
    }
    let mut plain = open(asset("test_av.mp4"));
    assert!(plain.set_transform(
        Lane::V1,
        0,
        Some(TransformParams { opacity: 0.37, scale: 0.8, ..TransformParams::default() })
    ));
    assert!(plain.set_color(Lane::V1, 0, Some(bright(0.1))));
    let mut cleared = open(asset("test_av.mp4"));
    assert!(cleared.set_transform(
        Lane::V1,
        0,
        Some(TransformParams { opacity: 0.37, scale: 0.8, ..TransformParams::default() })
    ));
    assert!(cleared.set_color(Lane::V1, 0, Some(bright(0.1))));
    assert!(cleared.set_opacity_keys(Lane::V1, 0, vec![okey(0, 0.5, KeyEase::Linear)]));
    assert!(cleared.set_color_keys(Lane::V1, 0, vec![ckey(0, 0.3, KeyEase::Linear)]));
    assert!(cleared.set_opacity_keys(Lane::V1, 0, Vec::new()));
    assert!(cleared.set_color_keys(Lane::V1, 0, Vec::new()));
    for frame in [0, 40, 99] {
        assert_eq!(
            plain.compose_at(frame, W, H).expect("compose").bgra,
            cleared.compose_at(frame, W, H).expect("compose").bgra,
            "frame {frame}"
        );
    }
}
