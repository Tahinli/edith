//! The one place this editor knows a colour.
//!
//! Every paint in every region reads a token from this table and nothing else:
//! a `0x` literal anywhere but this file is the bug the whole redesign is about
//! (`no_colour_is_written_outside_the_theme` is the guard). Names say the
//! *role*, never the shade -- `bg/panel`, not "the dark grey" -- so the palette
//! can be swapped whole without touching a single element.
//!
//! Which is what the modules below are for. Every family carries the same role
//! names, so the switch between them is one field lookup and no call site knows
//! it happened -- and a role missing from any of them is a compile error rather
//! than a region that quietly kept the old colour.
//!
//! * `cool` (default) -- near-black ground, cool neutral chrome, one cyan accent.
//! * `warm` -- a deep neutral warmed towards brown so no surface reads as
//!   office grey, with one coral accent.
//! * `forest` -- a green-tinted ground under one emerald accent.
//! * `violet` -- an indigo ground under one lavender accent.
//! * `rose` -- a neutral ground the accent alone warms, in rose.
//! * `amber` -- a graphite ground under one gold accent.
//! * `ocean` -- a blue-black ground under one azure accent.
//! * `ice` -- the crisp one: a near-black blue ground, pale ice-blue accent.
//! * `orchid` -- a dark plum ground under one orchid accent.
//! * `nord`, `gruvbox`, `dracula` -- the three open palettes people already
//!   have their editors in, mapped onto these roles rather than approximated.
//!
//! All twelve are dark: colour work is judged against its surround, and a light
//! ground pushes every clip body and every graded frame the wrong way -- which
//! is why no editor of this kind ships one. All twelve take the clip bodies from
//! the cross-NLE kind convention (video blue, audio green, image teal, text
//! purple) instead of four greys that differ by a hair, and all twelve are held
//! to the same measured floors: the contrast and tint guards in `main.rs` run
//! against *every* palette in [`PaletteId::ALL`], so the thirteenth lands gated
//! rather than untested.
//!
//! Which family is in force is the user's, picked from the toolbar's Theme
//! button or its stroke and kept in `~/.config/edith/theme` beside the
//! keybindings -- it used to be a compile feature, which is a choice only
//! whoever built the binary could make. A token is a call now
//! ([`BG_PANEL()`]): one relaxed load and one field read off a `'static`
//! table, which is what "the palette can be swapped whole" costs at paint time.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// The whole table, once: every role and its type, gathered from each family's
/// module below. One list, so a role added to [`Palette`] without a value in
/// every family is a compile error, and a role read at a paint cannot come from
/// anywhere but the palette in force.
///
/// corner-cut: the families are spelled out in [`PALETTES`] rather than derived
/// from a second list -- `macro_rules!` cannot nest a repetition over families
/// inside one over roles, and the escape hatch for that is unstable. Ceiling: a
/// thirteenth family is one line there, one variant in [`PaletteId`], one
/// module.
macro_rules! palette {
    ($($name:ident: $ty:ty),+ $(,)?) => {
        /// One family's numbers, in a struct so the set can be swapped whole.
        /// Field names are the role names the whole editor already paints by,
        /// which is why they shout.
        #[allow(non_snake_case)]
        #[derive(Clone, Copy, PartialEq, Debug)]
        pub struct Palette { $(pub $name: $ty,)+ }

        /// The families in [`PaletteId::ALL`]'s order, so the index the atomic
        /// holds is the id itself.
        static PALETTES: [Palette; PaletteId::ALL.len()] = [
            Palette { $($name: cool::$name,)+ },
            Palette { $($name: warm::$name,)+ },
            Palette { $($name: forest::$name,)+ },
            Palette { $($name: violet::$name,)+ },
            Palette { $($name: rose::$name,)+ },
            Palette { $($name: amber::$name,)+ },
            Palette { $($name: ocean::$name,)+ },
            Palette { $($name: ice::$name,)+ },
            Palette { $($name: orchid::$name,)+ },
            Palette { $($name: nord::$name,)+ },
            Palette { $($name: gruvbox::$name,)+ },
            Palette { $($name: dracula::$name,)+ },
            Palette { $($name: darkroom::$name,)+ },
        ];

        $(
            #[allow(non_snake_case)]
            // corner-cut: the Darkroom substrate roles (INK1.. SAFELIGHT_GLYPH)
            // have no call site yet -- this task owns theme.rs only and wires
            // the table, not the paints. Ceiling: drop this attribute once the
            // render-side task in the same DESIGN §12 package reads them.
            #[allow(dead_code)]
            #[inline]
            pub fn $name() -> $ty { palette().$name }
        )+
    };
}

palette! {
    BG_CANVAS: u32,
    BG_PANEL: u32,
    BG_RAISED: u32,
    BG_TIMELINE: u32,
    BG_HOVER: u32,
    BG_HOVER_DIM: u32,
    BG_SELECTED: u32,
    SCRIM: u32,
    SCRIM_LIGHT: u32,
    STROKE_DIVIDER: u32,
    STROKE_FOCUS: u32,
    STROKE_SELECTED: u32,
    FG_PRIMARY: u32,
    FG_SECONDARY: u32,
    FG_DISABLED: u32,
    ACCENT_PRIMARY: u32,
    ACCENT_HOVER: u32,
    ACCENT_PLAYHEAD: u32,
    ACCENT_WASH: u32,
    CLIP_VIDEO: u32,
    CLIP_AUDIO: u32,
    CLIP_IMAGE: u32,
    CLIP_TEXT: u32,
    SOURCE_TINTS: [u32; 12],
    STATUS_ERROR: u32,
    STATUS_WARNING: u32,
    STATUS_SUCCESS: u32,
    STATUS_PROGRESS: u32,
    DROP_REFUSE: u32,
    SUB_FG: u32,
    SUB_SHADE: u32,
    EQ_GRID: u32,
    EQ_SPECTRUM_INK: u32,
    EQ_FILL_INK: u32,
    EQ_BELL_INK: u32,
    HIST_INK: [u32; 3],
    // -- Darkroom token substrate (DESIGN.md §2) --------------------------------
    INK1: u32,
    INK2: u32,
    INK3: u32,
    INK4: u32,
    DARK_CANVAS: u32,
    DARK_PANEL: u32,
    DARK_RAISED: u32,
    DARK_HAIRLINE: u32,
    DARK_SEAM: u32,
    LAMP_WHITE: u32,
    NOTICE_TELL: u32,
    NOTICE_LOOK: u32,
    NOTICE_DECIDE: u32,
    SAFELIGHT_GROUND: u32,
    SAFELIGHT_GLYPH: u32,
}

/// Which family a person picked. An enum and not a bool: the door in the
/// toolbar is a list rather than a toggle precisely because the next family
/// must cost one variant here, one module below, and nothing anywhere else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteId {
    Cool,
    Warm,
    Forest,
    Violet,
    Rose,
    Amber,
    Ocean,
    Ice,
    Orchid,
    Nord,
    Gruvbox,
    Dracula,
    Darkroom,
}

impl PaletteId {
    /// Display order -- the picker lists them in it, and the index into
    /// [`PALETTES`] *is* the position in it, so this order and that array are
    /// the same order or every colour is wrong.
    pub const ALL: [PaletteId; 13] = [
        PaletteId::Cool,
        PaletteId::Warm,
        PaletteId::Forest,
        PaletteId::Violet,
        PaletteId::Rose,
        PaletteId::Amber,
        PaletteId::Ocean,
        PaletteId::Ice,
        PaletteId::Orchid,
        PaletteId::Nord,
        PaletteId::Gruvbox,
        PaletteId::Dracula,
        PaletteId::Darkroom,
    ];

    /// What the button and the picker row call it.
    pub fn label(self) -> &'static str {
        match self {
            PaletteId::Cool => "Cool",
            PaletteId::Warm => "Warm",
            PaletteId::Forest => "Forest",
            PaletteId::Violet => "Violet",
            PaletteId::Rose => "Rose",
            PaletteId::Amber => "Amber",
            PaletteId::Ocean => "Ocean",
            PaletteId::Ice => "Ice",
            PaletteId::Orchid => "Orchid",
            PaletteId::Nord => "Nord",
            PaletteId::Gruvbox => "Gruvbox",
            PaletteId::Dracula => "Dracula",
            PaletteId::Darkroom => "Darkroom",
        }
    }

    /// The small print beside the row: what the family actually looks like, so
    /// the choice is made before the click rather than after it. Short enough
    /// to sit in the list's right-hand column at the 640x360 floor -- past
    /// thirty characters it is truncated mid-word, which is a description that
    /// describes nothing.
    pub fn detail(self) -> &'static str {
        match self {
            PaletteId::Cool => "near-black ground, cyan accent",
            PaletteId::Warm => "warm ground, coral accent",
            PaletteId::Forest => "green ground, emerald",
            PaletteId::Violet => "indigo ground, lavender",
            PaletteId::Rose => "neutral ground, rose",
            PaletteId::Amber => "graphite ground, gold",
            PaletteId::Ocean => "blue-black ground, azure",
            PaletteId::Ice => "near-black blue, pale ice",
            PaletteId::Orchid => "plum ground, orchid accent",
            PaletteId::Nord => "polar night, frost accent",
            PaletteId::Gruvbox => "retro brown, gruvbox orange",
            PaletteId::Dracula => "dracula ground, pink accent",
            PaletteId::Darkroom => "dark ground, no chrome hue",
        }
    }

    /// What the file calls it: one word, never the label, which is free to be
    /// reworded ([`crate::keymap`]'s rule for the same reason).
    pub fn name(self) -> &'static str {
        match self {
            PaletteId::Cool => "cool",
            PaletteId::Warm => "warm",
            PaletteId::Forest => "forest",
            PaletteId::Violet => "violet",
            PaletteId::Rose => "rose",
            PaletteId::Amber => "amber",
            PaletteId::Ocean => "ocean",
            PaletteId::Ice => "ice",
            PaletteId::Orchid => "orchid",
            PaletteId::Nord => "nord",
            PaletteId::Gruvbox => "gruvbox",
            PaletteId::Dracula => "dracula",
            PaletteId::Darkroom => "darkroom",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The numbers themselves, for anything that has to measure a family that
    /// is not the one in force -- which is only the contrast guards, so it sits
    /// with them rather than in the binary.
    #[cfg(test)]
    pub fn palette(self) -> &'static Palette {
        &PALETTES[self as usize]
    }
}

/// The family in force, as an index into [`PALETTES`]. An atomic because a
/// token is read from whatever thread happens to be painting and the value is
/// one `usize`; relaxed because nothing is published *with* it -- the next
/// repaint reads it, and the swap asks for that repaint itself.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// The numbers every token function reads. One load, one bounds-checked index
/// into a `'static` table: no allocation, no hashing, no lock.
#[inline]
pub fn palette() -> &'static Palette {
    PALETTES.get(ACTIVE.load(Relaxed)).unwrap_or(&PALETTES[0])
}

/// Which family is in force -- what the button says and the picker marks.
pub fn active() -> PaletteId {
    PaletteId::ALL
        .get(ACTIVE.load(Relaxed))
        .copied()
        .unwrap_or(PaletteId::Cool)
}

/// Puts `id` in force. Every paint after this reads the new numbers; asking for
/// the repaint is the caller's, since only it holds the window.
pub fn set(id: PaletteId) {
    ACTIVE.store(id as usize, Relaxed);
}

/// Where the pick lives: one word in a file beside the keybindings, so a
/// desktop that names an XDG directory is obeyed for both.
pub fn config_path() -> PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("theme")
}

/// The pick from the last session, if there was one. Anything unreadable or
/// unknown leaves the default in force: a theme file is not the user's work,
/// so a bad one is worth no message at startup.
pub fn load() {
    if let Ok(text) = std::fs::read_to_string(config_path())
        && let Some(id) = PaletteId::from_name(text.trim())
    {
        set(id);
    }
}

/// Writes the pick. One word, written whole -- a torn write costs the theme and
/// nothing else, and [`load`] falls back to the default on one.
pub fn save(id: PaletteId) -> std::io::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{}\n", id.name()))
}

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
    pub const SOURCE_TINTS: [u32; 12] = [
        0x4f8fd6, 0xd69a4f, 0x4fd6a8, 0xb14fd6, 0x4f8fd6, 0xd69a4f, 0x4fd6a8, 0xb14fd6, 0x4f8fd6,
        0xd69a4f, 0x4fd6a8, 0xb14fd6,
    ];

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
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
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
    pub const SOURCE_TINTS: [u32; 12] = [
        0xe08a4f, 0x4fa8d6, 0x8fc94f, 0xd67aa8, 0xe08a4f, 0x4fa8d6, 0x8fc94f, 0xd67aa8, 0xe08a4f,
        0x4fa8d6, 0x8fc94f, 0xd67aa8,
    ];

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
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family C: the ground itself green, dark enough that a graded frame is still
/// the brightest thing on screen, with one emerald accent. The playhead goes
/// pink here for the same reason it is pink in `cool`: it crosses the emerald
/// and every clip body, so it may belong to none of them.
pub mod forest {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x080d0a;
    pub const BG_PANEL: u32 = 0x121b16;
    pub const BG_RAISED: u32 = 0x1e2c24;
    pub const BG_TIMELINE: u32 = 0x0b1310;
    pub const BG_HOVER: u32 = 0x2c4034;
    pub const BG_HOVER_DIM: u32 = 0x1a2620;
    /// Deeper than the accent it is made of: the dim ink on a picked row is held
    /// to 4.5:1 like every other family's (WCAG 1.4.3), and emerald at surface
    /// brightness will not carry it.
    pub const BG_SELECTED: u32 = 0x14503a;
    pub const SCRIM: u32 = 0x080d0acc;
    pub const SCRIM_LIGHT: u32 = 0x080d0a55;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x25362c;
    pub const STROKE_FOCUS: u32 = 0xffd166;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xe7f3eb;
    pub const FG_SECONDARY: u32 = 0xa8c4b4;
    pub const FG_DISABLED: u32 = 0x6e8579;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0x34d399;
    pub const ACCENT_HOVER: u32 = 0x6ee7b7;
    pub const ACCENT_PLAYHEAD: u32 = 0xff9db0;
    pub const ACCENT_WASH: u32 = 0x34d399aa;

    // -- clip kinds (the cross-NLE convention, at this family's weight) ---------
    pub const CLIP_VIDEO: u32 = 0x2b5fa8;
    /// Darker than the other families' green: on a green ground a clip body has
    /// to be the object and the bed the hole, and the dim ink on it still owes
    /// 3:1 (WCAG 1.4.11).
    pub const CLIP_AUDIO: u32 = 0x256b44;
    pub const CLIP_IMAGE: u32 = 0x1a6a6a;
    pub const CLIP_TEXT: u32 = 0x6b46c1;

    pub const SOURCE_TINTS: [u32; 12] = [
        0x5fd68f, 0xd6a45f, 0x5fa8d6, 0xc15fd6, 0x5fd68f, 0xd6a45f, 0x5fa8d6, 0xc15fd6, 0x5fd68f,
        0xd6a45f, 0x5fa8d6, 0xc15fd6,
    ];

    // -- feedback ---------------------------------------------------------------
    /// Not the accent family: a success that wore the emerald would say "this is
    /// live" in the same colour the whole window says it in.
    pub const STATUS_ERROR: u32 = 0xf07171;
    pub const STATUS_WARNING: u32 = 0xe3b341;
    pub const STATUS_SUCCESS: u32 = 0x7bd88f;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x7a2a33;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x25362c;
    pub const EQ_SPECTRUM_INK: u32 = 0xa8c4b466;
    pub const EQ_FILL_INK: u32 = 0x34d39926;
    pub const EQ_BELL_INK: u32 = 0x34d39966;
    /// The three channel inks are the picture's own R, G and B: they mean the
    /// same thing in every family, so they are the same numbers in every family.
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family D: an indigo ground with one lavender accent -- the cold end of the
/// set, where `cool` is neutral-cold and this one is frankly blue. The playhead
/// is the yellow that is furthest from everything in it.
pub mod violet {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x0a0a12;
    pub const BG_PANEL: u32 = 0x171827;
    pub const BG_RAISED: u32 = 0x252842;
    pub const BG_TIMELINE: u32 = 0x0e0f1b;
    pub const BG_HOVER: u32 = 0x353a5e;
    pub const BG_HOVER_DIM: u32 = 0x1f2138;
    pub const BG_SELECTED: u32 = 0x3b2a78;
    pub const SCRIM: u32 = 0x0a0a12cc;
    pub const SCRIM_LIGHT: u32 = 0x0a0a1255;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x2e3150;
    pub const STROKE_FOCUS: u32 = 0xfde68a;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xeceafb;
    pub const FG_SECONDARY: u32 = 0xb6b4d8;
    pub const FG_DISABLED: u32 = 0x7b79a0;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0xa78bfa;
    pub const ACCENT_HOVER: u32 = 0xc4b5fd;
    pub const ACCENT_PLAYHEAD: u32 = 0xfacc15;
    pub const ACCENT_WASH: u32 = 0xa78bfaaa;

    // -- clip kinds -------------------------------------------------------------
    pub const CLIP_VIDEO: u32 = 0x2f5a9e;
    pub const CLIP_AUDIO: u32 = 0x2c6b49;
    pub const CLIP_IMAGE: u32 = 0x1c6270;
    /// A shade off the accent on purpose: a text clip and "press this" must not
    /// be the same purple.
    pub const CLIP_TEXT: u32 = 0x6d3fa8;

    pub const SOURCE_TINTS: [u32; 12] = [
        0x9b7fe8, 0xe8a97f, 0x7fc8e8, 0xe87fb4, 0x9b7fe8, 0xe8a97f, 0x7fc8e8, 0xe87fb4, 0x9b7fe8,
        0xe8a97f, 0x7fc8e8, 0xe87fb4,
    ];

    // -- feedback ---------------------------------------------------------------
    pub const STATUS_ERROR: u32 = 0xf87171;
    pub const STATUS_WARNING: u32 = 0xfbbf24;
    pub const STATUS_SUCCESS: u32 = 0x6ee7b7;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x7c2440;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x2e3150;
    pub const EQ_SPECTRUM_INK: u32 = 0xb6b4d866;
    pub const EQ_FILL_INK: u32 = 0xa78bfa26;
    pub const EQ_BELL_INK: u32 = 0xa78bfa66;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family E: a ground with no cast at all -- the neutral one, for grading, where
/// nothing but the accent has a hue. That accent is rose, and the playhead is
/// the cyan on the other side of the wheel from it.
pub mod rose {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x0c0b0c;
    pub const BG_PANEL: u32 = 0x1a181a;
    pub const BG_RAISED: u32 = 0x2a272a;
    pub const BG_TIMELINE: u32 = 0x121012;
    pub const BG_HOVER: u32 = 0x3d383d;
    pub const BG_HOVER_DIM: u32 = 0x221f22;
    pub const BG_SELECTED: u32 = 0x6b1f38;
    pub const SCRIM: u32 = 0x0c0b0ccc;
    pub const SCRIM_LIGHT: u32 = 0x0c0b0c55;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x332f33;
    pub const STROKE_FOCUS: u32 = 0xfcd34d;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xf3edef;
    pub const FG_SECONDARY: u32 = 0xc0b4b8;
    pub const FG_DISABLED: u32 = 0x8a7e82;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0xfb7185;
    pub const ACCENT_HOVER: u32 = 0xfda4af;
    pub const ACCENT_PLAYHEAD: u32 = 0x67e8f9;
    pub const ACCENT_WASH: u32 = 0xfb7185aa;

    // -- clip kinds -------------------------------------------------------------
    pub const CLIP_VIDEO: u32 = 0x2f5a9e;
    pub const CLIP_AUDIO: u32 = 0x2c6b49;
    pub const CLIP_IMAGE: u32 = 0x1c6270;
    pub const CLIP_TEXT: u32 = 0x6b46c1;

    pub const SOURCE_TINTS: [u32; 12] = [
        0xe87f97, 0x7fc8e8, 0xe8c87f, 0x9b7fe8, 0xe87f97, 0x7fc8e8, 0xe8c87f, 0x9b7fe8, 0xe87f97,
        0x7fc8e8, 0xe8c87f, 0x9b7fe8,
    ];

    // -- feedback ---------------------------------------------------------------
    /// Brighter and more saturated than the accent it sits near on the wheel: on
    /// a rose window a failure has to be louder than "this is live", and the
    /// message's own first word says which it is either way ([`super::notice_tone`]).
    pub const STATUS_ERROR: u32 = 0xff4d4d;
    pub const STATUS_WARNING: u32 = 0xfacc15;
    pub const STATUS_SUCCESS: u32 = 0x4ade80;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x8f2740;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x332f33;
    pub const EQ_SPECTRUM_INK: u32 = 0xc0b4b866;
    pub const EQ_FILL_INK: u32 = 0xfb718526;
    pub const EQ_BELL_INK: u32 = 0xfb718566;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family F: a graphite ground -- neutral, a touch warm -- under one gold
/// accent.
pub mod amber {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x0d0d0c;
    pub const BG_PANEL: u32 = 0x1b1b19;
    pub const BG_RAISED: u32 = 0x2b2b27;
    pub const BG_TIMELINE: u32 = 0x131311;
    pub const BG_HOVER: u32 = 0x3e3e38;
    pub const BG_HOVER_DIM: u32 = 0x232320;
    /// Gold at surface brightness carries neither ink at 4.5:1, so the picked
    /// row is the accent taken down to a bronze.
    pub const BG_SELECTED: u32 = 0x5e3a0b;
    pub const SCRIM: u32 = 0x0d0d0ccc;
    pub const SCRIM_LIGHT: u32 = 0x0d0d0c55;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x35352f;
    pub const STROKE_FOCUS: u32 = 0x7dd3fc;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xf2f0e9;
    pub const FG_SECONDARY: u32 = 0xbdbaae;
    pub const FG_DISABLED: u32 = 0x87857c;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0xfbbf24;
    pub const ACCENT_HOVER: u32 = 0xfcd34d;
    pub const ACCENT_PLAYHEAD: u32 = 0x93c5fd;
    pub const ACCENT_WASH: u32 = 0xfbbf24aa;

    // -- clip kinds -------------------------------------------------------------
    pub const CLIP_VIDEO: u32 = 0x2f5a9e;
    pub const CLIP_AUDIO: u32 = 0x2c6b49;
    pub const CLIP_IMAGE: u32 = 0x1c6270;
    pub const CLIP_TEXT: u32 = 0x6b46c1;

    pub const SOURCE_TINTS: [u32; 12] = [
        0xe8b45f, 0x5fa8e8, 0x7fd69b, 0xd68fc1, 0xe8b45f, 0x5fa8e8, 0x7fd69b, 0xd68fc1, 0xe8b45f,
        0x5fa8e8, 0x7fd69b, 0xd68fc1,
    ];

    // -- feedback ---------------------------------------------------------------
    /// The warning steps to orange: amber *is* the accent here, and a warning
    /// wearing it would be indistinguishable from every live control.
    pub const STATUS_ERROR: u32 = 0xf05252;
    pub const STATUS_WARNING: u32 = 0xfb923c;
    pub const STATUS_SUCCESS: u32 = 0x4ade80;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x7a2f24;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x35352f;
    pub const EQ_SPECTRUM_INK: u32 = 0xbdbaae66;
    pub const EQ_FILL_INK: u32 = 0xfbbf2426;
    pub const EQ_BELL_INK: u32 = 0xfbbf2466;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family G: a blue-black ground -- deeper and bluer than `cool`'s neutral --
/// under one azure accent. Where `cool` is a grey that leans cold, this one is
/// water: the panels themselves carry the hue and the accent is the light on it.
pub mod ocean {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x05080f;
    pub const BG_PANEL: u32 = 0x0d1524;
    pub const BG_RAISED: u32 = 0x172436;
    pub const BG_TIMELINE: u32 = 0x08101c;
    pub const BG_HOVER: u32 = 0x223449;
    pub const BG_HOVER_DIM: u32 = 0x14202f;
    pub const BG_SELECTED: u32 = 0x0f3a6b;
    pub const SCRIM: u32 = 0x05080fcc;
    pub const SCRIM_LIGHT: u32 = 0x05080f55;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x1e2d42;
    pub const STROKE_FOCUS: u32 = 0xffd166;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xe6eefb;
    pub const FG_SECONDARY: u32 = 0xa4bcd8;
    pub const FG_DISABLED: u32 = 0x6b8199;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0x38bdf8;
    pub const ACCENT_HOVER: u32 = 0x7dd3fc;
    pub const ACCENT_PLAYHEAD: u32 = 0xff9db0;
    pub const ACCENT_WASH: u32 = 0x38bdf8aa;

    // -- clip kinds -------------------------------------------------------------
    pub const CLIP_VIDEO: u32 = 0x2f5a9e;
    pub const CLIP_AUDIO: u32 = 0x2c6b49;
    pub const CLIP_IMAGE: u32 = 0x1c6270;
    pub const CLIP_TEXT: u32 = 0x6b46c1;

    pub const SOURCE_TINTS: [u32; 12] = [
        0x5fa8e8, 0xe8a95f, 0x5fd6a0, 0xc98fe8, 0x5fa8e8, 0xe8a95f, 0x5fd6a0, 0xc98fe8, 0x5fa8e8,
        0xe8a95f, 0x5fd6a0, 0xc98fe8,
    ];

    // -- feedback ---------------------------------------------------------------
    pub const STATUS_ERROR: u32 = 0xf05252;
    pub const STATUS_WARNING: u32 = 0xfbbf24;
    pub const STATUS_SUCCESS: u32 = 0x4ade80;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x8a2740;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x1e2d42;
    pub const EQ_SPECTRUM_INK: u32 = 0xa4bcd866;
    pub const EQ_FILL_INK: u32 = 0x38bdf826;
    pub const EQ_BELL_INK: u32 = 0x38bdf866;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family H: the crisp one. A near-black blue ground with the widest ink range
/// in the set -- the primary text is nearly white and the canvas nearly black --
/// under a pale ice-blue accent, for the long grade where every other family
/// starts to read as soft.
pub mod ice {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x03060a;
    pub const BG_PANEL: u32 = 0x0b131c;
    pub const BG_RAISED: u32 = 0x18232f;
    pub const BG_TIMELINE: u32 = 0x060d15;
    pub const BG_HOVER: u32 = 0x24323f;
    pub const BG_HOVER_DIM: u32 = 0x121c26;
    /// A steel blue rather than the accent: ice at surface brightness is nearly
    /// white and would carry no ink at all (WCAG 1.4.3).
    pub const BG_SELECTED: u32 = 0x14476b;
    pub const SCRIM: u32 = 0x03060acc;
    pub const SCRIM_LIGHT: u32 = 0x03060a55;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x223140;
    pub const STROKE_FOCUS: u32 = 0xfcd34d;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xf4fafe;
    pub const FG_SECONDARY: u32 = 0xb8cfe0;
    pub const FG_DISABLED: u32 = 0x71889b;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0xa5f3fc;
    pub const ACCENT_HOVER: u32 = 0xcffafe;
    pub const ACCENT_PLAYHEAD: u32 = 0xff8fa3;
    pub const ACCENT_WASH: u32 = 0xa5f3fcaa;

    // -- clip kinds -------------------------------------------------------------
    pub const CLIP_VIDEO: u32 = 0x2f5a9e;
    pub const CLIP_AUDIO: u32 = 0x2c6b49;
    pub const CLIP_IMAGE: u32 = 0x1c6270;
    pub const CLIP_TEXT: u32 = 0x6b46c1;

    pub const SOURCE_TINTS: [u32; 12] = [
        0x7fd6e8, 0xe8b47f, 0x8fd69b, 0xc79be8, 0x7fd6e8, 0xe8b47f, 0x8fd69b, 0xc79be8, 0x7fd6e8,
        0xe8b47f, 0x8fd69b, 0xc79be8,
    ];

    // -- feedback ---------------------------------------------------------------
    pub const STATUS_ERROR: u32 = 0xff6b6b;
    pub const STATUS_WARNING: u32 = 0xfbbf24;
    pub const STATUS_SUCCESS: u32 = 0x4ade80;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x8f2740;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x223140;
    pub const EQ_SPECTRUM_INK: u32 = 0xb8cfe066;
    pub const EQ_FILL_INK: u32 = 0xa5f3fc26;
    pub const EQ_BELL_INK: u32 = 0xa5f3fc66;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family I: a dark plum ground under one orchid accent -- the warm end of the
/// purples, where `violet` is the cold one. The text clip is pushed towards
/// indigo here for `violet`'s reason: a text clip and "press this" may not be
/// the same purple.
pub mod orchid {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x0c0712;
    pub const BG_PANEL: u32 = 0x1a0f24;
    pub const BG_RAISED: u32 = 0x2a1a38;
    pub const BG_TIMELINE: u32 = 0x120a19;
    pub const BG_HOVER: u32 = 0x3b2650;
    pub const BG_HOVER_DIM: u32 = 0x231530;
    pub const BG_SELECTED: u32 = 0x6b1f5e;
    pub const SCRIM: u32 = 0x0c0712cc;
    pub const SCRIM_LIGHT: u32 = 0x0c071255;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x33203f;
    pub const STROKE_FOCUS: u32 = 0x7dd3fc;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xf6ecf8;
    pub const FG_SECONDARY: u32 = 0xc7b0cf;
    pub const FG_DISABLED: u32 = 0x8e7a97;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0xd946ef;
    pub const ACCENT_HOVER: u32 = 0xe879f9;
    pub const ACCENT_PLAYHEAD: u32 = 0xfbbf24;
    pub const ACCENT_WASH: u32 = 0xd946efaa;

    // -- clip kinds -------------------------------------------------------------
    pub const CLIP_VIDEO: u32 = 0x2f5a9e;
    pub const CLIP_AUDIO: u32 = 0x2c6b49;
    pub const CLIP_IMAGE: u32 = 0x1c6270;
    pub const CLIP_TEXT: u32 = 0x5a45b8;

    pub const SOURCE_TINTS: [u32; 12] = [
        0xe88fd6, 0x8fc9e8, 0xe8c48f, 0x8fe8b4, 0xe88fd6, 0x8fc9e8, 0xe8c48f, 0x8fe8b4, 0xe88fd6,
        0x8fc9e8, 0xe8c48f, 0x8fe8b4,
    ];

    // -- feedback ---------------------------------------------------------------
    pub const STATUS_ERROR: u32 = 0xf87171;
    pub const STATUS_WARNING: u32 = 0xfb923c;
    pub const STATUS_SUCCESS: u32 = 0x4ade80;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x8f2740;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x33203f;
    pub const EQ_SPECTRUM_INK: u32 = 0xc7b0cf66;
    pub const EQ_FILL_INK: u32 = 0xd946ef26;
    pub const EQ_BELL_INK: u32 = 0xd946ef66;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family J: Nord, the arctic palette (arcticicestudio/nord, MIT) -- Polar
/// Night for the surfaces, Snow Storm for the ink, Frost for the accent, Aurora
/// for everything that has to mean something. Its own numbers wherever they
/// clear the floors: `nord0`..`nord3` are the panels verbatim, and the canvas
/// and the lane bed are taken one step below `nord0`, which the set does not
/// name, because a bed lighter than its panel stops reading as a hole.
///
/// The clip bodies and the dim ink are the adapted ones. Aurora is a *text*
/// palette: `nord14` green under white is 2.6:1, and this editor draws a name
/// and a waveform on every clip body, so the four kinds are those hues taken
/// down to where WCAG 1.4.3 holds.
pub mod nord {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x21262e;
    pub const BG_PANEL: u32 = 0x2e3440;
    pub const BG_RAISED: u32 = 0x3b4252;
    pub const BG_TIMELINE: u32 = 0x272c36;
    pub const BG_HOVER: u32 = 0x434c5e;
    pub const BG_HOVER_DIM: u32 = 0x353c4a;
    /// `nord10` is Nord's own picked row and carries the dim ink at 2.9:1, so
    /// the row is that blue taken down until 4.5:1 holds (WCAG 1.4.3).
    pub const BG_SELECTED: u32 = 0x2e4a6e;
    pub const SCRIM: u32 = 0x21262ecc;
    pub const SCRIM_LIGHT: u32 = 0x21262e55;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x434c5e;
    pub const STROKE_FOCUS: u32 = 0xd08770;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xeceff4;
    pub const FG_SECONDARY: u32 = 0xd8dee9;
    /// Lighter than `nord3`, which is a *surface* in Nord and only 2.6:1 on the
    /// raised one: dimmed text still has to be read (WCAG 1.4.11).
    pub const FG_DISABLED: u32 = 0x8a94a8;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0x88c0d0;
    pub const ACCENT_HOVER: u32 = 0x8fbcbb;
    /// `nord13`. Orange (`nord12`) is the obvious playhead and is 2.3:1 on a
    /// video clip -- the line has to be findable on every body it crosses.
    pub const ACCENT_PLAYHEAD: u32 = 0xebcb8b;
    pub const ACCENT_WASH: u32 = 0x88c0d0aa;

    // -- clip kinds (Aurora, taken down to where a name is legible on it) -------
    pub const CLIP_VIDEO: u32 = 0x3b5f8a;
    pub const CLIP_AUDIO: u32 = 0x4d6b3f;
    pub const CLIP_IMAGE: u32 = 0x3f6a69;
    pub const CLIP_TEXT: u32 = 0x6b4a78;

    /// Aurora and Frost at full strength: a swatch carries no text, so these are
    /// Nord's own numbers.
    pub const SOURCE_TINTS: [u32; 12] = [
        0x88c0d0, 0xd08770, 0xa3be8c, 0xb48ead, 0x88c0d0, 0xd08770, 0xa3be8c, 0xb48ead, 0x88c0d0,
        0xd08770, 0xa3be8c, 0xb48ead,
    ];

    // -- feedback ---------------------------------------------------------------
    pub const STATUS_ERROR: u32 = 0xbf616a;
    pub const STATUS_WARNING: u32 = 0xd08770;
    pub const STATUS_SUCCESS: u32 = 0xa3be8c;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x7a3038;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x434c5e;
    pub const EQ_SPECTRUM_INK: u32 = 0xd8dee966;
    pub const EQ_FILL_INK: u32 = 0x88c0d026;
    pub const EQ_BELL_INK: u32 = 0x88c0d066;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family K: Gruvbox dark (morhetz/gruvbox, MIT) -- the retro brown-grey ground
/// and the warm ink that made it, under its own orange. `bg0_h`, `bg0`, `bg1`
/// and `bg2` are the surfaces verbatim and `fg1`..`fg4` the ink; the clip bodies
/// are the neutral-mode hues taken down for the same reason Nord's are.
///
/// Two of its colours do two jobs here rather than one: orange is the accent
/// and green the playhead -- so the family is recognisable from the chrome and
/// not only from the ground.
pub mod gruvbox {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x1d2021;
    pub const BG_PANEL: u32 = 0x282828;
    pub const BG_RAISED: u32 = 0x3c3836;
    pub const BG_TIMELINE: u32 = 0x232323;
    pub const BG_HOVER: u32 = 0x504945;
    pub const BG_HOVER_DIM: u32 = 0x32302f;
    /// The orange taken to where `fg2` clears 4.5:1 on it -- `bg2`, gruvbox's
    /// own selection, is a grey and says nothing about which row is picked.
    pub const BG_SELECTED: u32 = 0x7c3f12;
    pub const SCRIM: u32 = 0x1d2021cc;
    pub const SCRIM_LIGHT: u32 = 0x1d202155;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x504945;
    pub const STROKE_FOCUS: u32 = 0x83a598;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xebdbb2;
    pub const FG_SECONDARY: u32 = 0xd5c4a1;
    pub const FG_DISABLED: u32 = 0xa89984;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0xfe8019;
    pub const ACCENT_HOVER: u32 = 0xfabd2f;
    /// Bright green: it is the one gruvbox hue that clears 3:1 on all four clip
    /// bodies at once, which is what a line crossing every one of them owes.
    pub const ACCENT_PLAYHEAD: u32 = 0xb8bb26;
    pub const ACCENT_WASH: u32 = 0xfe8019aa;

    // -- clip kinds -------------------------------------------------------------
    pub const CLIP_VIDEO: u32 = 0x35657e;
    pub const CLIP_AUDIO: u32 = 0x4b661c;
    pub const CLIP_IMAGE: u32 = 0x2f6a63;
    pub const CLIP_TEXT: u32 = 0x7a4a72;

    pub const SOURCE_TINTS: [u32; 12] = [
        0xfe8019, 0x83a598, 0xb8bb26, 0xd3869b, 0xfe8019, 0x83a598, 0xb8bb26, 0xd3869b, 0xfe8019,
        0x83a598, 0xb8bb26, 0xd3869b,
    ];

    // -- feedback ---------------------------------------------------------------
    pub const STATUS_ERROR: u32 = 0xfb4934;
    pub const STATUS_WARNING: u32 = 0xfabd2f;
    pub const STATUS_SUCCESS: u32 = 0x8ec07c;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x9d0006;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x504945;
    pub const EQ_SPECTRUM_INK: u32 = 0xd5c4a166;
    pub const EQ_FILL_INK: u32 = 0xfe801926;
    pub const EQ_BELL_INK: u32 = 0xfe801966;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family L: Dracula (dracula/dracula-theme, MIT) -- its background, current
/// line and foreground verbatim, and its pink for the accent rather than the
/// purple, which `violet` already sits on. Comment (`#6272a4`) is Dracula's dim
/// ink and measures 2.4:1 on the raised surface, so the dim ink here is lifted
/// and the comment blue is what a *disabled* control wears.
pub mod dracula {
    // -- surfaces ---------------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x1e1f29;
    pub const BG_PANEL: u32 = 0x282a36;
    pub const BG_RAISED: u32 = 0x343746;
    pub const BG_TIMELINE: u32 = 0x22232e;
    pub const BG_HOVER: u32 = 0x44475a;
    pub const BG_HOVER_DIM: u32 = 0x2f3141;
    pub const BG_SELECTED: u32 = 0x6b2a70;
    pub const SCRIM: u32 = 0x1e1f29cc;
    pub const SCRIM_LIGHT: u32 = 0x1e1f2955;

    // -- strokes ----------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x44475a;
    pub const STROKE_FOCUS: u32 = 0x8be9fd;
    pub const STROKE_SELECTED: u32 = ACCENT_PRIMARY;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = 0xf8f8f2;
    pub const FG_SECONDARY: u32 = 0xc3c5d8;
    pub const FG_DISABLED: u32 = 0x7b8bc4;

    // -- interaction ------------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = 0xff79c6;
    pub const ACCENT_HOVER: u32 = 0xff92d0;
    pub const ACCENT_PLAYHEAD: u32 = 0xf1fa8c;
    pub const ACCENT_WASH: u32 = 0xff79c6aa;

    // -- clip kinds -------------------------------------------------------------
    pub const CLIP_VIDEO: u32 = 0x3f5aa6;
    pub const CLIP_AUDIO: u32 = 0x2f7a48;
    pub const CLIP_IMAGE: u32 = 0x2b6e78;
    pub const CLIP_TEXT: u32 = 0x6a3fb0;

    /// Dracula's own six, four of them: a swatch carries no text.
    pub const SOURCE_TINTS: [u32; 12] = [
        0xff79c6, 0x8be9fd, 0x50fa7b, 0xffb86c, 0xff79c6, 0x8be9fd, 0x50fa7b, 0xffb86c, 0xff79c6,
        0x8be9fd, 0x50fa7b, 0xffb86c,
    ];

    // -- feedback ---------------------------------------------------------------
    pub const STATUS_ERROR: u32 = 0xff5555;
    pub const STATUS_WARNING: u32 = 0xffb86c;
    pub const STATUS_SUCCESS: u32 = 0x50fa7b;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = 0x8f2a3a;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = 0xffffff;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram ----------------------------------
    pub const EQ_GRID: u32 = 0x44475a;
    pub const EQ_SPECTRUM_INK: u32 = 0xc3c5d866;
    pub const EQ_FILL_INK: u32 = 0xff79c626;
    pub const EQ_BELL_INK: u32 = 0xff79c666;
    pub const HIST_INK: [u32; 3] = [0xE0_5A_5A, 0x5A_D0_7A, 0x5A_9A_E0];
    // -- Darkroom token substrate: aliased onto this family's nearest role -----
    pub const INK1: u32 = FG_PRIMARY;
    pub const INK2: u32 = FG_SECONDARY;
    pub const INK3: u32 = FG_DISABLED;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_SEAM: u32 = SCRIM;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = STATUS_SUCCESS;
    pub const NOTICE_LOOK: u32 = STATUS_WARNING;
    pub const NOTICE_DECIDE: u32 = STATUS_ERROR;
    pub const SAFELIGHT_GROUND: u32 = BG_CANVAS;
    pub const SAFELIGHT_GLYPH: u32 = ACCENT_PRIMARY;
}

/// Family M: Darkroom (DESIGN.md §2) -- the thirteenth family and the room the
/// redesign is named after. Chrome is achromatic: every pre-existing role below
/// is a grey, distinguished by luminance rather than hue ("loudness is
/// luminance"); the only colour anywhere in this family sits on the roles
/// DESIGN.md actually gives one -- the amber safelight glyph and the two loud
/// notice tones. Everything else is the token substrate itself, taken verbatim
/// from the spec table.
pub mod darkroom {
    // -- surfaces -----------------------------------------------------------
    pub const BG_CANVAS: u32 = 0x050607;
    pub const BG_PANEL: u32 = 0x0e1013;
    // DESIGN §2's `raised` band (#14171B - #17191D) is the WHOLE interaction
    // ladder in this room: a resting raised fill, one step up for hover, one
    // more for a picked row -- selection's real mark is the 1px `ink1` ring
    // (§4), never a pale flood.
    //
    // These four used to carry the legacy tree's greys (hover #2A2E33,
    // selected #2E3237 -- two and three steps outside the band). Every
    // surface that paints a *role* rather than a Darkroom token -- the clip
    // menu, the library row menu, the keys overlay, the seven param cards --
    // read them, so each one opened a pale plate in a dim room while the
    // handful of surfaces written against `DARK_*`/`INK*` directly looked
    // right. That is the whole "some menus belong to old ui" class, and it
    // is fixed here, once, rather than by another per-call-site `if
    // darkroom` branch: role tokens ARE the theme's job (§2's guard).
    pub const BG_RAISED: u32 = 0x14171b;
    pub const BG_TIMELINE: u32 = 0x08090b;
    pub const BG_HOVER: u32 = 0x17191d;
    pub const BG_HOVER_DIM: u32 = 0x121418;
    pub const BG_SELECTED: u32 = 0x1c1f24;
    pub const SCRIM: u32 = 0x050607cc;
    pub const SCRIM_LIGHT: u32 = 0x05060755;

    // -- strokes --------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x22262b;
    pub const STROKE_FOCUS: u32 = INK1;
    /// "Focus/selection ring = 1px ink1 (lamp-adjacent, not coloured)."
    pub const STROKE_SELECTED: u32 = INK1;

    // -- text -------------------------------------------------------------------
    pub const FG_PRIMARY: u32 = INK1;
    pub const FG_SECONDARY: u32 = INK2;
    pub const FG_DISABLED: u32 = INK4;

    // -- interaction --------------------------------------------------------
    pub const ACCENT_PRIMARY: u32 = INK2;
    pub const ACCENT_HOVER: u32 = INK1;
    pub const ACCENT_PLAYHEAD: u32 = LAMP_WHITE;
    pub const ACCENT_WASH: u32 = 0x9ba3ac55;

    // -- clip kinds (luminance steps, no hue -- held under the grey that still
    // carries FG_SECONDARY at 3:1, WCAG 1.4.11) -------------------------------
    pub const CLIP_VIDEO: u32 = 0x343434;
    pub const CLIP_AUDIO: u32 = 0x3d3d3d;
    pub const CLIP_IMAGE: u32 = 0x464646;
    pub const CLIP_TEXT: u32 = 0x4f4f4f;

    // DESIGN §2 reference extraction, S/L clamped into the WCAG band and
    // walked around the wheel at 12 stops (30° apart) so the cap in the same
    // paragraph has one real value per hue rather than four greys repeated:
    // azure #64B5D1, magenta #D164B5, green #64D19A, violet #7F64D1, teal
    // #64D1D1 sit at indices 0, 4, 9(~), 2(~), 5(~) of this wheel.
    // hook: §12 step 5 -- real per-source extraction replaces this constant
    // wheel with a quantized dominant hue per import; until then the index
    // is `source % 12`, which still guarantees different sources differ.
    pub const SOURCE_TINTS: [u32; 12] = [
        0x64b5d1, 0x647ed1, 0x8064d1, 0xb664d1, 0xd164b5, 0xd1647e, 0xd18064, 0xd1b764, 0xb5d164,
        0x7ed164, 0x64d180, 0x64d1b7,
    ];

    // -- feedback -------------------------------------------------------------
    pub const STATUS_ERROR: u32 = NOTICE_DECIDE;
    pub const STATUS_WARNING: u32 = NOTICE_LOOK;
    pub const STATUS_SUCCESS: u32 = INK1;
    pub const STATUS_PROGRESS: u32 = ACCENT_PRIMARY;
    pub const DROP_REFUSE: u32 = NOTICE_DECIDE;

    // -- subtitles --------------------------------------------------------------
    pub const SUB_FG: u32 = LAMP_WHITE;
    pub const SUB_SHADE: u32 = 0x000000cc;

    // -- the equalizer graph and the histogram -------------------------------
    pub const EQ_GRID: u32 = 0x22262b;
    pub const EQ_SPECTRUM_INK: u32 = 0x9ba3ac66;
    pub const EQ_FILL_INK: u32 = 0x9ba3ac26;
    pub const EQ_BELL_INK: u32 = 0x9ba3ac66;
    pub const HIST_INK: [u32; 3] = [0x767e87, 0x9ba3ac, 0xc7cdd3];

    // -- Darkroom token substrate (DESIGN.md §2, verbatim) -----------------------
    pub const INK1: u32 = 0xe6e9ec;
    pub const INK2: u32 = 0x9ba3ac;
    pub const INK3: u32 = 0x5e656d;
    pub const INK4: u32 = 0x3c4249;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    /// Seam: `rgba(0,0,0,.7)`, encoded the way `SCRIM`/`SCRIM_LIGHT` are --
    /// `0xRRGGBBAA` -- so `0xb3` (179/255 ~= 0.702) is the closest byte to 70%.
    pub const DARK_SEAM: u32 = 0x000000b3;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = 0x5e656d;
    pub const NOTICE_LOOK: u32 = 0xd1b564;
    pub const NOTICE_DECIDE: u32 = 0xc85050;
    pub const SAFELIGHT_GROUND: u32 = 0x0a0908;
    pub const SAFELIGHT_GLYPH: u32 = 0xff9d57;
}

/// The box a button's glyph sits in, whatever the glyph happens to measure: the
/// pause bars are 12 px wide and the play triangle 11, and a button that resized
/// itself when it was pressed moved every button to its right.
pub const GLYPH_SLOT: f32 = 14.;

/// How solid the shadow a drag draws is (`0xRRGGBBAA`): enough to read as a box,
/// little enough that the clip under it is still legible. Not a colour, so both
/// families share it.
pub const GHOST_ALPHA: u32 = 0x66;

/// Which of the feedback colours a message wears. Read off the words rather
/// than carried alongside them: every message in this editor already opens with
/// what it is ("EXPORT DONE", "SCAN FAILED", "NOTHING DETACHED"), and a second
/// `tone` argument at seventy call sites is seventy chances to disagree with the
/// sentence it labels.
///
/// corner-cut: prefix matching, so a message worded outside these families reads
/// as neutral rather than wrong. Ceiling: a `Notice { text, tone }` struct the
/// day a message needs a colour its own words do not say.
pub fn notice_tone(message: &str) -> u32 {
    let has = |word: &str| message.contains(word);
    if has("FAILED") || has("ERROR") || has("REFUSED") || has("CANNOT") || has("COULD NOT") {
        STATUS_ERROR()
    } else if message.starts_with(crate::EXPORT_DONE) || has("SAVED") || has("DONE") {
        STATUS_SUCCESS()
    } else if has("NOTHING") || has("NO ") || has("EMPTY") {
        STATUS_WARNING()
    } else {
        ACCENT_PRIMARY()
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
