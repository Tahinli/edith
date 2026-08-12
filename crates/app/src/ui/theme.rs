//! The one place this editor knows a colour.
//!
//! Every paint in every region reads a token from this table and nothing else:
//! a `0x` literal anywhere but this file is the bug the whole redesign is about
//! (`no_colour_is_written_outside_the_theme` is the guard). Names say the
//! *role*, never the shade -- `bg/panel`, not "the dark grey" -- so the palette
//! can be swapped whole without touching a single element.
//!
//! Which is what the two modules below are for. Both families carry the same
//! role names, so the switch between them is one `use` and no call site knows
//! it happened -- and a role missing from either is a compile error rather than
//! a region that quietly kept the old colour.
//!
//! * `cool` (default) -- near-black ground, cool neutral chrome, one cyan accent.
//! * `warm` (`--features warm`) -- a deep neutral warmed towards brown so no
//!   surface reads as office grey, with one coral accent.
//!
//! Both take the clip bodies from the cross-NLE kind convention (video blue,
//! audio green, image teal, text purple) instead of four greys that differ by a
//! hair, and both are held to the same measured floors: the contrast guards in
//! `main.rs` run against whichever one is compiled.
//!
//! ponytail: a compile switch, because every token is a `const` and a runtime
//! palette means a lookup at every paint. Ceiling: the day the palette has to
//! change without a restart, these two modules become two `struct Palette`
//! values and the consts become fields on the one the window holds.

use engine::project::LaneKind;

#[cfg(not(feature = "warm"))]
pub use cool::*;
#[cfg(feature = "warm")]
pub use warm::*;

/// Family B: near-black ground, cool neutral chrome, one cyan accent.
pub mod cool {
    // -- surfaces ---------------------------------------------------------------
    /// The app's base, and the bed a picture is letterboxed against: darker than
    /// every panel, so the picture is what the eye lands on.
    pub const BG_CANVAS: u32 = 0x0b0d10;
    /// The library, the inspector, the toolbar: the chrome the work sits between.
    pub const BG_PANEL: u32 = 0x151a21;
    /// Buttons, menu bodies, cards -- one plane up from the panel. Flat: the
    /// separation is colour, never a shadow.
    pub const BG_RAISED: u32 = 0x212936;
    /// The lane bed, darker than the panel so a clip reads as an object on it.
    pub const BG_TIMELINE: u32 = 0x0e1218;
    /// One step lighter than whatever it sits on: the pointer's answer that this
    /// is clickable.
    pub const BG_HOVER: u32 = 0x2f3a4a;
    /// The same answer for the things that stand on the panel rather than on a
    /// button.
    pub const BG_HOVER_DIM: u32 = 0x1d2531;
    /// A picked row, a picked clip: the accent at surface brightness, so a
    /// selection is tinted rather than lit up.
    pub const BG_SELECTED: u32 = 0x0d4b5c;
    /// What a card is floated over when it is genuinely modal (the export, the
    /// actions list). Inspector sections never draw one -- they occlude nothing.
    pub const SCRIM: u32 = 0x0b0d10cc;
    /// The lighter scrim: the picker list floats over a card that is still being
    /// read, so it dims rather than hides.
    pub const SCRIM_LIGHT: u32 = 0x0b0d1055;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x2a3442;
    /// The keyboard's own ring, distinct from selection on purpose: focus is
    /// where the next stroke lands, selection is what an edit acts on.
    pub const STROKE_FOCUS: u32 = 0xffd166;
    pub const STROKE_SELECTED: u32 = 0x22d3ee;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xe9eff7;
    /// Shortcuts, dismissal hints, detail lines. Past 4.5:1 on every surface
    /// above.
    pub const FG_SECONDARY: u32 = 0xa7b6c9;
    pub const FG_DISABLED: u32 = 0x6c7a8b;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0x22d3ee;
    /// The accent under the pointer: lighter, still the accent.
    pub const ACCENT_HOVER: u32 = 0x67e8f9;
    /// Not the accent: the playhead crosses every clip colour there is and has
    /// to stay the one line that is none of them.
    pub const ACCENT_PLAYHEAD: u32 = 0xff9db0;
    /// The accent as a translucent wash, for the marks drawn over a clip body.
    pub const ACCENT_WASH: u32 = 0x22d3eeaa;

    // -- clip kinds (cross-NLE convention) --------------------------------------
    pub const CLIP_VIDEO: u32 = 0x2b5fa8;
    pub const CLIP_AUDIO: u32 = 0x276b43;
    pub const CLIP_IMAGE: u32 = 0x1a6a6a;
    pub const CLIP_TEXT: u32 = 0x6b46c1;

    /// One per source, so a clip that came from an imported file reads as coming
    /// from somewhere else than its neighbour. The *kind* is the body colour now
    /// ([`super::clip_kind`]); this is the identity stripe and the library
    /// swatch, and it only has to be four things telling each other apart.
    pub const SOURCE_TINTS: [u32; 4] = [0x4f8fd6, 0xd69a4f, 0x4fd6a8, 0xb14fd6];

    // -- feedback ---------------------------------------------------------------
    pub const STATUS_ERROR: u32 = 0xef4444;
    pub const STATUS_WARNING: u32 = 0xf59e0b;
    pub const STATUS_SUCCESS: u32 = 0x34d399;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    /// The mirror of [`BG_SELECTED`]: a drop the lane will not take, tinting the
    /// shadow the drag draws so a refusal is seen before the release.
    pub const DROP_REFUSE: u32 = 0x8f2740;

    // -- subtitles (drawn over the picture, so they own their own contrast) -----
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x2a3442;
    pub const EQ_SPECTRUM_INK: u32 = 0x7f95ad66;
    pub const EQ_FILL_INK: u32 = 0x22d3ee26;
    pub const EQ_BELL_INK: u32 = 0x22d3ee66;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
}

/// Family A: a warm consumer-editor ground (Movavi's bed, CapCut's saturation)
/// with one coral accent that is never used for anything but "this is live,
/// press this". Same roles, same measured floors, different numbers.
pub mod warm {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x0c0a09;
    pub const BG_PANEL: u32 = 0x1d1917;
    pub const BG_RAISED: u32 = 0x383029;
    pub const BG_TIMELINE: u32 = 0x151211;
    pub const BG_HOVER: u32 = 0x4a413b;
    pub const BG_HOVER_DIM: u32 = 0x241f1d;
    /// Darker than the family it came from: at V3's own `0x7a3a1c` the secondary
    /// ink on a picked row measures 4.09:1 and WCAG 1.4.3 wants 4.5, which the
    /// cool table's guard caught the moment this became a second palette.
    pub const BG_SELECTED: u32 = 0x6a3118;
    pub const SCRIM: u32 = 0x0c0a09cc;
    pub const SCRIM_LIGHT: u32 = 0x0c0a0955;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x3a322e;
    pub const STROKE_FOCUS: u32 = 0xffc38a;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xf2ece6;
    pub const FG_SECONDARY: u32 = 0xbdb1a8;
    pub const FG_DISABLED: u32 = 0x8a7f78;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0xff7a45;
    pub const ACCENT_HOVER: u32 = 0xff9a6e;
    /// Warmer still than the accent, because it crosses everything the accent is
    /// drawn on and has to stay findable over a selected clip.
    pub const ACCENT_PLAYHEAD: u32 = 0xffc23a;
    pub const ACCENT_WASH: u32 = 0xff7a45aa;

    // -- clip kinds (the same cross-NLE convention, at this family's weight) ----
    pub const CLIP_VIDEO: u32 = 0x2b4c7e;
    pub const CLIP_AUDIO: u32 = 0x2c5a3a;
    pub const CLIP_IMAGE: u32 = 0x1f5560;
    pub const CLIP_TEXT: u32 = 0x4a3070;

    /// Warm-side identity stripes: the same four-way distinctness the cool
    /// family's are held to, in hues that belong to this ground.
    pub const SOURCE_TINTS: [u32; 4] = [0xe08a4f, 0x4fa8d6, 0x8fc94f, 0xd67aa8];

    // -- feedback ---------------------------------------------------------------
    pub const STATUS_ERROR: u32 = 0xc85050;
    pub const STATUS_WARNING: u32 = 0xe0a53a;
    pub const STATUS_SUCCESS: u32 = 0x4fbf72;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x8a2f24;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x4a413b;
    pub const EQ_SPECTRUM_INK: u32 = 0xbdb1a866;
    pub const EQ_FILL_INK: u32 = 0xff7a4526;
    pub const EQ_BELL_INK: u32 = 0xff7a4566;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
}

/// The box a button's glyph sits in, whatever the glyph happens to measure: the
/// pause bars are 12 px wide and the play triangle 11, and a button that resized
/// itself when it was pressed moved every button to its right.
pub const GLYPH_SLOT: f32 = 14.;

/// How solid the shadow a drag draws is (`0xRRGGBBAA`): enough to read as a box,
/// little enough that the clip under it is still legible. Not a colour, so both
/// families share it.
pub const GHOST_ALPHA: u32 = 0x66;

/// What a clip on `kind` is painted: the timeline's whole colour language in
/// one call, so a lane added later cannot invent a shade of its own.
pub const fn clip_kind(kind: LaneKind, image: bool) -> u32 {
    match kind {
        LaneKind::Audio => CLIP_AUDIO,
        LaneKind::Video if image => CLIP_IMAGE,
        LaneKind::Video => CLIP_VIDEO,
    }
}

/// Which of the feedback colours a message wears. Read off the words rather
/// than carried alongside them: every message in this editor already opens with
/// what it is ("EXPORT DONE", "SCAN FAILED", "NOTHING DETACHED"), and a second
/// `tone` argument at seventy call sites is seventy chances to disagree with the
/// sentence it labels.
///
/// ponytail: prefix matching, so a message worded outside these families reads
/// as neutral rather than wrong. Ceiling: a `Notice { text, tone }` struct the
/// day a message needs a colour its own words do not say.
pub fn notice_tone(message: &str) -> u32 {
    let has = |word: &str| message.contains(word);
    if has("FAILED") || has("ERROR") || has("REFUSED") || has("CANNOT") || has("COULD NOT") {
        STATUS_ERROR
    } else if message.starts_with(crate::EXPORT_DONE) || has("SAVED") || has("DONE") {
        STATUS_SUCCESS
    } else if has("NOTHING") || has("NO ") || has("EMPTY") {
        STATUS_WARNING
    } else {
        ACCENT_PRIMARY
    }
}

// -- geometry the four regions are laid out on --------------------------------
/// Compact density: 11-13 px text, 28 px controls, 8 px gutters. The floor the
/// whole window is measured at is 640x360, and airy spacing loses a region.
pub const INSPECTOR_MIN_W: f32 = 208.;
pub const INSPECTOR_MAX_W: f32 = 320.;
pub const INSPECTOR_FRAC: f32 = 0.24;

/// The inspector's width at this window width, on the library's own rule: a
/// share of the window, clamped so the picture keeps the middle at every size.
pub fn inspector_w(window_w: f32) -> f32 {
    (window_w * INSPECTOR_FRAC).clamp(INSPECTOR_MIN_W, INSPECTOR_MAX_W)
}
