//! Per-clip position/scale/rotate/crop, applied engine-side on the placed
//! picture so that playback and export land the same pixel from the same
//! numbers -- [`crate::color::ColorParams`]'s twin in every respect but what
//! it moves: a grade changes what a sample *is*, this changes where it *goes*.
//!
//! Plain data with no invariants of its own: out-of-range or non-finite
//! values are clamped where the geometry is built ([`crate::scale::crop_rect`],
//! [`crate::scale::transformed_dst_rect`]), so a deserializer can hand this
//! over unchecked, exactly as [`ColorParams`](crate::color::ColorParams) can.

/// One clip's placement on the project canvas, on top of whatever
/// [`crate::scale::FitPolicy`] already does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformParams {
    /// Canvas-fraction offset from centred, positive right. `0.0` is
    /// identity.
    pub pos_x: f32,
    /// Canvas-fraction offset from centred, positive down. `0.0` is identity.
    pub pos_y: f32,
    /// Multiplier on the fit-policy placement size. `1.0` is identity.
    pub scale: f32,
    /// Degrees, clockwise. Rendered at the nearest 90-degree step
    /// ([`crate::scale::rotate_i420_90s`]); the value itself is kept exactly,
    /// so a UI that later grows continuous rotation loses nothing already
    /// saved. `0.0` is identity.
    pub rotate: f32,
    /// Fraction of the source cropped off the left, `0.0..0.5`.
    pub crop_l: f32,
    /// Fraction of the source cropped off the right, `0.0..0.5`.
    pub crop_r: f32,
    /// Fraction of the source cropped off the top, `0.0..0.5`.
    pub crop_t: f32,
    /// Fraction of the source cropped off the bottom, `0.0..0.5`.
    pub crop_b: f32,
}

impl Default for TransformParams {
    fn default() -> Self {
        Self {
            pos_x: 0.0,
            pos_y: 0.0,
            scale: 1.0,
            rotate: 0.0,
            crop_l: 0.0,
            crop_r: 0.0,
            crop_t: 0.0,
            crop_b: 0.0,
        }
    }
}

impl TransformParams {
    /// Whether this is the do-nothing setting. Compared exactly, for
    /// [`crate::color::ColorParams::is_identity`]'s reason: only a value that
    /// is precisely identity is allowed to skip the geometry.
    pub fn is_identity(&self) -> bool {
        self.pos_x == 0.0
            && self.pos_y == 0.0
            && self.scale == 1.0
            && self.rotate == 0.0
            && self.crop_l == 0.0
            && self.crop_r == 0.0
            && self.crop_t == 0.0
            && self.crop_b == 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity() {
        assert!(TransformParams::default().is_identity());
        assert!(!TransformParams {
            pos_x: 0.001,
            ..TransformParams::default()
        }
        .is_identity());
    }
}
