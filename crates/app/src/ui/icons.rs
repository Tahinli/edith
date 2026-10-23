//! REDESIGN §1.4 -- the icon set. Lucide (ISC; licence in
//! `assets/icons/LICENSE-lucide.txt`), vendored as SVG under `assets/icons/`
//! by `scripts/prep_icons.py` (stroke baked white, 24 grid) and compiled into
//! the binary: [`Assets`] is the `gpui::AssetSource` handed to
//! `Application::with_assets`, so `svg().path("icons/play.svg")` resolves
//! with nothing installed beside the executable.
//!
//! gpui rasterises the SVG through resvg to an alpha mask and tints it with the
//! element's `text_color`, which is why the files are white and why every icon
//! takes its colour from the theme at the call site:
//!
//! ```ignore
//! Icon::Play.el(IconSize::Toolbar, ink2())            // rest
//! Icon::Play.el(IconSize::Toolbar, tokens().accent)   // a toggle that is on
//! ```
//!
//! Colour policy (§1.4): `ink2` at rest, `ink1` on hover, `accent` when a
//! toggle is on, `ink4` disabled; [`Icon::tint`] applies it from an [`IconState`].
//! The lane glyphs (a record dot, a note, a square) stay typographic and are not here.

use crate::ui::theme::tokens;
use gpui::{AssetSource, Pixels, Result, SharedString, Svg, px, rgb, svg, Styled};
use std::borrow::Cow;

/// The three sizes §1.4 allows (stroke is baked for the 16 and 20 px renders).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconSize {
    /// Lane heads, menus, inspector, dock rows, buttons that also carry a label.
    Inline,
    /// Transport and icon-only toolbar buttons.
    Toolbar,
    /// The empty-state hero only.
    Hero,
}

impl IconSize {
    pub const fn px(self) -> f32 {
        match self {
            IconSize::Inline => 16.,
            IconSize::Toolbar => 20.,
            IconSize::Hero => 40.,
        }
    }
}

/// Where an icon is in its life, which decides its tint (§1.4 colour rule).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconState {
    Rest,
    Hover,
    /// A toggle or tool that is on.
    On,
    Disabled,
}

macro_rules! icons {
    ($($variant:ident = $file:literal,)+) => {
        /// One variant per vendored SVG. A variant without its file fails to build.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Icon { $($variant,)+  }

        impl Icon {
            /// Every icon, for the gallery and the "every file is used" test.
            pub const ALL: &'static [Icon] = &[$(Icon::$variant,)+ ];


            /// The icon's file stem, e.g. `"skip-back"`.
            pub const fn name(self) -> &'static str {
                match self { $(Icon::$variant => $file,)+ }
            }

            const fn bytes(self) -> &'static [u8] {
                match self {
                    $(Icon::$variant => include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"), "/../../assets/icons/", $file, ".svg"
                    )),)+
                }
            }
        }

        /// (asset path, bytes) for every icon.
        const TABLE: &[(&str, &[u8])] = &[
            $((concat!("icons/", $file, ".svg"), Icon::$variant.bytes()),)+
        ];
    };
}

icons! {
    ArrowLeftToLine = "arrow-left-to-line",
    // The same file turned half a revolution in `Icon::el_px`: no second SVG.
    ArrowRightToLine = "arrow-left-to-line",
    AudioWaveform = "audio-waveform",
    Brackets = "brackets",
    Captions = "captions",
    Check = "check",
    ChevronDown = "chevron-down",
    ChevronLeft = "chevron-left",
    ChevronRight = "chevron-right",
    ChevronUp = "chevron-up",
    CircleCheck = "circle-check",
    CircleHelp = "circle-help",
    CircleX = "circle-x",
    Clapperboard = "clapperboard",
    Clipboard = "clipboard",
    Copy = "copy",
    Download = "download",
    Ellipsis = "ellipsis",
    Eraser = "eraser",
    Eye = "eye",
    EyeOff = "eye-off",
    Film = "film",
    Flag = "flag",
    FlagTriangleRight = "flag-triangle-right",
    Folder = "folder",
    FolderOpen = "folder-open",
    Gauge = "gauge",
    Activity = "activity",
    Diamond = "diamond",
    FastForward = "fast-forward",
    KeyRound = "key-round",
    Signal = "signal",
    Spline = "spline",
    Timer = "timer",
    TrendingDown = "trending-down",
    TrendingUp = "trending-up",
    Zap = "zap",
    Grid3x3 = "grid-3x3",
    GripVertical = "grip-vertical",
    Image = "image",
    Info = "info",
    Keyboard = "keyboard",
    Layers = "layers",
    Link = "link",
    LoaderCircle = "loader-circle",
    Lock = "lock",
    LockOpen = "lock-open",
    Magnet = "magnet",
    Mic = "mic",
    Minus = "minus",
    Move = "move",
    Palette = "palette",
    Pause = "pause",
    Play = "play",
    Plus = "plus",
    Redo2 = "redo-2",
    RefreshCw = "refresh-cw",
    Repeat = "repeat",
    RotateCcw = "rotate-ccw",
    Save = "save",
    Scaling = "scaling",
    Scan = "scan",
    Scissors = "scissors",
    Search = "search",
    Settings = "settings",
    SkipBack = "skip-back",
    SkipForward = "skip-forward",
    SlidersHorizontal = "sliders-horizontal",
    Sparkles = "sparkles",
    SquareArrowDownLeft = "square-arrow-down-left",
    SquareArrowDownRight = "square-arrow-down-right",
    Star = "star",
    StarFilled = "star-filled",
    StepBack = "step-back",
    StepForward = "step-forward",
    Trash2 = "trash-2",
    TriangleAlert = "triangle-alert",
    Type = "type",
    Undo2 = "undo-2",
    Unlink = "unlink",
    Upload = "upload",
    Volume2 = "volume-2",
    VolumeX = "volume-x",
    WandSparkles = "wand-sparkles",
    Wrench = "wrench",
    X = "x",
    ZoomIn = "zoom-in",
    ZoomOut = "zoom-out",
    ArrowRight = "arrow-right",
    // The same file turned a quarter revolution in `Icon::el_px`: no second SVG.
    ArrowUp = "arrow-right",
    ArrowDown = "arrow-right",
    Circle = "circle",
    Square = "square",
}

impl Icon {
    /// The path `svg().path(..)` resolves through [`Assets`].
    pub fn path(self) -> SharedString {
        format!("icons/{}.svg", self.name()).into()
    }

    /// The icon as an element, `size` square, tinted `color` (a theme `0xRRGGBB`).
    /// Further `Styled` calls (margins, `flex_none`) chain on the result.
    pub fn el(self, size: IconSize, color: u32) -> Svg {
        self.el_px(px(size.px()), color)
    }

    /// [`Icon::el`] at an exact size, for the rare in-between slot.
    pub fn el_px(self, size: Pixels, color: u32) -> Svg {
        let el = svg().path(self.path()).size(size).flex_none().text_color(rgb(color));
        let turn = match self {
            Icon::ArrowRightToLine => std::f32::consts::PI,
            Icon::ArrowUp => -std::f32::consts::FRAC_PI_2,
            Icon::ArrowDown => std::f32::consts::FRAC_PI_2,
            _ => return el,
        };
        el.with_transformation(gpui::Transformation::rotate(gpui::radians(turn)))
    }

    /// The tint the colour rule gives an icon in `state`.
    pub fn tint(state: IconState) -> u32 {
        let t = tokens();
        match state {
            IconState::Rest => t.ink2,
            IconState::Hover => t.ink1,
            IconState::On => t.accent,
            IconState::Disabled => t.ink4,
        }
    }

    /// [`Icon::el`] tinted for `state`.
    pub fn stated(self, size: IconSize, state: IconState) -> Svg {
        self.el(size, Self::tint(state))
    }
}

/// The app's `AssetSource`: the embedded icon table and nothing else.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(TABLE.iter().find(|(p, _)| *p == path).map(|(_, b)| Cow::Borrowed(*b)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(TABLE
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every vendored file is a white-stroked 24 grid SVG gpui can tint, and
    /// every file in `assets/icons/` has a variant (no orphans).
    #[test]
    fn every_icon_is_baked_and_every_file_has_a_variant() {
        for icon in Icon::ALL {
            let s = std::str::from_utf8(icon.bytes()).unwrap();
            assert!(s.contains("viewBox=\"0 0 24 24\""), "{}", icon.name());
            assert!(!s.contains("currentColor"), "{} is not baked", icon.name());
            assert!(Assets.load(&icon.path()).unwrap().is_some());
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons");
        let files = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
            .filter(|n| n != "LICENSE-lucide")
            .count();
        let named: std::collections::HashSet<_> = Icon::ALL.iter().map(|i| i.name()).collect();
        assert_eq!(files, named.len(), "an svg without a variant");
    }

    #[test]
    fn sizes_are_the_three_the_spec_allows() {
        assert_eq!(
            [IconSize::Inline, IconSize::Toolbar, IconSize::Hero].map(IconSize::px),
            [16., 20., 40.]
        );
    }
}
