//! Generated clips (solid, gradient, countdown, bars), the adjustment layer, clip colour labels
//! and switched-off clips, through the session: the pictures the render shows, the grade that
//! reaches only the frames under an adjustment layer, a disabled clip left out of picture and
//! sound, the file (v48) and undo.
//!
//! ```text
//! cargo test -p engine --release --test generators
//! ```

use std::path::PathBuf;

use engine::PlaybackSession;
use engine::color::ColorParams;
use engine::generate::{Gen, Kind};
use engine::project::{Lane, LaneKind};
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

/// `test_av.mp4` (1280x720, 30 fps, 150 frames) with a second video lane.
fn rig() -> PlaybackSession {
    engine::ao::set_silent_output();
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    s.add_lane(LaneKind::Video);
    s
}

const W: u32 = 320;
const H: u32 = 180;

/// The BGRA pixel at (`x`, `y`) of timeline frame `frame`, as RGB.
fn px(s: &PlaybackSession, frame: u32, x: usize, y: usize) -> [i32; 3] {
    let f = s.compose_at(frame, W, H).expect("frame");
    let i = (y * f.width as usize + x) * 4;
    [i32::from(f.bgra[i + 2]), i32::from(f.bgra[i + 1]), i32::from(f.bgra[i])]
}

/// Row `y` of timeline frame `frame` as RGB: one composition for the whole row. `px` composes the
/// frame on every call, so a row read pixel by pixel costs a composition per pixel.
fn row_at(s: &PlaybackSession, frame: u32, y: usize) -> Vec<[i32; 3]> {
    let f = s.compose_at(frame, W, H).expect("frame");
    (0..W as usize)
        .map(|x| {
            let i = (y * f.width as usize + x) * 4;
            [i32::from(f.bgra[i + 2]), i32::from(f.bgra[i + 1]), i32::from(f.bgra[i])]
        })
        .collect()
}

fn near(a: [i32; 3], b: [i32; 3], tol: i32) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() <= tol)
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn mean_luma(s: &PlaybackSession, frame: u32) -> f64 {
    let f = s.compose_at(frame, W, H).expect("frame");
    f.bgra.chunks_exact(4).map(|p| 0.299 * f64::from(p[2]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[0])).sum::<f64>() / (f.bgra.len() / 4) as f64
}

fn solid(s: &PlaybackSession, rgb: u32) -> Gen {
    Gen::new(Kind::Solid { rgb }, s.meta().width, s.meta().height)
}

#[test]
fn a_generator_renders_its_pixels() {
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    assert!(s.place_generated(solid(&s, 0x00c800), 1.0, Some(2.0), Some(v2)).expect("placed"));
    assert!(near(px(&s, 45, 160, 90), [0, 200, 0], 6), "{:?}", px(&s, 45, 160, 90));
    assert!(!near(px(&s, 10, 160, 90), [0, 200, 0], 30), "before it the footage shows");
    assert!(!near(px(&s, 95, 160, 90), [0, 200, 0], 30), "after its two seconds the footage shows");
    // The clip is 2 s of the project's rate and says what it is.
    assert_eq!(s.lane_clips(v2)[0].frames(), 60);
    assert_eq!(s.generator_of(v2, 0).map(|g| g.kind), Some(Kind::Solid { rgb: 0x00c800 }));
    assert_eq!(s.generator_of(Lane::V1, 0), None);

    // A gradient runs between its two colours across the frame.
    let grad = Gen::new(Kind::Gradient { a: 0xff0000, b: 0x0000ff, angle: 0 }, s.meta().width, s.meta().height);
    assert!(s.set_generator(v2, 0, grad));
    let (left, right) = (px(&s, 45, 2, 90), px(&s, 45, W as usize - 3, 90));
    assert!(left[0] > 220 && left[2] < 40, "{left:?}");
    assert!(right[2] > 220 && right[0] < 40, "{right:?}");
    assert!(s.undo(), "the change of generator is one undo step");
    assert!(near(px(&s, 45, 160, 90), [0, 200, 0], 6));
}

#[test]
fn bars_and_a_countdown_are_pictures() {
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    let bars = s.generator_named("bars").expect("bars");
    assert!(s.place_generated(bars, 0.0, None, Some(v2)).expect("placed"));
    assert_eq!(s.lane_clips(v2)[0].frames(), 150, "five seconds at 30 fps");
    assert!(near(px(&s, 3, 5, 90), [191, 191, 191], 8), "white first");
    assert!(near(px(&s, 3, W as usize - 5, 90), [0, 0, 191], 8), "blue last");

    let cd = s.generator_named("countdown").expect("countdown");
    assert!(s.place_generated(cd, 6.0, None, Some(v2)).expect("placed"));
    let second = s.lane_clips(v2)[1].clone();
    assert_eq!(second.frames(), 150, "a countdown from 5 runs 5 s");
    let at = |sec: f64| s.compose_at(second.start + (sec * 30.0) as u32 + 2, W, H).expect("frame").bgra;
    assert_ne!(at(0.0), at(1.0));
    assert_ne!(at(1.0), at(2.0));
    assert_eq!(at(1.0), at(1.0));
}

#[test]
fn an_adjustment_layer_grades_only_what_is_under_it_and_only_while_it_lasts() {
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    let before: Vec<f64> = [10, 45, 80].iter().map(|&f| mean_luma(&s, f)).collect();
    let adjust = s.generator_named("adjust").expect("adjust");
    assert!(s.place_generated(adjust, 1.0, Some(1.0), Some(v2)).expect("placed"));
    assert!(s.set_color(v2, 0, Some(ColorParams { brightness: -0.9, ..Default::default() })));
    assert_eq!(mean_luma(&s, 10), before[0], "before the layer nothing changed");
    assert_eq!(mean_luma(&s, 80), before[2], "after the layer nothing changed");
    let inside = mean_luma(&s, 45);
    assert!(inside < before[1] - 20.0, "the grade reaches the footage under it: {inside} vs {}", before[1]);
    // A layer above the adjustment is not under it.
    let v3 = s.add_lane(LaneKind::Video);
    assert!(s.place_generated(solid(&s, 0xc8c8c8), 1.0, Some(1.0), Some(v3)).expect("placed"));
    let top = px(&s, 45, 160, 90);
    assert!(near(top, [200, 200, 200], 6), "a layer above the adjustment is untouched: {top:?}");
}

#[test]
fn a_disabled_clip_is_skipped_in_picture_and_sound_and_undo_brings_it_back() {
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    let plain = mean_luma(&s, 45);
    assert!(s.place_generated(solid(&s, 0xffffff), 1.0, Some(2.0), Some(v2)).expect("placed"));
    assert!(mean_luma(&s, 45) > 200.0, "the white fills the frame");
    assert!(s.set_enabled(v2, 0, false));
    assert_eq!(mean_luma(&s, 45), plain, "off, the clip is not drawn");
    assert_eq!(s.lane_clips(v2).len(), 1, "but it is still on the timeline");
    assert!(s.mark_of(v2, 0).off);
    // The sound too: the render view has no clip on the audio lane where it is switched off.
    let a1 = Lane::new(LaneKind::Audio, 0);
    assert!(!s.export_snapshot().0.lane(a1).is_empty());
    assert!(s.set_enabled(a1, 0, false));
    assert!(s.export_snapshot().0.lane(a1).is_empty(), "a disabled audio clip is not mixed");
    assert!(s.set_enabled(a1, 0, true));
    assert!(s.undo() && s.mark_of(a1, 0).off, "undo takes the last switch back");
    assert!(s.undo() && !s.mark_of(a1, 0).off, "and the one before");
    assert!(s.undo() && !s.mark_of(v2, 0).off, "and the first");
    assert!(mean_luma(&s, 45) > 200.0, "the white is drawn again");
}

#[test]
fn labels_generators_and_switches_round_trip_in_a_v48_file() {
    let dir = Scratch::dir("gens_roundtrip");
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    let grad = Gen::new(Kind::Gradient { a: 0x112233, b: 0xaabbcc, angle: 30 }, s.meta().width, s.meta().height);
    assert!(s.place_generated(grad, 0.0, Some(3.0), Some(v2)).expect("placed"));
    assert!(s.set_label(Lane::V1, 0, 7));
    assert!(!s.set_label(Lane::V1, 0, 7), "no change, no step");
    assert!(!s.set_label(Lane::V1, 0, 9), "past the palette");
    assert!(s.set_enabled(v2, 0, false));
    let path = dir.join("g.edith");
    s.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.starts_with("edith 48\n"), "{}", text.lines().next().unwrap_or(""));
    assert!(text.contains("\nmark video 1 0 7 on\n") && text.contains("\nmark video 2 0 0 off\n"), "{text}");
    let back = PlaybackSession::open_project(&path).expect("reopen");
    assert_eq!(back.mark_of(Lane::V1, 0).label, 7);
    assert!(back.mark_of(v2, 0).off);
    assert_eq!(back.generator_of(v2, 0).map(|g| g.kind), Some(Kind::Gradient { a: 0x112233, b: 0xaabbcc, angle: 30 }));
    assert!(back.offline_sources().is_empty(), "a generator has no file to be missing");
    // A project that uses none of it stays on its old version, and a v47 file refuses a mark line.
    let plain = dir.join("p.edith");
    PlaybackSession::open(asset("test_av.mp4")).expect("open").save_project(&plain).expect("save");
    assert!(!std::fs::read_to_string(&plain).unwrap().starts_with("edith 48"));
    let old = text.replacen("edith 48", "edith 47", 1);
    let bad = dir.join("old.edith");
    std::fs::write(&bad, old).unwrap();
    assert!(PlaybackSession::open_project(&bad).is_err(), "a v47 file has no mark lines");
}

#[test]
fn an_adjustment_layer_alone_is_black_not_an_error() {
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    // V1 holds the footage for 5 s; the layer sits past its end, with nothing beneath it.
    let adjust = s.generator_named("adjust").expect("adjust");
    assert!(s.place_generated(adjust, 6.0, Some(1.0), Some(v2)).expect("placed"));
    let alone = px(&s, 195, 160, 90);
    assert!(near(alone, [0, 0, 0], 4), "an adjustment layer alone is black: {alone:?}");
}

#[test]
fn an_adjustment_layer_on_the_lowest_lane_is_ignored() {
    let mut s = rig();
    let v1 = Lane::new(LaneKind::Video, 0);
    let v2 = Lane::new(LaneKind::Video, 1);
    let adjust = s.generator_named("adjust").expect("adjust");
    assert!(s.place_generated(adjust, 6.0, Some(1.0), Some(v1)).expect("placed"));
    assert!(s.set_color(v1, 1, Some(ColorParams { brightness: -0.9, ..Default::default() })));
    assert!(s.place_generated(solid(&s, 0x00c800), 6.0, Some(1.0), Some(v2)).expect("placed"));
    let p = px(&s, 195, 160, 90);
    assert!(near(p, [0, 200, 0], 6), "the layer beneath everything changes nothing: {p:?}");
}

#[test]
fn a_cropped_adjustment_layer_leaves_the_cropped_away_area_untouched() {
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    let before = px(&s, 45, 20, 90);
    let mean = mean_luma(&s, 45);
    let adjust = s.generator_named("adjust").expect("adjust");
    assert!(s.place_generated(adjust, 1.0, Some(2.0), Some(v2)).expect("placed"));
    assert!(s.set_color(v2, 0, Some(ColorParams { brightness: -0.9, ..Default::default() })));
    let t = engine::transform::TransformParams { crop_l: 0.5, ..Default::default() };
    assert!(s.set_transform(v2, 0, Some(t)));
    assert_eq!(px(&s, 45, 20, 90), before, "the cropped-away area keeps the base");
    let after = mean_luma(&s, 45);
    assert!(after < mean - 5.0, "the kept part is graded: {after} vs {mean}");
}

#[test]
fn a_scaled_adjustment_layer_moves_the_composite_and_leaves_black_around_it() {
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    let centre_before = px(&s, 45, 160, 90);
    let adjust = s.generator_named("adjust").expect("adjust");
    assert!(s.place_generated(adjust, 1.0, Some(2.0), Some(v2)).expect("placed"));
    assert!(s.set_color(v2, 0, Some(ColorParams { brightness: -0.5, ..Default::default() })));
    let t = engine::transform::TransformParams { scale: 0.5, ..Default::default() };
    assert!(s.set_transform(v2, 0, Some(t)));
    let corner = px(&s, 45, 3, 3);
    assert!(near(corner, [0, 0, 0], 4), "outside the shrunk composite is black: {corner:?}");
    let centre = px(&s, 45, 160, 90);
    assert!(centre != centre_before && !near(centre, [0, 0, 0], 4), "the centre is graded: {centre:?} vs {centre_before:?}");
}

#[test]
fn a_lone_adjustment_layer_is_the_same_black_as_a_gap() {
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    let adjust = s.generator_named("adjust").expect("adjust");
    assert!(s.place_generated(adjust, 6.0, Some(1.0), Some(v2)).expect("placed"));
    let lone = s.compose_at(195, W, H).expect("frame").bgra;
    let gap = s.compose_at(165, W, H).expect("frame").bgra;
    assert_eq!(lone, gap, "a lone adjustment layer composes the gap's black");
}

#[test]
fn a_shape_composites_over_the_lane_below_and_alone_over_black() {
    use engine::generate::{Form, Shape};
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    let bare = px(&s, 45, 10, 10);
    let circle = Gen::new(Kind::Shape(Shape { form: Form::Ellipse, fill: Some(0xff0000), stroke: None, width: 0, radius: 0 }), s.meta().width, s.meta().height);
    assert!(s.place_generated(circle, 1.0, Some(2.0), Some(v2)).expect("placed"));
    // The ellipse is red at the centre and the footage shows through the clear corners.
    assert!(near(px(&s, 45, 160, 90), [255, 0, 0], 12), "{:?}", px(&s, 45, 160, 90));
    assert!(near(px(&s, 45, 10, 10), bare, 6), "corner {:?} vs footage {bare:?}", px(&s, 45, 10, 10));
    // Its edge is anti-aliased: some pixel on the row is neither red nor the footage.
    let row = row_at(&s, 45, 40);
    let corner = row[0];
    assert!(row.iter().any(|p| p[0] > 200) && row.iter().any(|p| near(*p, corner, 6)));
    // A line clip alone on the timeline sits on black, not on its own colour.
    let mut alone = PlaybackSession::empty(W, H, 30.0).expect("empty");
    let line = Gen::new(Kind::Shape(Shape::of(Form::Line)), W, H);
    assert!(alone.place_generated(line, 0.0, Some(1.0), Some(Lane::V1)).expect("placed"));
    assert!(near(px(&alone, 5, 5, 5), [16, 16, 16], 24) || near(px(&alone, 5, 5, 5), [0, 0, 0], 24), "{:?}", px(&alone, 5, 5, 5));
    assert!(px(&alone, 5, 160, 90)[0] > 200, "the stroke is white");
    // Changing the shape is one undo step.
    let rect = Gen::new(Kind::Shape(Shape::of(Form::Rectangle)), s.meta().width, s.meta().height);
    assert!(s.set_generator(v2, 0, rect));
    assert!(near(px(&s, 45, 10, 10), [0x2a, 0x6d, 0xf4], 14), "{:?}", px(&s, 45, 10, 10));
    assert!(s.undo());
    assert!(near(px(&s, 45, 10, 10), bare, 6));
}

#[test]
fn a_shape_keeps_its_alpha_under_a_grade_and_on_a_canvas_of_another_size() {
    use engine::generate::{Form, Shape};
    let ellipse = |w, h| Gen::new(Kind::Shape(Shape { form: Form::Ellipse, fill: Some(0xff0000), stroke: None, width: 0, radius: 0 }), w, h);
    let mut s = rig();
    let v2 = Lane::new(LaneKind::Video, 1);
    let bare = row_at(&s, 45, 40);
    assert!(s.place_generated(ellipse(s.meta().width, s.meta().height), 1.0, Some(2.0), Some(v2)).expect("placed"));
    let covered = |s: &PlaybackSession| row_at(s, 45, 40).iter().zip(&bare).filter(|(p, b)| p.iter().zip(b.iter()).any(|(a, b)| (a - b).abs() > 8)).count();
    let plain = covered(&s);
    assert!(plain > 20 && plain < W as usize, "the ellipse covers part of the row: {plain}");
    // A grade changes the shape's colour and not where it is: the alpha takes no part in it.
    assert!(s.set_color(v2, 0, Some(ColorParams { brightness: 0.05, ..Default::default() })));
    let graded = covered(&s);
    assert!(plain.abs_diff(graded) <= 3, "plain {plain} graded {graded}");
    // A generator drawn for another size than the canvas is still a shape, not the full frame.
    let mut small = PlaybackSession::empty(W, H, 30.0).expect("empty");
    assert!(small.place_generated(ellipse(1920, 1080), 0.0, Some(1.0), Some(Lane::V1)).expect("placed"));
    assert!(px(&small, 5, 160, 90)[0] > 200, "centre is the fill");
    assert!(px(&small, 5, 2, 2)[0] < 40, "the corner is clear: {:?}", px(&small, 5, 2, 2));
}
