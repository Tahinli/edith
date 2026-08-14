// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::codec::h265::parser::Level;
use crate::codec::h265::parser::Profile;
use crate::encoder::PredictionStructure;
use crate::encoder::Tunings;
use crate::Resolution;

pub struct H265;

/// What the SPS says the coded samples mean, as H.273 code points, and whether
/// the luma codes cover the full range. Written into the VUI, which is the
/// answer a decoder takes before it looks at any container tag.
#[derive(Clone, Copy)]
pub struct ColourDescription {
    pub colour_primaries: u32,
    pub transfer_characteristics: u32,
    pub matrix_coeffs: u32,
    pub full_range: bool,
}

#[derive(Clone)]
pub struct EncoderConfig {
    pub resolution: Resolution,
    pub profile: Profile,
    pub level: Level,
    pub pred_structure: PredictionStructure,
    /// Initial tunings values
    pub initial_tunings: Tunings,
    /// What the VUI declares about the samples. `None` writes no video signal
    /// type at all, and a stream that says nothing is read as "unspecified".
    pub colour: Option<ColourDescription>,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        // Artificially encoder configuration with intent to be widely supported.
        Self {
            resolution: Resolution { width: 320, height: 240 },
            profile: Profile::Main,
            level: Level::L4,
            pred_structure: PredictionStructure::LowDelay { limit: 2048 },
            initial_tunings: Default::default(),
            colour: None,
        }
    }
}
