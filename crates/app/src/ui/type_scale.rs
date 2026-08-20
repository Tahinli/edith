//! DESIGN.md §3 -- the room's two faces, bundled so nothing depends on the
//! machine happening to have them (`fc-list` on the dev box finds neither).
//!
//! Archivo is the room's voice (labels, verbs, section heads); Spline Sans
//! Mono is everything the film says (timecode, chords, readouts, metadata,
//! names, the ledger). Every call site asks this module for a style by
//! *role*, the same discipline `ui::theme` holds for colour -- no element
//! spells a family string or a raw weight number itself.
//!
//! The two TTFs in `assets/fonts/` are Google Fonts variable masters (one
//! file, a `wght` axis). gpui's Linux text backend is `cosmic-text` 0.14,
//! which carries no variable-font axis support (grepped: no `fvar`/variation
//! handling anywhere in its source) -- so embedding the variable master
//! directly would register exactly one face per family and render every
//! requested weight as that face's single baked-in weight. The five
//! `*-Regular.ttf` / `*-Medium.ttf` / `*-Bold.ttf` files beside the masters
//! are static instances pinned with `fonttools varLib.instancer` (already on
//! this box; no new Rust dependency), one per weight DESIGN §3 actually
//! names. They share the master's OFL licence (an instanced subset is a
//! derivative work, which OFL permits) and its typographic family name, so
//! `font_kit::matching::find_best_match` picks the right static face for a
//! requested weight the same way it would pick among any other family's
//! separately-shipped weight files.

// The four region rebuilds (dock/bench/time band/spine) consume this module's
// `label`/`mono`/`head` in their own concurrent steps; until they land, this
// crate's only caller of the public API is `main.rs`'s `register`.
#![allow(dead_code)]

use gpui::{App, Font, FontFeatures, FontWeight, Pixels, SharedString, font, px};
use std::borrow::Cow;

/// Archivo: labels, verbs, section heads.
pub const ARCHIVO: &str = "Archivo";
/// Spline Sans Mono: timecode, chords, readouts, metadata, names, the ledger.
pub const SPLINE_SANS_MONO: &str = "Spline Sans Mono";

/// DESIGN §3's scale, named so a call site reads the role, not a number.
pub const HERO_TIMECODE_PX: f32 = 13.;
pub const LABEL_ROW_PX: f32 = 10.5;
pub const CHORD_METADATA_MAX_PX: f32 = 10.;
pub const CHORD_METADATA_MIN_PX: f32 = 9.5;
pub const SECTION_HEAD_PX: f32 = 9.;
/// The floor DESIGN §3 sets: nothing in the room reads smaller than this.
pub const FLOOR_PX: f32 = 8.;

/// A family, weight and size together -- what a call site needs to paint
/// text with `.font(style.font).text_size(style.size)`, and nothing it has
/// to spell out by hand.
#[derive(Clone, Debug)]
pub struct TypeStyle {
    pub font: Font,
    pub size: Pixels,
}

/// Tabular figures, wherever digits align (odometers, timecodes, readouts).
/// `tnum` is an arbitrary OpenType feature tag to gpui's `FontFeatures`, so
/// this is the one place that spells it -- every mono call site gets it for
/// free rather than asking for it by hand.
fn tabular_figures() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".to_string(), 1)]))
}

fn family_font(family: impl Into<SharedString>, weight: FontWeight, tabular: bool) -> Font {
    let mut f = font(family).with_weight(weight);
    if tabular {
        f.features = tabular_figures();
    }
    f
}

trait WithWeight {
    fn with_weight(self, weight: FontWeight) -> Self;
}
impl WithWeight for Font {
    fn with_weight(mut self, weight: FontWeight) -> Self {
        self.weight = weight;
        self
    }
}

/// Archivo, at the given size and weight -- labels, verbs, section heads.
/// No italics anywhere in the room (DESIGN §3), so this never offers one.
pub fn label(size_px: f32, weight: FontWeight) -> TypeStyle {
    debug_assert!(size_px >= FLOOR_PX, "below DESIGN §3's 8px floor: {size_px}");
    TypeStyle {
        font: family_font(ARCHIVO, weight, false),
        size: px(size_px),
    }
}

/// Spline Sans Mono, at the given size and weight, tabular figures on --
/// everything the film says.
pub fn mono(size_px: f32, weight: FontWeight) -> TypeStyle {
    debug_assert!(size_px >= FLOOR_PX, "below DESIGN §3's 8px floor: {size_px}");
    TypeStyle {
        font: family_font(SPLINE_SANS_MONO, weight, true),
        size: px(size_px),
    }
}

/// A 9px Archivo 700 section head, `ink3`. The caller still has to uppercase
/// its own text (`.to_uppercase()`) and paint `ink3` -- this module only
/// owns type, not colour, and stays out of `ui::theme`'s door.
///
/// The +0.14em tracking DESIGN §3 asks for has no home: gpui 0.2.2 carries
/// no letter-spacing API at all (grepped `styled.rs`, `style.rs`,
/// `text_system.rs` -- no `letter_spacing` symbol anywhere in the crate), so
/// section heads render at zero tracking until gpui gains one or a caller
/// hand-kerns with inserted thin spaces (not done here: that would change
/// the text content, not just its style).
pub fn head() -> TypeStyle {
    label(SECTION_HEAD_PX, FontWeight::BOLD)
}

/// One face's worth of TTFs to embed, `(bytes, name)` for the doc comment
/// above `register` to explain if a load ever fails.
const EMBEDDED: &[(&[u8], &str)] = &[
    (
        include_bytes!("../../../../assets/fonts/Archivo-Regular.ttf"),
        "Archivo-Regular.ttf",
    ),
    (
        include_bytes!("../../../../assets/fonts/Archivo-Medium.ttf"),
        "Archivo-Medium.ttf",
    ),
    (
        include_bytes!("../../../../assets/fonts/Archivo-Bold.ttf"),
        "Archivo-Bold.ttf",
    ),
    (
        include_bytes!("../../../../assets/fonts/SplineSansMono-Medium.ttf"),
        "SplineSansMono-Medium.ttf",
    ),
    (
        include_bytes!("../../../../assets/fonts/SplineSansMono-Bold.ttf"),
        "SplineSansMono-Bold.ttf",
    ),
];

/// Registers both faces with gpui's text system, once, before the first
/// window opens. Bundled bytes (`include_bytes!`) rather than a runtime
/// read of `assets/` -- the binary carries its own type, full stop, so a
/// build copied off this machine still renders Archivo and Spline Sans Mono
/// with nothing installed system-side.
///
/// A registration failure (malformed font data) is a build-time bug, not a
/// runtime one -- these bytes are fixed at compile time -- so it panics
/// rather than falling silently back to a system face DESIGN §3 forbids.
pub fn register(cx: &App) {
    let fonts: Vec<Cow<'static, [u8]>> =
        EMBEDDED.iter().map(|(bytes, _)| Cow::Borrowed(*bytes)).collect();
    cx.text_system()
        .add_fonts(fonts)
        .expect("bundled Archivo / Spline Sans Mono TTFs failed to register");
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::FontStyle;

    /// The scale never drifts below DESIGN §3's floor, and role sizes match
    /// the numbers §3 names -- a call site changing `13.` to `12.` for the
    /// hero timecode is the design contract silently moving.
    #[test]
    fn the_scale_matches_design_3() {
        assert_eq!(HERO_TIMECODE_PX, 13.);
        assert_eq!(LABEL_ROW_PX, 10.5);
        assert!((9.5..=10.).contains(&CHORD_METADATA_MIN_PX));
        assert_eq!(SECTION_HEAD_PX, 9.);
        assert_eq!(FLOOR_PX, 8.);
        for size in [
            HERO_TIMECODE_PX,
            LABEL_ROW_PX,
            CHORD_METADATA_MAX_PX,
            CHORD_METADATA_MIN_PX,
            SECTION_HEAD_PX,
        ] {
            assert!(size >= FLOOR_PX, "{size} sits below the floor");
        }
    }

    /// Mono asks for tabular figures; Archivo never does (it carries no
    /// digit odometers) -- and neither ever asks for italic, which DESIGN
    /// §3 bans outright.
    #[test]
    fn mono_is_tabular_archivo_is_not_and_neither_is_ever_italic() {
        let m = mono(LABEL_ROW_PX, FontWeight::MEDIUM);
        assert_eq!(m.font.family.as_ref(), SPLINE_SANS_MONO);
        assert_eq!(m.font.features.tag_value_list(), &[("tnum".to_string(), 1)]);
        assert_eq!(m.font.style, FontStyle::Normal);

        let l = label(LABEL_ROW_PX, FontWeight::MEDIUM);
        assert_eq!(l.font.family.as_ref(), ARCHIVO);
        assert!(l.font.features.tag_value_list().is_empty());
        assert_eq!(l.font.style, FontStyle::Normal);
    }

    /// `head()` is exactly what DESIGN §3 names for section heads: Archivo,
    /// 700, 9px.
    #[test]
    fn head_is_archivo_bold_9px() {
        let h = head();
        assert_eq!(h.font.family.as_ref(), ARCHIVO);
        assert_eq!(h.font.weight, FontWeight::BOLD);
        assert_eq!(h.size, px(SECTION_HEAD_PX));
    }

    /// The five embedded faces are exactly the static weight instances the
    /// module doc explains the need for -- a fifth or sixth file added here
    /// without also landing in `assets/fonts/` is a build break, not a
    /// silent gap.
    #[test]
    fn five_static_weights_are_embedded() {
        assert_eq!(EMBEDDED.len(), 5);
        for (bytes, name) in EMBEDDED {
            assert!(!bytes.is_empty(), "{name} embedded empty");
        }
    }
}
