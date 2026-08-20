//! The transform card's numbers -- [`crate::color_ui`]'s twin for where a
//! clip sits instead of what it looks like.

use crate::*;

/// The transform card's eight controls, in the order it lists them: what each
/// is called and the range it moves in. The order is `TransformParams`' own,
/// which is what [`transform_band_mut`] indexes. Rotation's range is one turn
/// short (`0..270`) because 360 is the same picture as 0 and a slider with two
/// ends that read the same is not a range, it is a cycle
/// ([`ROTATE_BAND`]/[`rotate_key`]).
pub(crate) const TRANSFORM_BANDS: [(&str, f32, f32); 8] = [
    ("Position X", -1., 1.),
    ("Position Y", -1., 1.),
    ("Scale", 0.1, 4.),
    ("Rotation", 0., 270.),
    ("Crop left", 0., 0.45),
    ("Crop right", 0., 0.45),
    ("Crop top", 0., 0.45),
    ("Crop bottom", 0., 0.45),
];

/// Which band is rotation: the one control on the card that wraps at its ends
/// instead of clamping, and steps by [`ROTATE_STEP`] rather than
/// [`TRANSFORM_STEP`] -- the engine only ever renders a 90-degree turn
/// ([`crate::transform::TransformParams::rotate`]), so a finer grid on this
/// one row would be numbers nothing on screen answers to.
pub(crate) const ROTATE_BAND: usize = 3;

/// Which band is scale: the one control on the card whose unit is a
/// multiplier (`×`) rather than a percent of frame or a degree.
pub(crate) const SCALE_BAND: usize = 2;

/// A press of a nudge key on every band but rotation: a fortieth-ish of a
/// band's range, [`crate::color_ui::COLOR_STEP`]'s own reason. A drag lands on
/// the same grid ([`Player::drag_transform`]).
pub(crate) const TRANSFORM_STEP: f32 = 0.05;

/// Rotation's own step: one 90-degree turn, [`ROTATE_BAND`]'s reason.
pub(crate) const ROTATE_STEP: f32 = 90.;

/// The card's width, [`crate::color_ui::COLOR_W`]'s own measure against this
/// card's longest label ("Position X" is short, but the value column is wider
/// here for the crop rows' extra digit).
pub(crate) const TRANSFORM_W: f32 = 460.;
/// How much of a slider row the bar itself gets, [`crate::color_ui::COLOR_BAR_W`].
pub(crate) const TRANSFORM_BAR_W: f32 = 240.;

/// A slider value on the band's own grid: [`ROTATE_BAND`] snaps to
/// [`ROTATE_STEP`] and wraps into its range, every other band snaps to
/// [`TRANSFORM_STEP`] and clamps into its own -- what a drag rounds to, so the
/// pointer stops where the arrow keys do.
pub(crate) fn transform_snap(band: usize, value: f32) -> f32 {
    let (_, low, high) = TRANSFORM_BANDS[band];
    if band == ROTATE_BAND {
        let span = high - low + ROTATE_STEP; // one turn: 0..360
        let wrapped = (value - low).rem_euclid(span);
        // The round can land exactly on `span` (315 rounds up to the 360 that
        // never appears in the range) which needs one more wrap back to 0,
        // not a clamp to a fourth stop past 270 -- `ROTATE_BAND`'s reason.
        low + ((wrapped / ROTATE_STEP).round() * ROTATE_STEP).rem_euclid(span)
    } else {
        ((value / TRANSFORM_STEP).round() * TRANSFORM_STEP).clamp(low, high)
    }
}

/// The band'th control of a placement, to read or to write. The order is
/// [`TRANSFORM_BANDS`]', which is the order the card lists them in --
/// [`crate::export_ui::band_mut`]'s own twin.
pub(crate) fn transform_band_mut(params: &mut TransformParams, band: usize) -> &mut f32 {
    match band {
        0 => &mut params.pos_x,
        1 => &mut params.pos_y,
        2 => &mut params.scale,
        3 => &mut params.rotate,
        4 => &mut params.crop_l,
        5 => &mut params.crop_r,
        6 => &mut params.crop_t,
        _ => &mut params.crop_b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_bands_snap_to_the_step_and_clamp() {
        assert_eq!(transform_snap(0, 0.024), 0.0);
        assert_eq!(transform_snap(0, 0.026), 0.05);
        assert_eq!(transform_snap(0, 5.0), 1.0); // clamped to Position X's high
        assert_eq!(transform_snap(0, -5.0), -1.0);
    }

    #[test]
    fn rotation_snaps_to_ninety_and_wraps() {
        assert_eq!(transform_snap(ROTATE_BAND, 44.0), 0.0);
        assert_eq!(transform_snap(ROTATE_BAND, 46.0), 90.0);
        assert_eq!(transform_snap(ROTATE_BAND, 270.0), 270.0);
        // Past the last stop wraps back to the first, not clamps to the last.
        // -45 is the same angle as 315 mod 360, so it lands on the same stop.
        assert_eq!(transform_snap(ROTATE_BAND, 315.0), 0.0);
        assert_eq!(transform_snap(ROTATE_BAND, -45.0), 0.0);
    }

    #[test]
    fn transform_band_mut_indexes_in_bands_order() {
        let mut p = TransformParams::default();
        *transform_band_mut(&mut p, 0) = 0.1;
        *transform_band_mut(&mut p, 3) = 90.;
        *transform_band_mut(&mut p, 7) = 0.2;
        assert_eq!(p.pos_x, 0.1);
        assert_eq!(p.rotate, 90.);
        assert_eq!(p.crop_b, 0.2);
    }
}
