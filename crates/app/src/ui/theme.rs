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
//! * `darkroom` (default) -- the mock's near-black ground and neutral inks, one
//!   sage accent, one lamp-white playhead.
//! * `cool` -- near-black ground, cool neutral chrome, one cyan accent.
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
//! All thirteen are dark: colour work is judged against its surround, and a light
//! ground pushes every clip body and every graded frame the wrong way -- which
//! is why no editor of this kind ships one. The twelve hued families take the clip
//! bodies from the cross-NLE kind convention (video blue, audio green, image teal,
//! text purple) instead of four greys that differ by a hair, and all thirteen are held
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
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, RwLock};

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
    DARK_EDGE: u32,
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
    /// What a fresh install and an unreadable theme file get: the mock's own
    /// room (DESIGN.md §2.1), not the first family in the list.
    pub const DEFAULT: PaletteId = PaletteId::Darkroom;

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
            PaletteId::Cool => "Black with cyan",
            PaletteId::Warm => "Warm grey with coral",
            PaletteId::Forest => "Dark green with emerald",
            PaletteId::Violet => "Indigo with lavender",
            PaletteId::Rose => "Grey with rose",
            PaletteId::Amber => "Graphite with gold",
            PaletteId::Ocean => "Blue-black with azure",
            PaletteId::Ice => "Navy black with pale blue",
            PaletteId::Orchid => "Plum with orchid",
            PaletteId::Nord => "Blue-grey with frost",
            PaletteId::Gruvbox => "Brown with orange",
            PaletteId::Dracula => "Purple-grey with pink",
            PaletteId::Darkroom => "Dark with sage green",
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
static ACTIVE: AtomicUsize = AtomicUsize::new(PaletteId::DEFAULT as usize);

/// The numbers every token function reads. One load, one bounds-checked index
/// into a `'static` table: no allocation, no hashing, no lock.
#[inline]
pub fn palette() -> &'static Palette {
    PALETTES
        .get(ACTIVE.load(Relaxed))
        .unwrap_or(&PALETTES[PaletteId::DEFAULT as usize])
}

/// Which family is in force -- what the button says and the picker marks.
pub fn active() -> PaletteId {
    PaletteId::ALL
        .get(ACTIVE.load(Relaxed))
        .copied()
        .unwrap_or(PaletteId::DEFAULT)
}

/// Puts `id` in force. Every paint after this reads the new numbers; asking for
/// the repaint is the caller's, since only it holds the window.
pub fn set(id: PaletteId) {
    ACTIVE.store(id as usize, Relaxed);
}

/// The accent colour a person picked, on top of whichever family is in force.
/// The six come from the sibling project's accent palette (its dark tokens):
/// the accent, a lighter one for hover, and the dark ink that sits on a fill.
/// `Custom` is a colour of the person's own ([`custom_colour`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccentId {
    Sage,
    Ocean,
    Plum,
    Rose,
    Ochre,
    Gold,
    Custom,
}

/// The custom colour before anyone has typed one: a soft orchid.
pub const CUSTOM_DEFAULT: u32 = 0xd49ae0;
/// The Vivid at which an accent is exactly the colour as designed.
pub const VIVID_DESIGNED: u8 = 50;
/// How vivid the accents are as shipped: bold (since 2026-10-21; before, the designed 50).
pub const VIVID_DEFAULT: u8 = 100;

impl AccentId {
    pub const DEFAULT: AccentId = AccentId::Sage;
    pub const ALL: [AccentId; 7] = [
        AccentId::Sage,
        AccentId::Ocean,
        AccentId::Plum,
        AccentId::Rose,
        AccentId::Ochre,
        AccentId::Gold,
        AccentId::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AccentId::Sage => "Sage",
            AccentId::Ocean => "Ocean",
            AccentId::Plum => "Plum",
            AccentId::Rose => "Rose",
            AccentId::Ochre => "Ochre",
            AccentId::Gold => "Gold",
            AccentId::Custom => "Custom",
        }
    }

    /// The word kept in the accent file.
    pub fn name(self) -> &'static str {
        match self {
            AccentId::Sage => "sage",
            AccentId::Ocean => "ocean",
            AccentId::Plum => "plum",
            AccentId::Rose => "rose",
            AccentId::Ochre => "ochre",
            AccentId::Gold => "gold",
            AccentId::Custom => "custom",
        }
    }

    /// The accent file's word for this accent; a file saved before the grey
    /// Slate was replaced still reads "slate" and lands on Gold.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "slate" => Some(AccentId::Gold),
            _ => Self::ALL.into_iter().find(|a| a.name() == name),
        }
    }

    /// `(accent, hover, ink on a fill)` as designed, before Vivid. A custom
    /// colour has its hover and ink worked out from it.
    pub fn base(self, custom: u32) -> (u32, u32, u32) {
        match self {
            AccentId::Sage => (0x9dbea2, 0xb5d2b9, 0x0f1510),
            AccentId::Ocean => (0x86b9dd, 0x9fcbea, 0x061118),
            AccentId::Plum => (0xbba9e0, 0xcbbceb, 0x110d18),
            AccentId::Rose => (0xe79dad, 0xf3b2c0, 0x190a0e),
            AccentId::Ochre => (0xd8a98a, 0xe4bca1, 0x170d07),
            AccentId::Gold => (0xe3d372, 0xf7ea9b, 0x191606),
            AccentId::Custom => {
                let (l, c, h) = to_oklch(custom);
                (custom, from_oklch((l + 0.06).min(1.), c * 0.9, h), from_oklch(0.17, c.min(0.03), h))
            }
        }
    }

    /// The swatch's colour: the accent as designed.
    pub fn colours(self) -> (u32, u32, u32) {
        self.base(custom_colour().unwrap_or(CUSTOM_DEFAULT))
    }
}

/// The colour a swatch wears: the accent as the Vivid in force makes it, so it matches the button.
pub fn swatch(a: AccentId) -> u32 {
    vivid_adjust(a.colours().0, vivid())
}

/// What a person chose: the accent, how vivid, and the colour behind Custom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AccentSpec {
    pub id: AccentId,
    pub vivid: u8,
    pub custom: u32,
}

impl AccentSpec {
    #[cfg(test)]
    pub const DEFAULT: AccentSpec = AccentSpec { id: AccentId::DEFAULT, vivid: VIVID_DEFAULT, custom: CUSTOM_DEFAULT };

    /// `t` with its accent roles replaced by this accent. The primary fill is
    /// the accent itself; press is the accent a little darker; the label on it
    /// is whichever of the accent's dark ink and white reads better. The colour
    /// itself is never moved to make anything readable: it is the person's.
    fn apply(self, mut t: Tokens) -> Tokens {
        let (base, base_hover, ink) = self.id.base(self.custom);
        let accent = vivid_adjust(base, self.vivid);
        let hover = match accent == base {
            true => base_hover,
            false => vivid_adjust(base_hover, self.vivid),
        };
        let press = mix(accent, 0x000000, 0.15);
        let score = |ink: u32| contrast(ink, accent).min(contrast(ink, press));
        t.accent = accent;
        t.accent_hover = hover;
        t.accent_press = press;
        t.accent_fill = accent;
        t.on_accent = match score(ink) >= score(0xffffff) {
            true => ink,
            false => 0xffffff,
        };
        t.accent_wash = with_alpha(accent, 0x33);
        t.range_tint = with_alpha(accent, 0x1a);
        t
    }
}

// -- OKLab / OKLCH, for the Vivid setting and the hue slider -------------------

fn srgb_to_lin(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn lin_to_srgb(x: f64) -> f64 {
    if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1. / 2.4) - 0.055 }
}

/// `0xRRGGBB` to OKLCH: lightness 0..1, chroma, hue in degrees.
pub fn to_oklch(rgb: u32) -> (f64, f64, f64) {
    let ch = |s: u32| srgb_to_lin(f64::from((rgb >> s) & 0xff) / 255.);
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    let big_l = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
    let a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
    let bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
    (big_l, a.hypot(bb), bb.atan2(a).to_degrees().rem_euclid(360.))
}

/// The linear sRGB of an OKLCH colour, possibly outside 0..1.
fn oklch_lin(l: f64, c: f64, h: f64) -> (f64, f64, f64) {
    let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    (
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    )
}

/// OKLCH to `0xRRGGBB`. A colour the screen cannot show keeps its lightness and
/// hue and gives up chroma until it fits.
pub fn from_oklch(l: f64, c: f64, h: f64) -> u32 {
    let l = l.clamp(0., 1.);
    let fits = |c: f64| {
        let (r, g, b) = oklch_lin(l, c, h);
        [r, g, b].iter().all(|v| (-0.0005..=1.0005).contains(v))
    };
    let mut c = c.max(0.);
    if !fits(c) {
        let (mut lo, mut hi) = (0., c);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.;
            match fits(mid) {
                true => lo = mid,
                false => hi = mid,
            }
        }
        c = lo;
    }
    let (r, g, b) = oklch_lin(l, c, h);
    let byte = |v: f64| (lin_to_srgb(v.clamp(0., 1.)) * 255.).round() as u32;
    byte(r) << 16 | byte(g) << 8 | byte(b)
}

/// `rgb` with its chroma moved by Vivid (0..100). 50 is the colour untouched;
/// 0 leaves a faint tint of it; 100 is bold, as far as the screen allows.
/// Hue and lightness stay.
pub fn vivid_adjust(rgb: u32, vivid: u8) -> u32 {
    if vivid == VIVID_DESIGNED {
        return rgb;
    }
    let (l, c, h) = to_oklch(rgb);
    let v = f64::from(vivid.min(100));
    let c = match v < 50. {
        true => c * (0.08 + 0.92 * v / 50.),
        false => c + (v - 50.) / 50. * ((c * 3.).max(0.17) - c),
    };
    from_oklch(l, c, h)
}

/// `#RRGGBB` (the `#` optional) to `0xRRGGBB`; anything else is `None`.
pub fn parse_hex(text: &str) -> Option<u32> {
    let t = text.trim().trim_start_matches('#');
    match t.len() == 6 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        true => u32::from_str_radix(t, 16).ok(),
        false => None,
    }
}

/// The accent chosen, as a spec: three words in force at once.
static ACCENT: AtomicUsize = AtomicUsize::new(AccentId::DEFAULT as usize);
static VIVID: AtomicU8 = AtomicU8::new(VIVID_DEFAULT);
/// The custom colour; above `0xffffff` means nobody has made one.
static CUSTOM: AtomicU32 = AtomicU32::new(u32::MAX);

pub fn accent() -> AccentId {
    AccentId::ALL.get(ACCENT.load(Relaxed)).copied().unwrap_or(AccentId::DEFAULT)
}

pub fn vivid() -> u8 {
    VIVID.load(Relaxed)
}

/// The custom colour, once there is one.
pub fn custom_colour() -> Option<u32> {
    Some(CUSTOM.load(Relaxed)).filter(|c| *c <= 0xffffff)
}

fn spec_in_force() -> AccentSpec {
    AccentSpec { id: accent(), vivid: vivid(), custom: custom_colour().unwrap_or(CUSTOM_DEFAULT) }
}

/// Puts `a` in force; the caller asks for the repaint. Custom with no colour
/// yet takes the default one, so the swatch has something to show.
pub fn set_accent(a: AccentId) {
    if a == AccentId::Custom && custom_colour().is_none() {
        CUSTOM.store(CUSTOM_DEFAULT, Relaxed);
    }
    ACCENT.store(a as usize, Relaxed);
    refresh();
}

pub fn set_vivid(v: u8) {
    VIVID.store(v.min(100), Relaxed);
    refresh();
}

/// Sets the custom colour and makes Custom the accent in force.
pub fn set_custom(rgb: u32) {
    CUSTOM.store(rgb & 0xffffff, Relaxed);
    ACCENT.store(AccentId::Custom as usize, Relaxed);
    refresh();
}

/// Beside the theme file: `~/.config/edith/accent`.
pub fn accent_path() -> PathBuf {
    config_path().with_file_name("accent")
}

/// Reads the accent file: `accent=sage`, `vivid=100`, `custom=#3ddc97`, one per
/// line. The first version kept just the word (`sage`), which still reads.
/// Anything unknown or unreadable leaves that part at its default.
pub fn parse_accent(text: &str) -> (AccentId, u8, Option<u32>) {
    let (mut id, mut vivid, mut custom) = (AccentId::DEFAULT, VIVID_DEFAULT, None);
    for line in text.lines().map(str::trim) {
        match line.split_once('=') {
            Some(("accent", v)) => id = AccentId::from_name(v.trim()).unwrap_or(id),
            Some(("vivid", v)) => vivid = v.trim().parse::<u32>().map_or(vivid, |n| n.min(100) as u8),
            Some(("custom", v)) => custom = parse_hex(v).or(custom),
            None => id = AccentId::from_name(line).unwrap_or(id),
            _ => {}
        }
    }
    (id, vivid, custom)
}

/// The accent file's text for what is in force.
pub fn accent_text() -> String {
    let mut text = format!("accent={}\nvivid={}\n", accent().name(), vivid());
    if let Some(c) = custom_colour() {
        text.push_str(&format!("custom=#{c:06x}\n"));
    }
    text
}

/// The accent from the last session; missing or unreadable leaves Sage.
pub fn load_accent() {
    if let Ok(text) = std::fs::read_to_string(accent_path()) {
        let (id, vivid, custom) = parse_accent(&text);
        if let Some(c) = custom {
            CUSTOM.store(c, Relaxed);
        }
        VIVID.store(vivid, Relaxed);
        set_accent(id);
    }
}

/// Writes the accent in force, like [`save`].
pub fn save_accent() -> std::io::Result<()> {
    let path = accent_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, accent_text())
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
    pub const INK3: u32 = 0x98a2af;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0xaca5a0;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0x95a79e;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0xa4a3be;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    /// message's own first word says which it is either way ([`crate::notices::Tone::of`]).
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
    pub const INK3: u32 = 0xa9a0a3;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0xa5a49d;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0x92a3b4;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0x97a8b6;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0xa99ab0;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0xbfc5d0;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0xbaaf9e;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const INK3: u32 = 0xa6b0d7;
    pub const INK4: u32 = FG_DISABLED;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    pub const DARK_EDGE: u32 = INK3;
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
    pub const BG_CANVAS: u32 = 0x0b0c0e;
    pub const BG_PANEL: u32 = 0x131518;
    // DESIGN §2.1's `raised` (#1B1E22) and `hover` (#24282D) are the interaction
    // ladder in this room: a resting raised fill, one step up for hover.
    // Selection's real mark is the 1px `ink1` ring (§4), never a pale flood.
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
    pub const BG_RAISED: u32 = 0x1b1e22;
    pub const BG_TIMELINE: u32 = 0x08090b;
    pub const BG_HOVER: u32 = 0x24282d;
    pub const BG_HOVER_DIM: u32 = 0x121418;
    pub const BG_SELECTED: u32 = 0x1c1f24;
    pub const SCRIM: u32 = 0x050607cc;
    pub const SCRIM_LIGHT: u32 = 0x05060755;

    // -- strokes --------------------------------------------------------------
    pub const STROKE_DIVIDER: u32 = 0x2a2e34;
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
    pub const ACCENT_WASH: u32 = 0xb4bac155;

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
    pub const EQ_SPECTRUM_INK: u32 = 0xb4bac166;
    pub const EQ_FILL_INK: u32 = 0xb4bac126;
    pub const EQ_BELL_INK: u32 = 0xb4bac166;
    pub const HIST_INK: [u32; 3] = [0x767e87, 0x9ba3ac, 0xc7cdd3];

    // -- Darkroom token substrate (DESIGN.md §2, verbatim) -----------------------
    pub const INK1: u32 = 0xe8eaed;
    pub const INK2: u32 = 0xb4bac1;
    pub const INK3: u32 = 0x8d959e;
    pub const INK4: u32 = 0x777f88;
    pub const DARK_CANVAS: u32 = BG_CANVAS;
    pub const DARK_PANEL: u32 = BG_PANEL;
    pub const DARK_RAISED: u32 = BG_RAISED;
    pub const DARK_HAIRLINE: u32 = STROKE_DIVIDER;
    /// The boundary of a control that has to be recognised as one (WCAG 1.4.11,
    /// 3:1 on every surface); `DARK_HAIRLINE` stays for dividers, which need not be.
    pub const DARK_EDGE: u32 = 0x6e757d;
    /// Seam: `rgba(0,0,0,.7)`, encoded the way `SCRIM`/`SCRIM_LIGHT` are --
    /// `0xRRGGBBAA` -- so `0xb3` (179/255 ~= 0.702) is the closest byte to 70%.
    pub const DARK_SEAM: u32 = 0x000000b3;
    pub const LAMP_WHITE: u32 = 0xffffff;
    pub const NOTICE_TELL: u32 = 0x5e656d;
    pub const NOTICE_LOOK: u32 = 0xd1b564;
    pub const NOTICE_DECIDE: u32 = 0xe07a7a;
    pub const SAFELIGHT_GROUND: u32 = 0x0a0908;
    pub const SAFELIGHT_GLYPH: u32 = 0xff9d57;
}

// -- REDESIGN §1.3 tokens -----------------------------------------------------
//
// The elevation ladder, the ink and accent roles and the clip-type colours the
// redesigned regions paint by. They sit beside the legacy `palette!` roles
// rather than inside them: `darkroom` carries the spec's hard values, and the
// twelve other families get the same roles *derived* from the roles they
// already have ([`derive`]), so a thirteenth family still costs one module.
//
// Colour encoding is the theme's: `0xRRGGBB`, or `0xRRGGBBAA` for the few
// alpha roles (`line`, `line_hi`, `accent_wash`) which only ever go to `rgba()`.

/// What kind of media a clip is. Colour is never the only cue: every kind has
/// a 16px icon too (`ui::icons`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClipKind {
    Video,
    Image,
    Audio,
    /// Title and caption-generator clips.
    Title,
    /// Subtitle cues.
    Caption,
    /// A nested sequence.
    Compound,
    Multicam,
}

impl ClipKind {
    pub const ALL: [ClipKind; 7] = [
        ClipKind::Video,
        ClipKind::Image,
        ClipKind::Audio,
        ClipKind::Title,
        ClipKind::Caption,
        ClipKind::Compound,
        ClipKind::Multicam,
    ];
}

/// One clip kind's colours (§1.3): `strip` is the name strip and the lane
/// head dot, `body` shows behind transparent areas, `ink` tints waveforms and glyphs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ClipTone {
    pub strip: u32,
    pub body: u32,
    pub ink: u32,
    /// The name text on the strip: `ink1`, white or `e0`, whichever reads best on
    /// `strip` (the amber and grey strips need dark text).
    pub label: u32,
}

/// The §1.3 role table of one family. Read it with [`tokens`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tokens {
    /// Window, seams between regions, menu bar.
    pub e0: u32,
    /// Panels: dock body, lane heads, transport, toolbar, status bar.
    pub e1: u32,
    /// Raised: cards, fields, buttons at rest.
    pub e2: u32,
    /// Hover fill, popup plate, selected segment.
    pub e3: u32,
    /// Pressed fill.
    pub e4: u32,
    /// The timeline's lane area, recessed.
    pub well: u32,
    /// The viewer surround.
    pub matte: u32,
    /// Hairline inside a surface (`0xRRGGBBAA`).
    pub line: u32,
    /// The popup plate's border (`0xRRGGBBAA`).
    pub line_hi: u32,
    /// Boxes that must pass 3:1 and the focus-ring fallback.
    pub edge: u32,
    pub ink1: u32,
    pub ink2: u32,
    pub ink3: u32,
    /// Disabled only, never information.
    pub ink4: u32,
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_press: u32,
    /// Primary button fill (the accent itself); `on_accent` sits on it.
    pub accent_fill: u32,
    /// The label and glyph on a filled accent: a near-black tint of its hue.
    pub on_accent: u32,
    /// Selected row / drop target (`0xRRGGBBAA`).
    pub accent_wash: u32,
    /// The In-Out range's tint across the lanes (`0xRRGGBBAA`): subtle, never hides a clip.
    pub range_tint: u32,
    pub err: u32,
    pub warn: u32,
    pub ok: u32,
    /// Clip colours by kind, in [`ClipKind::ALL`] order.
    pub clip: [ClipTone; 7],
}

impl Tokens {
    pub fn clip(&self, kind: ClipKind) -> ClipTone {
        self.clip[kind as usize]
    }
}

/// WCAG 2.x contrast ratio of two `0xRRGGBB` colours.
pub fn contrast(a: u32, b: u32) -> f64 {
    let lum = |hex: u32| {
        let lin = |shift: u32| {
            let c = f64::from((hex >> shift) & 0xff) / 255.;
            if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(16) + 0.7152 * lin(8) + 0.0722 * lin(0)
    };
    let (a, b) = (lum(a), lum(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Linear mix of two `0xRRGGBB` colours, `t` of the way from `a` to `b`.
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = (((a >> s) & 0xff) as f32, ((b >> s) & 0xff) as f32);
        ((x + (y - x) * t).round() as u32).min(255) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// `0xRRGGBB` with an alpha byte: the `0xRRGGBBAA` the alpha roles use.
pub const fn with_alpha(rgb: u32, alpha: u8) -> u32 {
    ((rgb & 0xffffff) << 8) | alpha as u32
}

const fn tone(strip: u32, body: u32, ink: u32) -> ClipTone {
    ClipTone { strip, body, ink, label: 0 }
}

/// Darkroom's hard values, straight from REDESIGN §1.3.
const DARKROOM_TOKENS: Tokens = Tokens {
    e0: 0x0f0f11,
    e1: 0x17171a,
    e2: 0x202024,
    e3: 0x2a2a2f,
    e4: 0x35353b,
    well: 0x0c0c0e,
    matte: 0x08080a,
    line: 0xffffff12,
    line_hi: 0xffffff14,
    edge: 0x6e757d,
    ink1: 0xececef,
    ink2: 0xa9a9b2,
    ink3: 0x85858f,
    ink4: 0x5a5a63,
    accent: 0x9dbea2,
    accent_hover: 0xb5d2b9,
    accent_press: 0x86a18a,
    accent_fill: 0x9dbea2,
    on_accent: 0x0f1510,
    accent_wash: 0x9dbea233,
    range_tint: 0x9dbea21a,
    err: 0xf0707a,
    warn: 0xe2b860,
    ok: 0x5acb8e,
    clip: [
        tone(0x2f5f9e, 0x1c3350, 0xffffff),
        tone(0x2b8080, 0x183b3c, 0xffffff),
        tone(0x2e8b5e, 0x17291f, 0x6fd6a0),
        tone(0x7b5cc4, 0x2b2248, 0xc3b0f5),
        tone(0xb98a2e, 0x33280f, 0xf0cc82),
        tone(0x8b8f99, 0x25272c, 0xc9ccd4),
        tone(0xc25b8a, 0x3a1b2b, 0xf1a8c6),
    ],
};

/// The twelve legacy families' §1.3 roles, from the roles they already carry:
/// the four backgrounds are the ladder's first four steps, pressed is hover
/// lifted towards `ink1`, the accent roles come from their accent, and the
/// clip kinds from their `CLIP_*` bodies (kinds the family has no role for
/// borrow the notice / ink / source-tint hues).
fn derive(p: &Palette) -> Tokens {
    let kind = |strip: u32, ink: u32| tone(strip, mix(strip, p.BG_CANVAS, 0.55), ink);
    let lift = |c: u32| mix(c, 0xffffff, 0.55);
    Tokens {
        e0: p.BG_CANVAS,
        e1: p.BG_PANEL,
        e2: p.BG_RAISED,
        e3: p.BG_HOVER,
        e4: mix(p.BG_HOVER, p.INK1, 0.08),
        well: p.BG_TIMELINE,
        matte: mix(p.BG_CANVAS, 0x000000, 0.35),
        line: 0xffffff12,
        line_hi: 0xffffff14,
        edge: p.DARK_EDGE,
        ink1: p.INK1,
        ink2: p.INK2,
        ink3: p.INK3,
        ink4: p.INK4,
        accent: p.ACCENT_PRIMARY,
        accent_hover: p.ACCENT_HOVER,
        accent_press: mix(p.ACCENT_PRIMARY, 0x000000, 0.2),
        accent_fill: p.ACCENT_PRIMARY,
        on_accent: 0x0f1510,
        accent_wash: with_alpha(p.ACCENT_PRIMARY, 0x33),
        range_tint: with_alpha(p.ACCENT_PRIMARY, 0x1a),
        err: p.STATUS_ERROR,
        warn: p.STATUS_WARNING,
        ok: p.STATUS_SUCCESS,
        clip: [
            kind(p.CLIP_VIDEO, 0xffffff),
            kind(p.CLIP_IMAGE, 0xffffff),
            kind(p.CLIP_AUDIO, lift(p.CLIP_AUDIO)),
            kind(p.CLIP_TEXT, lift(p.CLIP_TEXT)),
            kind(p.NOTICE_LOOK, lift(p.NOTICE_LOOK)),
            kind(p.INK3, lift(p.INK3)),
            kind(p.SOURCE_TINTS[4], lift(p.SOURCE_TINTS[4])),
        ],
    }
}

/// Fills each clip tone's `label` with the better of `ink1` and `e0` on its strip.
fn with_labels(mut t: Tokens) -> Tokens {
    for tone in &mut t.clip {
        tone.label = [t.ink1, 0xffffff, t.e0]
            .into_iter()
            .max_by(|a, b| contrast(*a, tone.strip).total_cmp(&contrast(*b, tone.strip)))
            .unwrap_or(t.ink1);
    }
    t
}

/// The §1.3 roles of the family in force: one relaxed load and one index, like
/// the legacy tokens. Darkroom is hard-coded; the others are [`derive`]d once.
pub fn tokens() -> &'static Tokens {
    // The slice is `'static` (leaked, one per accent setting ever shown), so the
    // lock is held for the pointer copy only.
    let load = || *LIVE.read().unwrap_or_else(|e| e.into_inner());
    let mut all = load();
    if all.is_empty() {
        refresh();
        all = load();
    }
    all.get(ACTIVE.load(Relaxed)).unwrap_or(&all[PaletteId::DEFAULT as usize])
}

/// The roles of every family under the accent in force.
static LIVE: RwLock<&'static [Tokens]> = RwLock::new(&[]);

/// Every family's roles before any accent is laid on them, once.
fn bases() -> &'static [Tokens] {
    static BASES: std::sync::OnceLock<Vec<Tokens>> = std::sync::OnceLock::new();
    BASES.get_or_init(|| PaletteId::ALL.iter().map(|&id| base_tokens(id)).collect())
}

fn base_tokens(id: PaletteId) -> Tokens {
    match id {
        PaletteId::Darkroom => with_labels(DARKROOM_TOKENS),
        _ => with_labels(derive(&PALETTES[id as usize])),
    }
}

/// Swaps in the tables of the accent now in force. A table is leaked once per
/// distinct setting; a slider drag visits a few dozen of them.
fn refresh() {
    static SEEN: Mutex<Vec<(AccentSpec, &'static [Tokens])>> = Mutex::new(Vec::new());
    let spec = spec_in_force();
    let Ok(mut seen) = SEEN.lock() else { return };
    let table = match seen.iter().find(|(s, _)| *s == spec) {
        Some((_, t)) => *t,
        None => {
            let t: &'static [Tokens] = Box::leak(bases().iter().map(|b| spec.apply(*b)).collect());
            seen.push((spec, t));
            t
        }
    };
    *LIVE.write().unwrap_or_else(|e| e.into_inner()) = table;
}

/// The roles of family `id` under `accent`.
#[cfg(test)]
fn tokens_with(id: PaletteId, accent: AccentSpec) -> Tokens {
    accent.apply(base_tokens(id))
}

/// [`tokens`] of a family that is not the one in force (the guards).
#[cfg(test)]
pub fn tokens_of(id: PaletteId) -> Tokens {
    tokens_with(id, AccentSpec::DEFAULT)
}

/// White washes (`0xRRGGBBAA`, `rgba()` only) laid over a transparent rest
/// fill for hover and press (§1.5): 6%, 10%, and the 4% a list row uses.
pub const WASH_HOVER: u32 = 0xffffff0f;
pub const WASH_PRESS: u32 = 0xffffff1a;
#[allow(dead_code, reason = "media rows adopt it in P4")]
pub const WASH_ROW: u32 = 0xffffff0a;

/// The popup plate's shadow colour (`0xRRGGBBAA`): `0 8 24 rgba(0,0,0,.5)`.
pub const PLATE_SHADOW: u32 = 0x00000080;

/// The box a button's glyph sits in, whatever the glyph happens to measure: the
/// pause bars are 12 px wide and the play triangle 11, and a button that resized
/// itself when it was pressed moved every button to its right.
pub const GLYPH_SLOT: f32 = 14.;

/// How solid the shadow a drag draws is (`0xRRGGBBAA`): enough to read as a box,
/// little enough that the clip under it is still legible. Not a colour, so both
/// families share it.
pub const GHOST_ALPHA: u32 = 0x66;

// -- geometry the four regions are laid out on --------------------------------
/// Compact density: 11-13 px text, 28 px controls, 8 px gutters. The floor the
/// whole window is measured at is 640x360, and airy spacing loses a region.
pub const INSPECTOR_MIN_W: f32 = 208.;
pub const INSPECTOR_MAX_W: f32 = 320.;
pub const INSPECTOR_FRAC: f32 = 0.24;

/// The colour a comment marker is drawn in. The palette is the marker's own, fixed
/// (red, orange, yellow, green, blue, purple), and is not one of the window's themes:
/// a marker's colour is data saved with the project, so it reads the same under every
/// theme.
pub fn marker_rgb(color: engine::project::MarkerColor) -> u32 {
    use engine::project::MarkerColor;
    match color {
        MarkerColor::Red => 0xe5484d,
        MarkerColor::Orange => 0xf76b15,
        MarkerColor::Yellow => 0xf5d90a,
        MarkerColor::Green => 0x46a758,
        MarkerColor::Blue => 0x3e8fe0,
        MarkerColor::Purple => 0x8e4ec6,
    }
}

/// The inspector's width at this window width, on the library's own rule: a
/// share of the window, clamped so the picture keeps the middle at every size.
pub fn inspector_w(window_w: f32) -> f32 {
    (window_w * INSPECTOR_FRAC).clamp(INSPECTOR_MIN_W, INSPECTOR_MAX_W)
}

#[cfg(test)]
mod tests {
    use super::darkroom as d;

    /// WCAG 2.x relative luminance of an `0xRRGGBB`.
    fn luminance(hex: u32) -> f64 {
        let lin = |shift: u32| {
            let c = f64::from((hex >> shift) & 0xff) / 255.;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(16) + 0.7152 * lin(8) + 0.0722 * lin(0)
    }

    fn ratio(a: u32, b: u32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    const SURFACES: [(&str, u32); 4] = [
        ("canvas", d::BG_CANVAS),
        ("panel", d::BG_PANEL),
        ("raised", d::BG_RAISED),
        ("hover", d::BG_HOVER),
    ];

    /// DESIGN §2.1 / §4.1: every (ink, floor) pair on every surface it can sit on.
    #[test]
    fn darkroom_token_pairs_meet_their_contrast_spec() {
        let specs: [(&str, u32, f64); 6] = [
            ("ink1", d::INK1, 4.5),
            ("ink2", d::INK2, 4.5),
            ("ink3", d::INK3, 4.5),
            ("err", d::NOTICE_DECIDE, 4.5),
            // Disabled: spec'd 3.6 (WCAG exempts it; below ink3 keeps it distinct).
            ("ink4 (disabled)", d::INK4, 3.6),
            // WCAG 1.4.11 control boundary.
            ("edge", d::DARK_EDGE, 3.0),
        ];
        for (name, fg, floor) in specs {
            for (surface, bg) in SURFACES {
                let r = ratio(fg, bg);
                assert!(r >= floor, "{name} on {surface}: {r:.2} < {floor}");
            }
        }
        // The focus ring and the playhead are ink1/lamp: they must clear 3:1 too.
        for (surface, bg) in SURFACES {
            assert!(ratio(d::STROKE_SELECTED, bg) >= 3.0, "focus ring on {surface}");
            assert!(ratio(d::LAMP_WHITE, bg) >= 3.0, "playhead on {surface}");
        }
        // The dividers are decorative: the hairline stays below the control edge.
        assert!(ratio(d::DARK_EDGE, d::BG_PANEL) > ratio(d::DARK_HAIRLINE, d::BG_PANEL));
    }

    /// REDESIGN §1.3: the new token pairs. Ink on the surfaces it sits on,
    /// the accent as a focus ring, white on the primary fill, the status hues.
    #[test]
    fn darkroom_tokens_meet_the_redesign_spec() {
        let t = super::tokens_of(super::PaletteId::Darkroom);
        for (name, bg) in [("e0", t.e0), ("e1", t.e1), ("e2", t.e2), ("e3", t.e3), ("e4", t.e4)] {
            assert!(ratio(t.ink1, bg) >= 7.0, "ink1 on {name}");
        }
        for (name, bg) in [("e0", t.e0), ("e1", t.e1), ("e2", t.e2), ("e3", t.e3)] {
            assert!(ratio(t.ink2, bg) >= 4.5, "ink2 on {name}");
        }
        // ink3 carries keys and units on panels; it is not asked to sit on a popup plate at 4.5.
        for (name, bg) in [("e0", t.e0), ("e1", t.e1)] {
            assert!(ratio(t.ink3, bg) >= 4.5, "ink3 on {name}");
        }
        for (name, bg) in [("e0", t.e0), ("e1", t.e1), ("e2", t.e2)] {
            assert!(ratio(t.accent, bg) >= 3.0, "focus ring on {name}");
            for (c, hue) in [("err", t.err), ("warn", t.warn), ("ok", t.ok)] {
                assert!(ratio(hue, bg) >= 4.5, "{c} on {name}");
            }
        }
        assert!(ratio(t.on_accent, t.accent_fill) >= 4.5, "label on the primary fill");
        // Disabled stays distinct from information.
        assert!(ratio(t.ink4, t.e1) < ratio(t.ink3, t.e1));
        // The ladder rises and the well sits below every panel.
        let l = |c| luminance(c);
        assert!(l(t.matte) < l(t.well) && l(t.well) < l(t.e0) && l(t.e0) < l(t.e1));
        assert!(l(t.e1) < l(t.e2) && l(t.e2) < l(t.e3) && l(t.e3) < l(t.e4));
        // Clip strips carry white-ish labels at 4.5; waveform ink reads on its body.
        for kind in super::ClipKind::ALL {
            let c = t.clip(kind);
            // §1.3's audio green tops out at ~4.3:1 for a 12px/500 name; 4.0 is the floor here.
            assert!(ratio(c.label, c.strip) >= 4.0, "{kind:?} name on strip");
            assert!(ratio(c.ink, c.body) >= 4.5, "{kind:?} ink on body");
        }
    }

    use super::{AccentId, AccentSpec};

    fn spec(id: AccentId, vivid: u8, custom: u32) -> AccentSpec {
        AccentSpec { id, vivid, custom }
    }

    /// The six presets as designed (Vivid 50), on every family: ring on the surfaces, label on the
    /// fill and on its press.
    #[test]
    fn every_preset_meets_contrast_on_every_family() {
        for id in super::PaletteId::ALL {
            for a in super::AccentId::ALL.into_iter().filter(|a| *a != AccentId::Custom) {
                let t = super::tokens_with(id, spec(a, 50, super::CUSTOM_DEFAULT));
                for bg in [t.e0, t.e1, t.e2] {
                    assert!(ratio(t.accent, bg) >= 3.0, "{a:?} ring on {id:?}");
                }
                assert!(ratio(t.on_accent, t.accent_fill) >= 4.5, "{a:?} label on fill");
                assert!(ratio(t.on_accent, t.accent_press) >= 4.5, "{a:?} label on press");
                assert!(ratio(t.accent_hover, t.e1) >= 3.0, "{a:?} hover");
            }
        }
    }

    #[test]
    fn oklch_round_trips_every_colour() {
        for r in (0..=255).step_by(15) {
            for g in (0..=255).step_by(15) {
                for b in (0..=255).step_by(15) {
                    let rgb = (r << 16) | (g << 8) | b;
                    let (l, c, h) = super::to_oklch(rgb);
                    assert_eq!(super::from_oklch(l, c, h), rgb, "{rgb:06x}");
                }
            }
        }
    }

    #[test]
    fn vivid_50_is_the_designed_colour_and_the_ends_move_chroma_only() {
        for a in AccentId::ALL {
            let (base, hover, _) = a.base(0x3ddc97);
            let t = super::tokens_with(super::PaletteId::Darkroom, spec(a, 50, 0x3ddc97));
            assert_eq!((t.accent, t.accent_hover), (base, hover), "{a:?}");
            let chroma = |v| super::to_oklch(super::vivid_adjust(base, v)).1;
            assert!(chroma(0) < chroma(50) && chroma(50) < chroma(100), "{a:?}");
            let (l0, _, h0) = super::to_oklch(base);
            for v in [0, 100] {
                let (l, c, h) = super::to_oklch(super::vivid_adjust(base, v));
                assert!((l - l0).abs() < 0.02, "{a:?} lightness at {v}");
                if c > 0.02 {
                    assert!((h - h0).abs() < 4., "{a:?} hue at {v}");
                }
            }
        }
    }

    /// The colour is the person's: never moved for readability. Only the label picks a side.
    #[test]
    fn a_custom_colour_is_kept_and_its_label_is_the_better_of_ink_and_white() {
        for hex in [0x000000, 0xffffff, 0x0000ff, 0xff4d8d, 0x3ddc97, 0x123456] {
            let t = super::tokens_with(super::PaletteId::Darkroom, spec(AccentId::Custom, 50, hex));
            assert_eq!(t.accent, hex);
            assert_eq!(t.accent_fill, hex);
            let score = |ink| ratio(ink, hex).min(ratio(ink, t.accent_press));
            assert!(score(t.on_accent) >= score(0xffffff), "{hex:06x}");
        }
    }

    #[test]
    fn accent_files_parse_old_and_new() {
        use super::parse_accent as p;
        assert_eq!(p("rose\n"), (AccentId::Rose, 100, None));
        assert_eq!(p("accent=plum\nvivid=80\ncustom=#3ddc97\n"), (AccentId::Plum, 80, Some(0x3ddc97)));
        assert_eq!(p("accent=custom\nvivid=500\n"), (AccentId::Custom, 100, None));
        assert_eq!(p("garbage\nvivid=x\ncustom=#12\n=\n\0"), (AccentId::Sage, 100, None));
        assert_eq!(p(""), (AccentId::Sage, 100, None));
        assert_eq!(super::parse_hex("3DDC97"), Some(0x3ddc97));
        assert_eq!(super::parse_hex("#3ddc9"), None);
        assert_eq!(super::parse_hex("#3ddc9g"), None);
    }

    #[test]
    fn accent_names_round_trip_and_switching_changes_tokens() {
        for a in super::AccentId::ALL {
            assert_eq!(super::AccentId::from_name(a.name()), Some(a));
        }
        assert_eq!(super::AccentId::from_name("nope"), None);
        assert_eq!(super::AccentId::DEFAULT, super::AccentId::Sage);
        let sage = super::tokens_with(super::PaletteId::Darkroom, spec(AccentId::Sage, 50, 0));
        let rose = super::tokens_with(super::PaletteId::Darkroom, spec(AccentId::Rose, 50, 0));
        assert_ne!(sage.accent, rose.accent);
    }

    /// Every family's derived ladder still rises, and its ink stays legible on every step.
    #[test]
    fn every_palette_derives_a_rising_ladder_and_legible_inks() {
        use super::{PaletteId, tokens_of};
        for id in PaletteId::ALL {
            let t = tokens_of(id);
            let l = luminance;
            assert!(l(t.e0) < l(t.e1) && l(t.e1) < l(t.e2) && l(t.e2) < l(t.e3), "{id:?}");
            assert!(l(t.e3) < l(t.e4), "{id:?} pressed above hover");
            for bg in [t.e0, t.e1, t.e2, t.e3, t.e4] {
                assert!(ratio(t.ink1, bg) >= 4.5, "{id:?} ink1 on {bg:06x}");
            }
            for bg in [t.e0, t.e1, t.e2, t.e3] {
                assert!(ratio(t.ink2, bg) >= 4.5, "{id:?} ink2 on {bg:06x}");
            }
        }
    }

    #[test]
    fn mix_and_alpha_helpers() {
        assert_eq!(super::mix(0x000000, 0xffffff, 0.5), 0x808080);
        assert_eq!(super::mix(0x123456, 0xffffff, 0.0), 0x123456);
        assert_eq!(super::with_alpha(0x3d8bff, 0x33), 0x3d8bff33);
    }
}
