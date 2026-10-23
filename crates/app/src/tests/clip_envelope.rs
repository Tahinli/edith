//! The audio clip envelope's geometry (`ui/bench_stance/clip_envelope.rs`):
//! where each key lands on a clip body, the dB-to-height curve, and what counts
//! as an envelope to draw at all. Pure numbers, so no window is needed.

use crate::ui::bench_stance::clip_envelope::{
    BOTTOM_DB, TOP_DB, envelope, has_envelope, height_of, key_x,
};
use engine::GainKey;

fn k(at: u32, db: f32) -> GainKey {
    GainKey::new(at, db)
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

/// The fader-style curve pins its ends and holds outside the range.
#[test]
fn height_of_pins_the_ends_and_clamps() {
    assert!(close(height_of(TOP_DB), 1.));
    assert!(close(height_of(BOTTOM_DB), 0.));
    assert!(
        close(height_of(40.), 1.),
        "above the top is held at the top"
    );
    assert!(
        close(height_of(-100.), 0.),
        "below the floor is held at the floor"
    );
}

/// Louder is always higher, so a dip reads as a dip.
#[test]
fn height_of_is_monotonic() {
    let mut prev = height_of(BOTTOM_DB);
    let mut db = BOTTOM_DB;
    while db <= TOP_DB {
        let h = height_of(db);
        assert!(h >= prev, "height fell at {db} dB");
        prev = h;
        db += 0.5;
    }
}

/// Unity sits well up the body, and the -6 and -18 dips are clearly apart.
#[test]
fn unity_is_high_and_the_dips_are_visible() {
    let zero = height_of(0.);
    assert!((0.6..0.75).contains(&zero), "0 dB at {zero}");
    let six = height_of(-6.);
    let eighteen = height_of(-18.);
    assert!(zero - six > 0.1, "-6 dB dip too small: {}", zero - six);
    assert!(six - eighteen > 0.15, "-18 dB dip too small");
}

/// Only a clip with keys or a non-zero gain gets an envelope.
#[test]
fn has_envelope_needs_keys_or_gain() {
    assert!(!has_envelope(&[], 0.));
    assert!(has_envelope(&[], -6.));
    assert!(has_envelope(&[k(0, 0.)], 0.), "a key at unity still counts");
}

/// A key's x: its source frame relative to the in point, divided by speed, times
/// the pixels per timeline frame. Before the in point it is negative.
#[test]
fn key_x_follows_in_point_and_speed() {
    assert!(close(key_x(150, 100, 1., 2.), 100.));
    assert!(
        close(key_x(150, 100, 2., 2.), 50.),
        "2x plays twice as fast"
    );
    assert!(close(key_x(100, 100, 1., 2.), 0.));
    assert!(
        key_x(90, 100, 1., 2.) < 0.,
        "before the in point is left of the edge"
    );
}

/// A clip with no keys is a flat line at its own gain, end to end.
#[test]
fn clip_gain_alone_is_a_flat_line() {
    let env = envelope(&[], -6., 0, 1., 1., 30.);
    assert!(env.dots.is_empty());
    assert!(!env.line.is_empty());
    assert!(close(env.line.first().unwrap().0, 0.));
    assert!(close(env.line.last().unwrap().0, 30.));
    for &(_, h) in &env.line {
        assert!(close(h, height_of(-6.)));
    }
}

/// Between two keys the dB is linear in source frames, so a sample halfway
/// between them carries the halfway dB, and each key is a dot at its own x.
#[test]
fn the_line_is_linear_in_db_between_keys() {
    let keys = [k(0, 0.), k(30, -12.)];
    let env = envelope(&keys, 0., 0, 1., 1., 60.);
    // x = 15 is on the sampling grid, halfway between the keys: -6 dB.
    let mid = env
        .line
        .iter()
        .find(|(x, _)| close(*x, 15.))
        .expect("grid point at 15");
    assert!(close(mid.1, height_of(-6.)), "midpoint height {}", mid.1);
    // x = 30 is the second key, exact.
    let at_key = env
        .line
        .iter()
        .find(|(x, _)| close(*x, 30.))
        .expect("key x is a sample");
    assert!(close(at_key.1, height_of(-12.)));
    assert_eq!(env.dots.len(), 2);
    assert!(close(env.dots[1].0, 30.) && close(env.dots[1].1, height_of(-12.)));
}

/// Clip gain is added to the keyed envelope, on the line and on the dots alike.
#[test]
fn clip_gain_offsets_keys() {
    let env = envelope(&[k(0, 0.)], -6., 0, 1., 1., 20.);
    assert!(close(env.line[0].1, height_of(-6.)));
    assert!(close(env.dots[0].1, height_of(-6.)));
}

/// Outside the first and last key the envelope holds its end value.
#[test]
fn the_envelope_holds_beyond_its_ends() {
    let env = envelope(&[k(50, -12.)], 0., 0, 1., 1., 100.);
    assert_eq!(env.dots.len(), 1, "the one key is inside the body");
    for &(_, h) in &env.line {
        assert!(close(h, height_of(-12.)));
    }
}

/// Keys off the body are not dots, but the line still runs across it, following
/// the envelope between the keys either side of the body.
#[test]
fn keys_outside_the_body_are_not_dots() {
    let env = envelope(&[k(10, -24.), k(200, -3.)], 0., 100, 1., 1., 50.);
    assert!(env.dots.is_empty(), "both keys are off the body");
    // Source 100 and 150 sit between the keys at 10 and 200 (-24 and -3 dB).
    let at = |src: f32| -24. + 21. * (src - 10.) / 190.;
    assert!(close(env.line[0].1, height_of(at(100.))));
    assert!(close(env.line.last().unwrap().1, height_of(at(150.))));
}

/// A trimmed-in clip: the first kept key sits on the left edge, a later one
/// lands at its own offset, and speed scales that offset.
#[test]
fn trim_and_speed_move_the_dots() {
    let keys = [k(100, 0.), k(200, -24.)];
    let env = envelope(&keys, 0., 100, 1., 1., 100.);
    assert!(close(env.dots[0].0, 0.));
    assert!(close(env.dots[1].0, 100.));

    let fast = envelope(&keys, 0., 100, 2., 1., 100.);
    assert!(close(fast.dots[1].0, 50.), "at 2x the same key is half way");
}

/// Keys on the body and at both edges are all samples, sorted, inside the
/// body, with no duplicates -- the polyline a painter can walk in order.
#[test]
fn line_is_sorted_inside_the_body() {
    let keys = [k(0, 0.), k(7, -3.), k(40, -20.), k(90, 6.)];
    let env = envelope(&keys, 0., 0, 1., 1., 45.);
    let xs: Vec<f32> = env.line.iter().map(|p| p.0).collect();
    assert!(xs.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
    assert!(close(xs[0], 0.) && close(*xs.last().unwrap(), 45.));
    assert!(env.dots.iter().all(|(x, _)| (0. ..=45.).contains(x)));
}

/// A zero-width body, or a scale that is not usable, has nothing to draw.
#[test]
fn degenerate_geometry_is_empty() {
    assert!(envelope(&[k(0, 0.)], 0., 0, 1., 1., 0.).line.is_empty());
    assert!(envelope(&[k(0, 0.)], 0., 0, 1., 0., 10.).line.is_empty());
    assert!(
        envelope(&[k(0, 0.)], 0., 0, 1., f32::NAN, 10.)
            .line
            .is_empty()
    );
}
