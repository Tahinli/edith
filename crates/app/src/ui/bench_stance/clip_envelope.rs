//! The volume envelope drawn over an audio clip's body: a thin line through the
//! clip's effective dB (its own gain plus its keys) and a dot at each key. Display
//! only -- nothing here edits the envelope.
//!
//! The geometry is pure and lives in [`envelope`], so the tests can pin it
//! without a window: clip-local x in pixels, and a height in `0..=1` for each
//! point. [`overlay`] paints it. Keys sit in source frames; a clip maps source to
//! timeline as `src = in_frame + offset * speed` (the same mapping
//! `Project::gain_db_at` uses), so a key at source `at` lands at the offset
//! `(at - in_frame) / speed` timeline frames after the clip's left edge.

use crate::*;
use engine::GainKey;
use engine::project::gain_keys_db_at;
use gpui::PathBuilder;

/// The dB at the top of a clip body. Above it the line is held at the top.
pub(crate) const TOP_DB: f32 = 12.;
/// The dB at the bottom of a clip body. Below it the line is held at the bottom.
pub(crate) const BOTTOM_DB: f32 = -60.;
/// Pixels kept clear above and below the line, so a peak does not touch the edge.
const PAD: f32 = 4.;
/// Pixels between sampled points of the line where no key falls. The mapping
/// from dB to height is curved, so a straight run between two samples would
/// drift from the true curve by a sub-pixel amount at this spacing.
const SAMPLE_PX: f32 = 3.;
/// Side of a key's dot, in pixels.
const DOT_PX: f32 = 4.;
/// How close, in pixels, the pointer must be to a key's centre to grab it.
pub(crate) const KEY_HIT: f32 = 6.;
/// How close, in pixels, the pointer must be to the line to grab it.
pub(crate) const LINE_HIT: f32 = 5.;

/// What a clip's envelope draws, in clip-local pixels: `line` runs from the left
/// edge to the right edge, and `dots` are the keys that fall inside the body.
/// Both carry `(x, height)` with height in `0..=1`, `0` at the bottom.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Envelope {
    pub(crate) line: Vec<(f32, f32)>,
    pub(crate) dots: Vec<(f32, f32)>,
}

/// Whether there is an envelope to draw at all: a clip with keys or with a
/// non-zero clip gain. A plain clip draws nothing.
pub(crate) fn has_envelope(keys: &[GainKey], clip_gain_db: f32) -> bool {
    !keys.is_empty() || clip_gain_db != 0.
}

/// The height in `0..=1` of `db` on a clip body. A power curve over the linear
/// dB range, the way a fader's throw is laid out, so the quiet end is not
/// crushed into the floor: 0 dB sits about two thirds up, and a -6 dB dip and a
/// -18 dB dip stay clearly apart. Clamped to [`BOTTOM_DB`]..=[`TOP_DB`].
pub(crate) fn height_of(db: f32) -> f32 {
    let t = ((db.clamp(BOTTOM_DB, TOP_DB) - BOTTOM_DB) / (TOP_DB - BOTTOM_DB)).clamp(0., 1.);
    t * t
}

/// Clip-local x, in pixels, of the timeline frame that plays source frame `at`.
/// `px_per_frame` is the bed's pixels per timeline frame. A speed that is not
/// positive is treated as 1x, so a bad value cannot divide by zero.
pub(crate) fn key_x(at: u32, in_frame: u32, speed: f64, px_per_frame: f32) -> f32 {
    let speed = if speed > 0. { speed } else { 1. };
    let offset = (f64::from(at) - f64::from(in_frame)) / speed;
    (offset * f64::from(px_per_frame)) as f32
}

/// The effective dB of the clip at clip-local `x`: its own gain plus the keyed
/// envelope at the source frame that plays there. Fades are not in it.
fn db_at_x(
    keys: &[GainKey],
    clip_gain_db: f32,
    in_frame: u32,
    speed: f64,
    px_per_frame: f32,
    x: f32,
) -> f32 {
    let speed = if speed > 0. { speed } else { 1. };
    let offset = f64::from(x) / f64::from(px_per_frame);
    let src = f64::from(in_frame) + offset * speed;
    clip_gain_db + gain_keys_db_at(keys, src)
}

/// The envelope of a clip `width` pixels wide. The line is sampled every
/// [`SAMPLE_PX`], and every key's x is added as a sample of its own, so each
/// bend of the curve is exact rather than approximated. Empty for a zero-width
/// body or a non-positive scale.
pub(crate) fn envelope(
    keys: &[GainKey],
    clip_gain_db: f32,
    in_frame: u32,
    speed: f64,
    px_per_frame: f32,
    width: f32,
) -> Envelope {
    if width <= 0. || px_per_frame <= 0. || !px_per_frame.is_finite() {
        return Envelope {
            line: Vec::new(),
            dots: Vec::new(),
        };
    }
    let key_xs: Vec<f32> = keys
        .iter()
        .map(|k| key_x(k.at, in_frame, speed, px_per_frame))
        .collect();
    let mut xs: Vec<f32> = vec![0., width];
    let mut x = SAMPLE_PX;
    while x < width {
        xs.push(x);
        x += SAMPLE_PX;
    }
    xs.extend(key_xs.iter().copied().filter(|x| (0. ..=width).contains(x)));
    xs.sort_by(f32::total_cmp);
    xs.dedup();
    let line = xs
        .into_iter()
        .map(|x| {
            let db = db_at_x(keys, clip_gain_db, in_frame, speed, px_per_frame, x);
            (x, height_of(db))
        })
        .collect();
    let dots = keys
        .iter()
        .zip(&key_xs)
        .filter(|(_, x)| (0. ..=width).contains(*x))
        .map(|(k, &x)| (x, height_of(clip_gain_db + k.db)))
        .collect();
    Envelope { line, dots }
}

/// What the pointer is over on an envelope: a key, or the line -- and then the segment it lies
/// on, between key `i` and key `i + 1`, or `None` where the line is held flat past the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvHit {
    Key(usize),
    Line(Option<usize>),
}

/// The pixel height of a point `height` (0..=1) on a body `body_h` pixels tall, from the top.
pub(crate) fn y_of(height: f32, body_h: f32) -> f32 {
    PAD + (1. - height) * (body_h - 2. * PAD).max(0.)
}

/// The display dB of a pixel `y` on a body `body_h` tall: the inverse of [`height_of`] and
/// [`y_of`], clamped to the drawn range.
pub(crate) fn db_of_y(y: f32, body_h: f32) -> f32 {
    let span = (body_h - 2. * PAD).max(1.);
    let height = (1. - (y - PAD) / span).clamp(0., 1.);
    BOTTOM_DB + height.sqrt() * (TOP_DB - BOTTOM_DB)
}

/// What the pointer `p` (clip-local pixels) is over, for an envelope drawn with
/// [`envelope`]'s own mapping. A key wins over the line; the line counts anywhere along the clip.
pub(crate) fn hit(
    keys: &[GainKey],
    clip_gain_db: f32,
    in_frame: u32,
    speed: f64,
    px_per_frame: f32,
    size: (f32, f32),
    p: (f32, f32),
) -> Option<EnvHit> {
    let (width, body_h) = size;
    if width <= 0. || body_h <= 0. || px_per_frame <= 0. || !px_per_frame.is_finite() {
        return None;
    }
    for (i, k) in keys.iter().enumerate() {
        let x = key_x(k.at, in_frame, speed, px_per_frame);
        let y = y_of(height_of(clip_gain_db + k.db), body_h);
        if (p.0 - x).abs() <= KEY_HIT && (p.1 - y).abs() <= KEY_HIT {
            return Some(EnvHit::Key(i));
        }
    }
    if p.0 < 0. || p.0 > width {
        return None;
    }
    let y = y_of(height_of(db_at_x(keys, clip_gain_db, in_frame, speed, px_per_frame, p.0)), body_h);
    if (p.1 - y).abs() > LINE_HIT {
        return None;
    }
    let seg = (0..keys.len().saturating_sub(1)).find(|&i| {
        let x0 = key_x(keys[i].at, in_frame, speed, px_per_frame);
        let x1 = key_x(keys[i + 1].at, in_frame, speed, px_per_frame);
        (x0..=x1).contains(&p.0)
    });
    Some(EnvHit::Line(seg))
}

/// Paints an [`Envelope`] over a clip body: the line in the primary ink, the
/// key dots in the accent. The body's own height is mapped to the padded range,
/// with height `1` at the top.
pub(crate) fn overlay(env: Envelope) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let h = f32::from(s.height);
            if env.line.len() < 2 || h <= 0. {
                return;
            }
            let y = |t: f32| o.y + px(PAD + (1. - t) * (h - 2. * PAD).max(0.));
            let mut line = PathBuilder::stroke(px(1.5));
            for (i, &(x, t)) in env.line.iter().enumerate() {
                let p = point(o.x + px(x), y(t));
                if i == 0 {
                    line.move_to(p);
                } else {
                    line.line_to(p);
                }
            }
            if let Ok(line) = line.build() {
                window.paint_path(line, rgb(INK1()));
            }
            let r = DOT_PX / 2.;
            for &(x, t) in &env.dots {
                let (cx, cy) = (o.x + px(x), y(t));
                let mut dot = PathBuilder::fill();
                dot.add_polygon(
                    &[
                        point(cx - px(r), cy - px(r)),
                        point(cx + px(r), cy - px(r)),
                        point(cx + px(r), cy + px(r)),
                        point(cx - px(r), cy + px(r)),
                    ],
                    true,
                );
                if let Ok(dot) = dot.build() {
                    window.paint_path(dot, rgb(ACCENT_PRIMARY()));
                }
            }
        },
    )
    .size_full()
}
