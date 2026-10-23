//! Text in the picture: a pure-Rust glyph rasteriser for burned-in captions
//! and title cards. Front-end free like the rest of the engine, so the export,
//! [`crate::export::compose_at_with`] and the playback path all draw one look.
//!
//! The look is the subtitle plate the preview already draws, cut down to what
//! a picture can carry: white Archivo text, optionally outlined in black
//! ([`Outline`]), optionally on a translucent black box, centred on the frame
//! and placed at the bottom, the top or the middle ([`Place`]). Sizes are a
//! share of the *frame height* ([`TextStyle::size`]), so a caption reads the
//! same on a 640x360 proxy and a 4K master.
//!
//! A character Archivo has no glyph for is looked up in order: the bundled Noto
//! Sans (Latin, Greek, Cyrillic and more), then the bundled Arabic, Hebrew,
//! Devanagari and Thai faces, then the system colour emoji font (painted in
//! colour), then the bundled monochrome Noto Emoji, then the system Noto Sans CJK
//! (read only when a character needs it). Only a character none of them has is
//! drawn as the empty box ([`missing_glyphs`] names them). Runs of a non-Latin
//! face are shaped with `rustybuzz`, and right-to-left runs are reordered by the
//! bidi algorithm; Archivo's runs keep the plain advance-and-kern path, so Latin
//! text draws exactly as it always did.
//!
//! Several cues on screen at once are laid out together by [`overlay_stack`],
//! stacked so none covers another.
//!
//! The coverage is computed once per text ([`Overlay`]) and composed per frame.
//! [`Overlay::blend_bgra`] composes over a BGRA canvas; [`Overlay::blend_i420`]
//! composes over the planes an encoder reads, which is what the export uses so
//! a burned-in frame never leaves the planar format it was encoded from.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont, point};
use unicode_bidi::BidiInfo;

use crate::colorspace::is_sd;

/// The text face this crate draws Latin with, bundled so no system font is ever
/// read. The path is relative to this file: `src/` up three to the repository root.
const ARCHIVO: usize = 0;
/// Noto Sans: Latin, Greek and Cyrillic, plus the symbols Archivo lacks.
const NOTO_SANS: usize = 1;
/// The monochrome emoji face, the last outline face before the system fonts.
const MONO_EMOJI: usize = 6;
/// Archivo Bold, drawn in place of [`ARCHIVO`] when a style asks for [`Weight::Bold`].
const ARCHIVO_BOLD: usize = 7;
/// How many bundled outline faces there are.
const OUTLINE_FACES: usize = 8;

/// The bundled outline faces, in lookup order (see the module docs). All are
/// SIL Open Font License fonts under `assets/fonts`, the variable ones at their
/// default instance. The path is relative to this file.
static OUTLINE_BYTES: [&[u8]; OUTLINE_FACES] = [
    include_bytes!("../../../assets/fonts/Archivo-Medium.ttf"),
    include_bytes!("../../../assets/fonts/NotoSans[wght].ttf"),
    include_bytes!("../../../assets/fonts/NotoSansArabic[wght].ttf"),
    include_bytes!("../../../assets/fonts/NotoSansHebrew[wght].ttf"),
    include_bytes!("../../../assets/fonts/NotoSansDevanagari[wght].ttf"),
    include_bytes!("../../../assets/fonts/NotoSansThai[wght].ttf"),
    // Noto Emoji, monochrome outlines (`assets/fonts/OFL-notoemoji.txt`).
    include_bytes!("../../../assets/fonts/NotoEmoji-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Archivo-Bold.ttf"),
];

/// Each bundled face, parsed the first time a character reaches it.
static PARSED: [OnceLock<Option<FontRef<'static>>>; OUTLINE_FACES] = [const { OnceLock::new() }; OUTLINE_FACES];

/// The outline face at index `i` of [`OUTLINE_BYTES`], parsed once.
fn outline_face(i: usize) -> Option<&'static FontRef<'static>> {
    PARSED[i]
        .get_or_init(|| FontRef::try_from_slice(OUTLINE_BYTES[i]).ok())
        .as_ref()
}

static SHAPERS: [OnceLock<Option<rustybuzz::Face<'static>>>; OUTLINE_FACES] = [const { OnceLock::new() }; OUTLINE_FACES];

/// The shaping face of outline face `i`, parsed once: `rustybuzz` reads the same
/// bytes ab_glyph draws from, so the glyph ids it returns are the ones drawn.
fn shaper(i: usize) -> Option<&'static rustybuzz::Face<'static>> {
    SHAPERS[i]
        .get_or_init(|| rustybuzz::Face::from_slice(OUTLINE_BYTES[i], 0))
        .as_ref()
}

/// The system colour emoji font, read from the first of these that exists.
const COLOUR_PATHS: [&str; 2] = [
    "/usr/share/fonts/google-noto-color-emoji-fonts/Noto-COLRv1.ttf",
    "/usr/share/fonts/noto-emoji/NotoColorEmoji.ttf",
];

/// The system Noto Sans CJK collection, the first path that exists. Index 0 of
/// the collection is read: the Japanese faces, whose ideographs are the common
/// forms. Read only when a character needs it.
const CJK_PATHS: [&str; 4] = [
    "/usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
];

/// The bytes of the first readable path in `paths`, read once.
fn system_bytes(cell: &'static OnceLock<Option<Vec<u8>>>, paths: &[&str]) -> Option<&'static [u8]> {
    cell.get_or_init(|| paths.iter().find_map(|p| std::fs::read(p).ok()))
        .as_deref()
}

/// The system colour emoji face, parsed once. None when the system has no such font.
fn colour_face() -> Option<&'static ttf_parser::Face<'static>> {
    static BYTES: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    static FACE: OnceLock<Option<ttf_parser::Face<'static>>> = OnceLock::new();
    FACE.get_or_init(|| {
        let bytes = system_bytes(&BYTES, &COLOUR_PATHS)?;
        ttf_parser::Face::parse(bytes, 0).ok()
    })
    .as_ref()
}

/// The system CJK face, parsed once and only on first use. It is a variable font
/// whose default weight is Thin (wght 100), which reads grey beside the title's
/// Medium, so it is set to Medium (wght 500). ab_glyph cannot set an axis, so
/// this face is read through ttf-parser and drawn by [`draw_outline`].
fn cjk_face() -> Option<&'static ttf_parser::Face<'static>> {
    static BYTES: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    static FACE: OnceLock<Option<ttf_parser::Face<'static>>> = OnceLock::new();
    FACE.get_or_init(|| {
        let bytes = system_bytes(&BYTES, &CJK_PATHS)?;
        let mut face = ttf_parser::Face::parse(bytes, 0).ok()?;
        face.set_variation(ttf_parser::Tag::from_bytes(b"wght"), 500.0);
        Some(face)
    })
    .as_ref()
}

/// The size each outline face is set at, as a multiple of the em. Archivo is 1, so
/// Latin is unchanged. Every other face's x-height (the OS/2 `sxHeight`) is set to
/// Archivo's, so a Cyrillic, Arabic or Thai letter is as tall as a Latin one
/// beside it. The mono emoji face is left at 1. Its ab_glyph scale is per face
/// height, so the factor is the ratio of heights times the ratio of x-heights.
fn face_k() -> &'static [f32; OUTLINE_FACES] {
    static K: OnceLock<[f32; OUTLINE_FACES]> = OnceLock::new();
    K.get_or_init(|| {
        let mut k = [1.0; OUTLINE_FACES];
        let Some(text) = ttf_parser::Face::parse(OUTLINE_BYTES[ARCHIVO], 0)
            .ok()
            .and_then(|f| f.x_height())
        else {
            return k;
        };
        let text_h = outline_face(ARCHIVO).map_or(1.0, |f| f.height_unscaled());
        for (i, slot) in k.iter_mut().enumerate().take(MONO_EMOJI).skip(ARCHIVO + 1) {
            let (Some(font), Ok(face)) = (
                outline_face(i),
                ttf_parser::Face::parse(OUTLINE_BYTES[i], 0),
            ) else {
                continue;
            };
            if let Some(x) = face.x_height().filter(|&x| x > 0) {
                *slot = (font.height_unscaled() / text_h) * (f32::from(text) / f32::from(x));
            }
        }
        k
    })
}

/// A system family a style names: its face, and the scale that sets its height and x-height to
/// Archivo's, as [`face_k`] does for the bundled faces.
#[derive(Clone, Copy)]
pub(crate) struct Family {
    font: &'static FontRef<'static>,
    k: f32,
}

/// The scale that sets a system face's height and x-height to Archivo's.
fn family_k(font: &FontRef<'_>, data: &[u8], index: u32) -> f32 {
    let text_h = outline_face(ARCHIVO).map_or(1.0, |f| f.height_unscaled());
    let text_x = ttf_parser::Face::parse(OUTLINE_BYTES[ARCHIVO], 0).ok().and_then(|f| f.x_height());
    let own_x = ttf_parser::Face::parse(data, index).ok().and_then(|f| f.x_height()).filter(|&x| x > 0);
    match (text_x, own_x) {
        (Some(t), Some(x)) => (font.height_unscaled() / text_h) * (f32::from(t) / f32::from(x)),
        _ => font.height_unscaled() / text_h,
    }
}

/// The system face `name` names, or None when the system has no such family or its face does not
/// parse. Each name is looked up once; the database is read on first use. A face's bytes live for
/// the rest of the process, as the bundled faces do.
pub(crate) fn family_face(name: &str) -> Option<Family> {
    static DB: OnceLock<fontdb::Database> = OnceLock::new();
    static FOUND: Mutex<BTreeMap<String, Option<Family>>> = Mutex::new(BTreeMap::new());
    let mut found = FOUND.lock().ok()?;
    if let Some(known) = found.get(name) {
        return *known;
    }
    let db = DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db
    });
    let query = fontdb::Query { families: &[fontdb::Family::Name(name)], ..fontdb::Query::default() };
    let family = db.query(&query).and_then(|id| {
        db.with_face_data(id, |data, index| {
            let data: &'static [u8] = Box::leak(data.to_vec().into_boxed_slice());
            let font = FontRef::try_from_slice_and_index(data, index).ok()?;
            let k = family_k(&font, data, index);
            Some(Family { font: Box::leak(Box::new(font)), k })
        })
        .flatten()
    });
    found.insert(name.to_string(), family);
    family
}

/// Where a character's glyph comes from, in lookup order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Src {
    /// One of the bundled outline faces, by its index in [`OUTLINE_BYTES`].
    Outline(usize),
    /// The system colour emoji face, painted in its colours.
    Colour,
    /// The system CJK face.
    Cjk,
    /// The family a style names, when the system has it (see [`Faces::family`]).
    Family,
}

/// The first source with a glyph for `c` and that glyph's id there, or None when
/// no face has one (the character is drawn as the empty box).
fn lookup(c: char) -> Option<(Src, u16)> {
    let outline = |i: usize| {
        let g = outline_face(i)?.glyph_id(c).0;
        (g != 0).then_some((Src::Outline(i), g))
    };
    let colour = || {
        let face = colour_face()?;
        let g = face.glyph_index(c)?;
        face.is_color_glyph(g).then_some((Src::Colour, g.0))
    };
    let cjk = || {
        let g = cjk_face()?.glyph_index(c)?.0;
        (g != 0).then_some((Src::Cjk, g))
    };
    (0..MONO_EMOJI)
        .find_map(outline)
        .or_else(colour)
        .or_else(|| outline(MONO_EMOJI))
        .or_else(cjk)
}

/// The source and glyph a character is drawn with: Archivo's empty glyph (the
/// box) when no source has one.
fn pick(c: char) -> (Src, u16) {
    lookup(c).unwrap_or((Src::Outline(ARCHIVO), 0))
}

/// [`pick`] for the faces a block is set in: the named family first, when it has the character.
fn pick_in(faces: &Faces, c: char) -> (Src, u16) {
    if let Some(family) = faces.family {
        let g = family.font.glyph_id(c).0;
        if g != 0 {
            return (Src::Family, g);
        }
    }
    pick(c)
}

/// Em size range in permille of the frame height: 1% (a sliver) to 20%.
pub const SIZE_RANGE: (u32, u32) = (10, 200);

/// The em size a style gets when nothing says otherwise: 5% of the frame
/// height, the size a 1080p caption reads at.
pub const DEFAULT_SIZE: u32 = 50;

/// How far the text sits from the frame's edges, in fractions of the height:
/// the title-safe margin, the same on all four sides.
const MARGIN: f32 = 0.05;

/// The gap between two stacked blocks, in ems: enough that their boxes do not touch.
const STACK_GAP: f32 = 0.4;

/// The default box's opacity, 0 to 255: dark enough to read white text over a
/// bright picture, light enough that the picture still shows through.
const BOX_ALPHA: u8 = 153;

/// How the glyphs are edged. Thin and thick are a stroke of a fixed share of
/// the em, so the outline grows with the text rather than staying a hairline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Outline {
    #[default]
    None,
    Thin,
    Thick,
}

/// Where the block of text sits on the frame, vertically. Across the frame it
/// sits where [`HPlace`] says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    #[default]
    Bottom,
    Top,
    Centre,
}

/// Where the block of text sits across the frame: centred, or against the left
/// or right title-safe margin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HPlace {
    #[default]
    Centre,
    Left,
    Right,
}

/// How heavy the Latin letters are drawn: Archivo Medium, or Archivo Bold. Other
/// scripts are set in their one bundled weight either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Weight {
    #[default]
    Regular,
    Bold,
}

/// How the lines of one block line up against each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Centre,
    Left,
    Right,
}

/// Everything the rasteriser needs to draw a block of text. `Eq` on purpose:
/// a style is part of a project's saved state and of an export's settings, and
/// both compare by value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextStyle {
    /// Em size as permille of the frame height, clamped to [`SIZE_RANGE`].
    pub size: u32,
    pub outline: Outline,
    /// The translucent black box behind the whole block.
    pub boxed: bool,
    pub place: Place,
    /// Where the block sits across the frame.
    pub hplace: HPlace,
    /// The colour of the letters, `[r, g, b]`.
    pub ink: [u8; 3],
    pub weight: Weight,
    /// How the lines of a block line up inside it.
    pub align: Align,
    /// The box's colour and opacity, `[r, g, b, a]`; read only when `boxed`.
    pub box_color: [u8; 4],
    /// The system family the text is set in, by its name. `None`, or a name the system does not
    /// have, draws with the bundled faces; a character the family lacks falls back to them.
    pub family: Option<String>,
    /// How a burned caption is animated against its word times. Only the
    /// caption path ([`crate::captionanim::plan`]) reads it; a title ignores it.
    pub caption: crate::captionanim::CaptionStyle,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: DEFAULT_SIZE,
            outline: Outline::None,
            boxed: true,
            place: Place::Bottom,
            hplace: HPlace::Centre,
            ink: [255, 255, 255],
            weight: Weight::Regular,
            align: Align::Centre,
            box_color: [0, 0, 0, BOX_ALPHA],
            family: None,
            caption: crate::captionanim::CaptionStyle::default(),
        }
    }
}

impl TextStyle {
    /// The same style with its size inside [`SIZE_RANGE`]. Every entry point
    /// goes through it, so a hand-built or loaded style cannot draw nothing
    /// or a whole frame.
    pub fn clamped(&self) -> Self {
        Self {
            size: self.size.clamp(SIZE_RANGE.0, SIZE_RANGE.1),
            caption: self.caption.clamped(),
            ..self.clone()
        }
    }
}

/// The coverage of one block of text on one frame size, in three planes the
/// same size as the frame (one byte each, 255 is full), composed bottom to top:
/// the box, the outline, then the white glyphs; a colour glyph is composed last,
/// over those, from the `colour` plane. Only `bounds` can be non-zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlay {
    width: u32,
    height: u32,
    /// `[x0, y0, x1, y1)`, the rectangle every plane is confined to.
    bounds: (u32, u32, u32, u32),
    plate: Vec<u8>,
    stroke: Vec<u8>,
    fill: Vec<u8>,
    /// Colour glyphs, straight (not premultiplied) RGBA per pixel. Empty unless a
    /// colour glyph was drawn, so text without one carries no fourth plane.
    colour: Vec<[u8; 4]>,
    /// The colour the fill plane mixes toward (white unless the style says so).
    ink: [u8; 3],
    /// The colour the plate plane mixes toward (black unless the style says so).
    plate_rgb: [u8; 3],
}

impl Overlay {
    fn empty(width: u32, height: u32) -> Self {
        let n = width as usize * height as usize;
        Self {
            width,
            height,
            bounds: (0, 0, 0, 0),
            plate: vec![0; n],
            stroke: vec![0; n],
            fill: vec![0; n],
            colour: Vec::new(),
            ink: [255; 3],
            plate_rgb: [0; 3],
        }
    }

    /// The colour glyph at pixel `i`, when it covers the pixel at all.
    fn colour_at(&self, i: usize) -> Option<[u8; 4]> {
        self.colour.get(i).copied().filter(|c| c[3] > 0)
    }

    /// Whether any pixel is covered at all: a blank string or a size-less
    /// frame composes to nothing.
    pub fn is_blank(&self) -> bool {
        self.bounds.0 >= self.bounds.2 || self.bounds.1 >= self.bounds.3
    }

    /// The glyph coverage, one byte a pixel. Exposed for tests and for a
    /// caller that wants the shape without a canvas.
    pub fn fill(&self) -> &[u8] {
        &self.fill
    }

    /// The outline coverage: the stroke around the glyphs, where it is drawn
    /// under the white fill.
    pub fn stroke(&self) -> &[u8] {
        &self.stroke
    }

    /// Composes the text over a tightly packed BGRA canvas of this overlay's
    /// size. The canvas's own alpha byte is left as it was.
    pub fn blend_bgra(&self, canvas: &mut [u8]) {
        let (x0, y0, x1, y1) = self.bounds;
        let w = self.width as usize;
        if canvas.len() < w * self.height as usize * 4 {
            return;
        }
        for y in y0 as usize..y1 as usize {
            for x in x0 as usize..x1 as usize {
                let i = y * w + x;
                let (p, s, f) = (self.plate[i], self.stroke[i], self.fill[i]);
                let colour = self.colour_at(i);
                if p == 0 && s == 0 && f == 0 && colour.is_none() {
                    continue;
                }
                let px = &mut canvas[i * 4..i * 4 + 4];
                // The canvas is blue, green, red; `ink` and `plate_rgb` are red, green, blue.
                for (k, c) in px[..3].iter_mut().enumerate() {
                    let v = mix(*c, self.plate_rgb[2 - k], p);
                    let v = mix(v, 0, s);
                    *c = mix(v, self.ink[2 - k], f);
                }
                // The colour is BGRA-ordered on the canvas: blue, green, red.
                if let Some([r, g, b, a]) = colour {
                    px[0] = mix(px[0], b, a);
                    px[1] = mix(px[1], g, a);
                    px[2] = mix(px[2], r, a);
                }
            }
        }
    }

    /// Composes the text over planar I420 samples of this overlay's size, the
    /// planes an encoder reads. Luma is pulled to the black or white of the
    /// frame's own range (`full_range` as [`crate::colorspace::ColorDescription`]
    /// says it). Every colour here is grey, so chroma only moves toward 128,
    /// and a chroma sample is covered by the mean of its 2x2 luma pixels.
    pub fn blend_i420(&self, y: &mut [u8], u: &mut [u8], v: &mut [u8], full_range: bool) {
        let (w, h) = (self.width as usize, self.height as usize);
        if y.len() < w * h || u.len() < w.div_ceil(2) * h.div_ceil(2) || v.len() != u.len() {
            return;
        }
        let black = if full_range { 0 } else { 16 };
        // The matrix the frame is coded in, the one `colorspace` gives its size.
        let bt709 = !is_sd(self.width, self.height);
        let (ink_y, ink_cb, ink_cr) = yuv([self.ink[0], self.ink[1], self.ink[2], 255], bt709, full_range);
        let (plate_y, plate_cb, plate_cr) =
            yuv([self.plate_rgb[0], self.plate_rgb[1], self.plate_rgb[2], 255], bt709, full_range);
        let grey = self.ink[0] == self.ink[1]
            && self.ink[1] == self.ink[2]
            && self.plate_rgb[0] == self.plate_rgb[1]
            && self.plate_rgb[1] == self.plate_rgb[2];
        let (x0, y0, x1, y1) = self.bounds;
        let cw = w.div_ceil(2);
        // The total coverage of each luma pixel, the chroma's to average.
        let cover = |i: usize| -> u8 {
            let (p, s, f) = (
                self.plate[i] as u64,
                self.stroke[i] as u64,
                self.fill[i] as u64,
            );
            let k = self.colour_at(i).map_or(0, |c| c[3]) as u64;
            // Four factors over 255^3 is the three-factor cover over 255^2, exactly.
            let kept = (255 - p) * (255 - s) * (255 - f) * (255 - k);
            (255 - kept / (255 * 255 * 255)) as u8
        };
        for py in y0 as usize..y1 as usize {
            for px in x0 as usize..x1 as usize {
                let i = py * w + px;
                let (p, s, f) = (self.plate[i], self.stroke[i], self.fill[i]);
                let colour = self.colour_at(i);
                if p == 0 && s == 0 && f == 0 && colour.is_none() {
                    continue;
                }
                let mut luma = y[i];
                luma = mix(luma, plate_y, p);
                luma = mix(luma, black, s);
                luma = mix(luma, ink_y, f);
                if let Some(c) = colour {
                    let (yc, _, _) = yuv(c, bt709, full_range);
                    luma = mix(luma, yc, c[3]);
                }
                y[i] = luma;
            }
        }
        for cy in (y0 as usize / 2)..(y1 as usize).div_ceil(2) {
            for cx in (x0 as usize / 2)..(x1 as usize).div_ceil(2) {
                let mut sum = 0u32;
                let mut n = 0u32;
                // With a colour glyph in the block, each pixel pulls the chroma
                // toward its own colour's (weighted by its coverage) and a grey
                // pixel toward 128: `tu` and `tv` are those weighted targets.
                let mut coloured = false;
                let (mut tu, mut tv) = (0f32, 0f32);
                for dy in 0..2 {
                    for dx in 0..2 {
                        let (px, py) = (cx * 2 + dx, cy * 2 + dy);
                        if px < w && py < h {
                            let i = py * w + px;
                            let c = cover(i) as u32;
                            sum += c;
                            n += 1;
                            let (cb, cr) = match self.colour_at(i) {
                                Some(k) => {
                                    coloured = true;
                                    let (_, cb, cr) = yuv(k, bt709, full_range);
                                    (f32::from(cb), f32::from(cr))
                                }
                                None => (128.0, 128.0),
                            };
                            tu += c as f32 * cb;
                            tv += c as f32 * cr;
                        }
                    }
                }
                if n == 0 {
                    continue;
                }
                let ci = cy * cw + cx;
                if ci < u.len() {
                    if !grey {
                        // Coloured ink or box: each layer pulls the chroma toward its own, by its mean coverage.
                        let (mut p, mut s, mut f, mut ka) = (0u32, 0u32, 0u32, 0u32);
                        let (mut kcb, mut kcr) = (0f32, 0f32);
                        for dy in 0..2 {
                            for dx in 0..2 {
                                let (px, py) = (cx * 2 + dx, cy * 2 + dy);
                                if px < w && py < h {
                                    let i = py * w + px;
                                    p += u32::from(self.plate[i]);
                                    s += u32::from(self.stroke[i]);
                                    f += u32::from(self.fill[i]);
                                    if let Some(k) = self.colour_at(i) {
                                        let (_, cb, cr) = yuv(k, bt709, full_range);
                                        ka += u32::from(k[3]);
                                        kcb += f32::from(k[3]) * f32::from(cb);
                                        kcr += f32::from(k[3]) * f32::from(cr);
                                    }
                                }
                            }
                        }
                        let mean = |t: u32| (t / n) as u8;
                        for (c, plate, ink) in [(&mut u[ci], plate_cb, ink_cb), (&mut v[ci], plate_cr, ink_cr)] {
                            *c = mix(*c, plate, mean(p));
                            *c = mix(*c, 128, mean(s));
                            *c = mix(*c, ink, mean(f));
                        }
                        if ka > 0 {
                            let a = mean(ka);
                            u[ci] = mix(u[ci], (kcb / ka as f32).round() as u8, a);
                            v[ci] = mix(v[ci], (kcr / ka as f32).round() as u8, a);
                        }
                    } else if coloured {
                        let keep = 1.0 - sum as f32 / (255.0 * n as f32);
                        let target = 1.0 / (255.0 * n as f32);
                        u[ci] = (f32::from(u[ci]) * keep + tu * target)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        v[ci] = (f32::from(v[ci]) * keep + tv * target)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                    } else {
                        let a = (sum / n) as u8;
                        u[ci] = mix(u[ci], 128, a);
                        v[ci] = mix(v[ci], 128, a);
                    }
                }
            }
        }
    }
}

impl Overlay {
    /// The same coverage with every plane scaled by `alpha` (255 leaves it
    /// as it is, 0 draws nothing): what a title's fade is. Each plane is a
    /// mix toward its colour, so scaling the coverage scales the mix.
    pub fn faded(&self, alpha: u8) -> Self {
        let scale = |p: &[u8]| -> Vec<u8> {
            p.iter()
                .map(|&c| ((u32::from(c) * u32::from(alpha) + 127) / 255) as u8)
                .collect()
        };
        Self {
            width: self.width,
            height: self.height,
            bounds: self.bounds,
            ink: self.ink,
            plate_rgb: self.plate_rgb,
            plate: scale(&self.plate),
            stroke: scale(&self.stroke),
            fill: scale(&self.fill),
            colour: self
                .colour
                .iter()
                .map(|c| {
                    let a = (u32::from(c[3]) * u32::from(alpha) + 127) / 255;
                    [c[0], c[1], c[2], a as u8]
                })
                .collect(),
        }
    }
}

impl Overlay {
    /// The same coverage moved `dx` pixels right and `dy` down, what a title's slide is. The part
    /// that moves off the frame is cut, as the frame cuts any picture.
    pub fn moved(&self, dx: i32, dy: i32) -> Self {
        let (w, h) = (self.width as i64, self.height as i64);
        let shift = |p: &[u8]| -> Vec<u8> {
            let mut out = vec![0u8; p.len()];
            let (x0, y0, x1, y1) = self.bounds;
            for y in y0 as i64..y1 as i64 {
                for x in x0 as i64..x1 as i64 {
                    let (nx, ny) = (x + i64::from(dx), y + i64::from(dy));
                    if (0..w).contains(&nx) && (0..h).contains(&ny) {
                        out[(ny * w + nx) as usize] = p[(y * w + x) as usize];
                    }
                }
            }
            out
        };
        let (x0, y0, x1, y1) = self.bounds;
        let bounds = (
            (i64::from(x0) + i64::from(dx)).clamp(0, w) as u32,
            (i64::from(y0) + i64::from(dy)).clamp(0, h) as u32,
            (i64::from(x1) + i64::from(dx)).clamp(0, w) as u32,
            (i64::from(y1) + i64::from(dy)).clamp(0, h) as u32,
        );
        let mut colour = vec![[0u8; 4]; self.colour.len()];
        if !self.colour.is_empty() {
            for y in y0 as i64..y1 as i64 {
                for x in x0 as i64..x1 as i64 {
                    let (nx, ny) = (x + i64::from(dx), y + i64::from(dy));
                    if (0..w).contains(&nx) && (0..h).contains(&ny) {
                        colour[(ny * w + nx) as usize] = self.colour[(y * w + x) as usize];
                    }
                }
            }
        }
        Self {
            width: self.width,
            height: self.height,
            bounds,
            plate: shift(&self.plate),
            stroke: shift(&self.stroke),
            fill: shift(&self.fill),
            colour,
            ink: self.ink,
            plate_rgb: self.plate_rgb,
        }
    }
}

/// `dst` pulled toward `src` by `a / 255`.
fn mix(dst: u8, src: u8, a: u8) -> u8 {
    let a = u32::from(a);
    ((u32::from(dst) * (255 - a) + u32::from(src) * a + 127) / 255) as u8
}

/// The Y, Cb and Cr of a colour in the matrix (BT.709 or BT.601) and range a frame
/// is coded in: full range spans 0 to 255, limited range 16 to 235 and 16 to 240.
fn yuv(c: [u8; 4], bt709: bool, full_range: bool) -> (u8, u8, u8) {
    let (kr, kb) = if bt709 {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    };
    let kg = 1.0 - kr - kb;
    let (r, g, b) = (f32::from(c[0]), f32::from(c[1]), f32::from(c[2]));
    let luma = (kr * r + kg * g + kb * b) / 255.0;
    let cb = (b / 255.0 - luma) / (2.0 * (1.0 - kb));
    let cr = (r / 255.0 - luma) / (2.0 * (1.0 - kr));
    let (ys, cs, yo) = if full_range {
        (255.0, 255.0, 0.0)
    } else {
        (219.0, 224.0, 16.0)
    };
    let to_u8 = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    (
        to_u8(yo + ys * luma),
        to_u8(128.0 + cs * cb),
        to_u8(128.0 + cs * cr),
    )
}

/// A rectangle `(x0, y0, x1, y1)` in pixels, the edges as floats.
type Rect = (f32, f32, f32, f32);

/// The smallest rectangle holding both `acc` and `r`.
fn grow(acc: Option<Rect>, r: Rect) -> Option<Rect> {
    Some(match acc {
        None => r,
        Some(b) => (b.0.min(r.0), b.1.min(r.1), b.2.max(r.2), b.3.max(r.3)),
    })
}

/// Lays `lines` out in `style` on a `width` x `height` frame and returns the
/// coverage. Each entry of `lines` is a paragraph: they are wrapped to the
/// frame's width, never joined. A blank list or a zero-sized frame is an
/// overlay that [`Overlay::is_blank`].
pub fn overlay(style: &TextStyle, width: u32, height: u32, lines: &[String]) -> Overlay {
    overlay_stack(style, width, height, &[lines.to_vec()])
}

/// Lays several blocks of text out together and returns their coverage: each
/// block is one cue's lines, wrapped as [`overlay`] wraps them. The blocks
/// stack from the edge the style places them at, the first block nearest that
/// edge (for [`Place::Bottom`] the first sits lowest, and each later one sits
/// above it; [`Place::Top`] stacks downward; [`Place::Centre`] centres the
/// stack and puts the first lowest). Boxes are per block and never touch. A
/// block with no words takes no room.
pub fn overlay_stack(
    style: &TextStyle,
    width: u32,
    height: u32,
    blocks: &[Vec<String>],
) -> Overlay {
    let plain: Vec<Block> = blocks
        .iter()
        .map(|b| Block { lines: b.clone(), fx: Vec::new(), kara: Vec::new() })
        .collect();
    overlay_blocks(style, width, height, &plain)
}

/// What one word of an animated block is drawn with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordFx {
    /// Not drawn at all, but its room in the line is kept, so the words around it do not move.
    pub hidden: bool,
    /// The size of the word against its line, about its own centre.
    pub scale: f32,
    /// How far the word is lifted off the baseline, in ems.
    pub lift: f32,
    /// The highlight: its colour, and whether it is a rounded box behind white text
    /// rather than coloured text.
    pub highlight: Option<([u8; 3], bool)>,
}

impl Default for WordFx {
    fn default() -> Self {
        Self { hidden: false, scale: 1.0, lift: 0.0, highlight: None }
    }
}

/// One cue's text for [`overlay_blocks`]: its lines, and optionally one [`WordFx`] per
/// whitespace-separated word of them, in reading order. An `fx` of another length (or none)
/// draws the lines plainly, as [`overlay_stack`] does.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub lines: Vec<String>,
    pub fx: Vec<WordFx>,
    /// The karaoke syllables of an ASS cue, drawn when `fx` is empty.
    pub kara: Vec<Syl>,
}

/// One karaoke syllable of a [`Block`] as it looks now: which letters, and what colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Syl {
    /// The first of its characters, counting the block's non-whitespace characters from 0.
    pub from: usize,
    /// How many characters it covers.
    pub len: usize,
    /// The colour of the part of the syllable left of the split, and of the part right of it;
    /// `None` is the style's own ink.
    pub left: Option<[u8; 3]>,
    pub right: Option<[u8; 3]>,
    /// Where the split falls across the syllable's width, 0 to 1.
    pub split: f32,
    /// Drawn without the outline (an outline karaoke syllable that has not begun).
    pub bare: bool,
}

/// [`overlay_stack`] over blocks whose words may carry an effect.
pub fn overlay_blocks(style: &TextStyle, width: u32, height: u32, blocks: &[Block]) -> Overlay {
    overlay_blocks_shown(style, width, height, blocks, None)
}

/// The text laid out whole, with only its first `ink` letters (non-whitespace characters) drawn:
/// the rest keep their room, so a title that types on does not move. `ink` at or past the text's
/// letters draws all of it.
pub fn overlay_revealed(style: &TextStyle, width: u32, height: u32, lines: &[String], ink: usize) -> Overlay {
    overlay_blocks_shown(style, width, height, &[Block { lines: lines.to_vec(), fx: Vec::new(), kara: Vec::new() }], Some(ink))
}

/// [`overlay_blocks`] with an optional count of the letters drawn (see [`overlay_revealed`]).
fn overlay_blocks_shown(style: &TextStyle, width: u32, height: u32, blocks: &[Block], shown: Option<usize>) -> Overlay {
    let mut out = Overlay::empty(width, height);
    let blank = |b: &Block| b.lines.iter().all(|l| l.trim().is_empty());
    if width == 0 || height == 0 || blocks.iter().all(blank) {
        return out;
    }
    let style = style.clamped();
    let em = (style.size as f32 / 1000.0 * height as f32).max(6.0);
    let Some(faces) = Faces::with_style(em, &style) else {
        return out;
    };
    out.ink = style.ink;
    out.plate_rgb = [style.box_color[0], style.box_color[1], style.box_color[2]];
    let margin = MARGIN * height as f32;
    let max_w = (width as f32 - 2.0 * margin).max(em);
    let laid: Vec<(Vec<String>, &[WordFx], &[Syl])> = blocks
        .iter()
        .filter(|b| !blank(b))
        .map(|b| {
            let lines = wrap(&faces, &b.lines, max_w);
            // Wrapping that cut a word (one wider than the frame) loses the count: plain then.
            let words = lines.iter().map(|l| l.split_whitespace().count()).sum::<usize>();
            let fx: &[WordFx] = if b.fx.len() == words { &b.fx } else { &[] };
            let kara: &[Syl] = if fx.is_empty() { &b.kara } else { &[] };
            (lines, fx, kara)
        })
        .filter(|(w, _, _)| !w.is_empty())
        .collect();
    if laid.is_empty() {
        return out;
    }
    let line_h = (faces.text.height() + faces.text.line_gap()) * 1.1;
    let heights: Vec<f32> = laid.iter().map(|(w, _, _)| line_h * w.len() as f32).collect();
    let gap = em * STACK_GAP;
    let total = heights.iter().sum::<f32>() + gap * (laid.len() - 1) as f32;
    // The top of each block, in the order the blocks were given.
    let tops: Vec<f32> = match style.place {
        Place::Top => {
            let mut y = margin;
            heights
                .iter()
                .map(|h| {
                    let t = y;
                    y += h + gap;
                    t
                })
                .collect()
        }
        Place::Bottom | Place::Centre => {
            let mut y = match style.place {
                Place::Centre => (height as f32 + total) / 2.0,
                _ => height as f32 - margin,
            };
            heights
                .iter()
                .map(|h| {
                    y -= h;
                    let t = y;
                    y -= gap;
                    t
                })
                .collect()
        }
    };
    // Each block's own rectangle, which its box and the bounds are cut from.
    let mut rects: Vec<Rect> = Vec::with_capacity(laid.len());
    // The cells colour glyphs were painted in, which can reach past their line.
    let mut colour_cells: Option<Rect> = None;
    let mut scratch: Vec<u8> = Vec::new();
    // The letters of the lines already laid, against `shown`.
    let mut seen = 0usize;
    // Syllables with no outline, drawn once the outline is: `(text, x, baseline)`.
    let mut bare: Vec<(String, f32, f32)> = Vec::new();
    for ((lines, fx, kara), top) in laid.iter().zip(&tops) {
        let mut rect: Option<Rect> = None;
        let mut next_word = 0;
        let mut kara_seen = 0;
        let block_w = lines.iter().map(|l| string_width(&faces, l)).fold(0.0f32, f32::max);
        // The block's left edge: against a margin, or centred.
        let block_x = match style.hplace {
            HPlace::Left => margin,
            HPlace::Right => width as f32 - margin - block_w,
            HPlace::Centre => (width as f32 - block_w) / 2.0,
        };
        for (n, line) in lines.iter().enumerate() {
            let line_w = string_width(&faces, line);
            let x0 = match (style.hplace, style.align) {
                // Centred in a centred block is centred on the frame, as it always was.
                (HPlace::Centre, Align::Centre) => (width as f32 - line_w) / 2.0,
                (_, Align::Centre) => block_x + (block_w - line_w) / 2.0,
                (_, Align::Left) => block_x,
                (_, Align::Right) => block_x + block_w - line_w,
            };
            let baseline = top + n as f32 * line_h + faces.text.ascent();
            let r = (
                x0,
                top + n as f32 * line_h,
                x0 + line_w,
                top + (n as f32 + 1.0) * line_h,
            );
            rect = grow(rect, r);
            let cell = if !kara.is_empty() {
                let at = Pen { x0, baseline, top: top + n as f32 * line_h, line_h };
                draw_line_kara(&mut out, &faces, line, at, kara, &mut kara_seen, &mut scratch, &mut bare)
            } else if fx.is_empty() {
                let clip = reveal_clip(&faces, line, x0, shown, &mut seen);
                draw_line(&mut out, &faces, line, x0, baseline, clip)
            } else {
                let count = line.split_whitespace().count();
                let at = Pen { x0, baseline, top: top + n as f32 * line_h, line_h };
                let cell = draw_line_fx(&mut out, &faces, line, at, &fx[next_word..next_word + count], &mut scratch);
                next_word += count;
                cell
            };
            if let Some(cell) = cell {
                colour_cells = grow(colour_cells, cell);
            }
        }
        rects.extend(rect);
    }
    let stroke_r = match style.outline {
        Outline::None => 0,
        Outline::Thin => (em / 24.0).round().max(1.0) as u32,
        Outline::Thick => (em / 12.0).round().max(2.0) as u32,
    };
    // The bounds: every block, padded by the box and the stroke, inside the frame.
    let pad = em * 0.3 + stroke_r as f32;
    let mut bounds: Option<Rect> = colour_cells;
    for &(bx0, by0, bx1, by1) in &rects {
        bounds = grow(bounds, (bx0 - pad, by0 - pad, bx1 + pad, by1 + pad));
    }
    if let Some((x0, y0, x1, y1)) = bounds {
        out.bounds = (
            x0.floor().max(0.0) as u32,
            y0.floor().max(0.0) as u32,
            x1.ceil().min(width as f32) as u32,
            y1.ceil().min(height as f32) as u32,
        );
    }
    if stroke_r > 0 {
        // The outline goes round the white glyphs and the colour ones alike.
        let mut shape = out.fill.clone();
        for (s, c) in shape.iter_mut().zip(&out.colour) {
            *s = (*s).max(c[3]);
        }
        out.stroke = dilate(&shape, width, height, stroke_r);
    }
    for (text, x, baseline) in &bare {
        let ink = Some(out.ink);
        let reach = (*x, 0.0, x + string_width(&faces, text), height as f32);
        draw_split(&mut out, &faces, text, (*x, *baseline), reach, (f32::INFINITY, ink, ink), &mut scratch);
    }
    if style.boxed {
        for &(bx0, by0, bx1, by1) in &rects {
            let (px0, py0) = (
                (bx0 - em * 0.3).floor().max(0.0) as u32,
                (by0 - em * 0.15).floor().max(0.0) as u32,
            );
            let (px1, py1) = (
                (bx1 + em * 0.3).ceil().min(width as f32) as u32,
                (by1 + em * 0.15).ceil().min(height as f32) as u32,
            );
            for y in py0..py1 {
                let row = y as usize * width as usize;
                for x in px0..px1 {
                    out.plate[row + x as usize] = style.box_color[3];
                }
            }
        }
    }
    out
}

/// How much of the frame the text takes, for a warning about a title that fills it: the wrapped
/// line count and the share of the frame height its line boxes cover (0 to 1). `lines` are
/// paragraphs, as [`overlay`] takes them. A blank block or a zero-sized frame is `(0, 0.0)`.
/// Only the line boxes count, not the box or the stroke around them.
pub fn layout_share(style: &TextStyle, width: u32, height: u32, lines: &[String]) -> (usize, f32) {
    if width == 0 || height == 0 || lines.iter().all(|l| l.trim().is_empty()) {
        return (0, 0.0);
    }
    let style = style.clamped();
    let em = (style.size as f32 / 1000.0 * height as f32).max(6.0);
    let Some(faces) = Faces::with_style(em, &style) else {
        return (0, 0.0);
    };
    let margin = MARGIN * height as f32;
    let max_w = (width as f32 - 2.0 * margin).max(em);
    let count = wrap(&faces, lines, max_w).len();
    let line_h = (faces.text.height() + faces.text.line_gap()) * 1.1;
    (count, line_h * count as f32 / height as f32)
}

/// Lays `lines` out and composes them over a BGRA canvas: [`overlay`] followed
/// by [`Overlay::blend_bgra`]. Blank text leaves the canvas as it was.
pub fn draw_text(canvas: &mut [u8], width: u32, height: u32, style: &TextStyle, lines: &[String]) {
    overlay(style, width, height, lines).blend_bgra(canvas);
}

/// The characters in `lines` that neither bundled face has a glyph for, each
/// once and in the order they first appear: what a caption shows as an empty
/// box. Whitespace and the invisible joiners of an emoji sequence are not
/// missing. Empty when the faces are unavailable.
pub fn missing_glyphs(lines: &[String]) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    for c in lines.iter().flat_map(|l| l.chars()) {
        if c.is_whitespace() || is_invisible(c) || out.contains(&c) {
            continue;
        }
        if lookup(c).is_none() {
            out.push(c);
        }
    }
    out
}

/// Characters that take no room and draw nothing of their own: the zero-width
/// joiner that stitches an emoji sequence, and the variation selectors that
/// pick its presentation. They are skipped, never drawn as an empty box.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200D}' | '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

/// The faces one block is set in at one em size. The glyphs come from [`lookup`]
/// and the outline faces are parsed once for the whole process; this only holds
/// the em and Archivo's metrics, which a line is laid out on.
struct Faces {
    em: f32,
    /// Latin is set in Archivo Bold rather than Medium.
    bold: bool,
    text: Scaled<'static>,
    /// The system family the style names, drawn before the bundled faces when it has a glyph.
    family: Option<Family>,
}

impl Faces {
    #[cfg(test)]
    fn new(em: f32) -> Option<Self> {
        Self::with_weight(em, Weight::Regular)
    }

    fn with_weight(em: f32, weight: Weight) -> Option<Self> {
        let bold = weight == Weight::Bold;
        let text = outline_face(if bold { ARCHIVO_BOLD } else { ARCHIVO })?.as_scaled(PxScale::from(em));
        Some(Self { em, bold, text, family: None })
    }

    /// The faces a style is set in at `em`: its weight, and its family when the system has it.
    fn with_style(em: f32, style: &TextStyle) -> Option<Self> {
        let mut faces = Self::with_weight(em, style.weight)?;
        faces.family = style.family.as_deref().and_then(family_face);
        Some(faces)
    }

    /// The same faces at another em (a word that scales in an animation).
    fn rescaled(&self, em: f32) -> Option<Self> {
        let mut faces = Self::with_weight(em, if self.bold { Weight::Bold } else { Weight::Regular })?;
        faces.family = self.family;
        Some(faces)
    }

    /// The outline face `src` is drawn with at this em, or None for a colour
    /// glyph, which [`draw_colour`] paints instead.
    fn font(&self, src: Src) -> Option<Scaled<'static>> {
        match src {
            Src::Outline(i) => {
                let i = if self.bold && i == ARCHIVO { ARCHIVO_BOLD } else { i };
                let px = PxScale::from(self.em * face_k()[i]);
                Some(outline_face(i)?.as_scaled(px))
            }
            Src::Family => {
                let family = self.family?;
                Some(family.font.as_scaled(PxScale::from(self.em * family.k)))
            }
            Src::Cjk | Src::Colour => None,
        }
    }
}

/// One glyph of a laid-out line, in the order it is drawn. The pen moves by
/// `kern` before the glyph and by `advance` after it. The glyph itself is drawn
/// `shift` to the right of the pen and `lift` above the baseline, which moves
/// nothing (a mark sits on its base that way).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placed {
    src: Src,
    id: u16,
    kern: f32,
    shift: f32,
    lift: f32,
    advance: f32,
}

/// The runs of `s` in visual order, left to right across the line, each with
/// whether it is right-to-left. A line with no right-to-left text is one run.
fn visual_parts(s: &str) -> Vec<(bool, &str)> {
    // ASCII has no right-to-left character, and most captions are ASCII: skip the analysis.
    if s.is_ascii() {
        return vec![(false, s)];
    }
    let info = BidiInfo::new(s, None);
    if !info.has_rtl() {
        return vec![(false, s)];
    }
    info.paragraphs
        .iter()
        .flat_map(|para| {
            let (levels, runs) = info.visual_runs(para, para.range.clone());
            runs.into_iter()
                .map(|r| (levels[r.start].is_rtl(), &s[r]))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The characters of `part` split into runs of one source, in logical order.
/// Invisible characters are dropped.
fn segments(faces: &Faces, part: &str) -> Vec<(Src, String)> {
    let mut out: Vec<(Src, String)> = Vec::new();
    for ch in part.chars() {
        if is_invisible(ch) {
            continue;
        }
        let (src, _) = pick_in(faces, ch);
        match out.last_mut() {
            Some((s, text)) if *s == src => text.push(ch),
            _ => out.push((src, ch.to_string())),
        }
    }
    out
}

/// The glyphs of `s` in the order they are drawn, left to right across the line.
/// A right-to-left run is reversed into visual order and shaped right-to-left.
/// Invisible characters are dropped.
fn run(faces: &Faces, s: &str) -> Vec<Placed> {
    let mut out = Vec::new();
    for (rtl, part) in visual_parts(s) {
        let mut segs = segments(faces, part);
        if rtl {
            segs.reverse();
        }
        for (src, text) in segs {
            out.extend(place(faces, src, &text, rtl));
        }
    }
    out
}

/// The glyphs of one run of one source: shaped for the scripts that need it,
/// plain for Archivo and the emoji and CJK faces, painted from the colour font
/// for a colour glyph.
fn place(faces: &Faces, src: Src, text: &str, rtl: bool) -> Vec<Placed> {
    match src {
        Src::Outline(i) if (NOTO_SANS..MONO_EMOJI).contains(&i) => {
            shape(faces, i, text, rtl).unwrap_or_else(|| plain(faces, src, text))
        }
        Src::Colour => colour_glyphs(faces, text),
        Src::Cjk => cjk_glyphs(faces, text),
        _ => plain(faces, src, text),
    }
}

/// Advance-and-kern placement, the path Archivo has always been set with: each
/// glyph's advance and the pair kern before it, read from the face.
fn plain(faces: &Faces, src: Src, text: &str) -> Vec<Placed> {
    let Some(font) = faces.font(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut prev: Option<GlyphId> = None;
    for c in text.chars() {
        let id = font.glyph_id(c);
        let kern = prev.map_or(0.0, |p| font.kern(p, id));
        out.push(Placed {
            src,
            id: id.0,
            kern,
            shift: 0.0,
            lift: 0.0,
            advance: font.h_advance(id),
        });
        prev = Some(id);
    }
    out
}

/// Shapes one run with `rustybuzz`: joining forms, reordering and conjuncts come
/// from the face's own GSUB and GPOS. None when the face does not parse.
fn shape(faces: &Faces, i: usize, text: &str, rtl: bool) -> Option<Vec<Placed>> {
    let face = shaper(i)?;
    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(text);
    buf.set_direction(if rtl {
        rustybuzz::Direction::RightToLeft
    } else {
        rustybuzz::Direction::LeftToRight
    });
    let glyphs = rustybuzz::shape(face, &[], buf);
    // The same scale ab_glyph draws the outlines at: the em over the face's height.
    let scale = faces.em / outline_face(i)?.height_unscaled();
    Some(
        glyphs
            .glyph_infos()
            .iter()
            .zip(glyphs.glyph_positions())
            .map(|(info, pos)| Placed {
                src: Src::Outline(i),
                id: info.glyph_id as u16,
                kern: 0.0,
                shift: pos.x_offset as f32 * scale,
                lift: pos.y_offset as f32 * scale,
                advance: pos.x_advance as f32 * scale,
            })
            .collect(),
    )
}

/// The CJK glyphs of `text`: no shaping, each advancing by its own advance in the
/// variable face at Medium. One em is one unit of the face's own em.
fn cjk_glyphs(faces: &Faces, text: &str) -> Vec<Placed> {
    let Some(face) = cjk_face() else {
        return Vec::new();
    };
    let scale = faces.em / f32::from(face.units_per_em());
    text.chars()
        .filter_map(|c| face.glyph_index(c))
        .map(|g| Placed {
            src: Src::Cjk,
            id: g.0,
            kern: 0.0,
            shift: 0.0,
            lift: 0.0,
            advance: f32::from(face.glyph_hor_advance(g).unwrap_or(0)) * scale,
        })
        .collect()
}

/// The colour glyphs of `text`, each advancing by its own advance in the colour
/// font. Empty when the system has no colour font.
fn colour_glyphs(faces: &Faces, text: &str) -> Vec<Placed> {
    let Some(face) = colour_face() else {
        return Vec::new();
    };
    let scale = em_scale(faces.em, face);
    text.chars()
        .filter_map(|c| face.glyph_index(c))
        .map(|g| Placed {
            src: Src::Colour,
            id: g.0,
            kern: 0.0,
            shift: 0.0,
            lift: 0.0,
            advance: f32::from(face.glyph_hor_advance(g).unwrap_or(0)) * scale,
        })
        .collect()
}

/// Where a line sits: its pen start and baseline, and the box of the line.
struct Pen {
    x0: f32,
    baseline: f32,
    top: f32,
    line_h: f32,
}

/// [`draw_line`] a word at a time, each with its [`WordFx`]: a hidden word keeps its room and
/// draws nothing, a scaled or lifted one is drawn about its own place, and a highlighted one goes
/// to the colour plane (over a rounded box of the colour, when that is asked). `fx` has one entry
/// per word of `line`. Returns the cell the effects reached past the line's own.
fn draw_line_fx(
    out: &mut Overlay,
    faces: &Faces,
    line: &str,
    at: Pen,
    fx: &[WordFx],
    scratch: &mut Vec<u8>,
) -> Option<Rect> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let em = faces.em;
    let mut cells: Option<Rect> = None;
    for (i, (word, f)) in words.iter().zip(fx).enumerate() {
        if f.hidden {
            continue;
        }
        let before = if i == 0 { 0.0 } else { string_width(faces, &format!("{} ", words[..i].join(" "))) };
        let natural = string_width(faces, word);
        let scaled = faces.rescaled(em * f.scale.max(0.1));
        let face = scaled.as_ref().unwrap_or(faces);
        let width = string_width(face, word);
        let x = at.x0 + before + (natural - width) / 2.0;
        let baseline = at.baseline - f.lift * em;
        let reach = (x - 0.25 * em, baseline - 1.3 * face.em, x + width + 0.25 * em, baseline + 0.5 * face.em);
        match f.highlight {
            None => {
                if let Some(c) = draw_line(out, face, word, x, baseline, f32::INFINITY) {
                    cells = grow(cells, c);
                }
                if f.scale != 1.0 || f.lift != 0.0 {
                    cells = grow(cells, reach);
                }
            }
            Some((rgb, boxed)) => {
                let colour = if boxed {
                    let (px, py) = (0.2 * em, at.line_h * 0.04);
                    let r = (at.x0 + before - px, at.top + py, at.x0 + before + natural + px, at.top + at.line_h - py);
                    rounded_box(out, r, rgb);
                    cells = grow(cells, r);
                    [255, 255, 255]
                } else {
                    rgb
                };
                // The word is drawn into a clean fill plane, then moved to the colour plane.
                let n = out.fill.len();
                scratch.clear();
                scratch.resize(n, 0);
                std::mem::swap(&mut out.fill, scratch);
                let c = draw_line(out, face, word, x, baseline, f32::INFINITY);
                std::mem::swap(&mut out.fill, scratch);
                let (w, h) = (out.width as usize, out.height as usize);
                let (x0, x1) = (reach.0.max(0.0) as usize, (reach.2.ceil().max(0.0) as usize).min(w));
                let (y0, y1) = (reach.1.max(0.0) as usize, (reach.3.ceil().max(0.0) as usize).min(h));
                for py in y0..y1 {
                    for px in x0..x1 {
                        let a = scratch[py * w + px];
                        if a > 0 {
                            put_colour(out, py * w + px, colour, a);
                        }
                    }
                }
                cells = grow(cells, reach);
                if let Some(c) = c {
                    cells = grow(cells, c);
                }
            }
        }
    }
    cells
}

/// [`draw_line`] a line whose letters belong to karaoke syllables ([`Syl`]), a syllable at a time:
/// each at the place the line's own layout gives it, split in two colours where it is half sung.
/// `seen` counts the block's non-whitespace characters across its lines. A syllable without an
/// outline is not drawn but put in `bare`, for after the outline. Returns the cell the colours
/// reached.
#[allow(clippy::too_many_arguments)]
fn draw_line_kara(
    out: &mut Overlay,
    faces: &Faces,
    line: &str,
    at: Pen,
    kara: &[Syl],
    seen: &mut usize,
    scratch: &mut Vec<u8>,
    bare: &mut Vec<(String, f32, f32)>,
) -> Option<Rect> {
    // The syllable of each non-whitespace character (`None` for one no syllable covers).
    let letters: Vec<(usize, char)> = line.char_indices().filter(|(_, c)| !c.is_whitespace()).collect();
    let owner: Vec<Option<usize>> = letters
        .iter()
        .map(|_| {
            *seen += 1;
            kara.iter().position(|k| (k.from..k.from + k.len).contains(&(*seen - 1)))
        })
        .collect();
    let mut cells: Option<Rect> = None;
    let mut i = 0;
    while i < letters.len() {
        // The run of one syllable (the spaces inside it stay), or of unlit text.
        let mut j = i + 1;
        while j < letters.len() && owner[j] == owner[i] {
            j += 1;
        }
        let (start, (last, c)) = (letters[i].0, letters[j - 1]);
        let text = &line[start..last + c.len_utf8()];
        let x = at.x0 + string_width(faces, &line[..start]);
        match owner[i].map(|k| kara[k]) {
            None => {
                if let Some(c) = draw_line(out, faces, text, x, at.baseline, f32::INFINITY) {
                    cells = grow(cells, c);
                }
            }
            Some(k) if k.bare => bare.push((text.to_string(), x, at.baseline)),
            Some(k) => {
                let em = faces.em;
                let w = string_width(faces, text);
                let reach = (x - 0.25 * em, at.baseline - 1.3 * em, x + w + 0.25 * em, at.baseline + 0.5 * em);
                draw_split(out, faces, text, (x, at.baseline), reach, (x + w * k.split, k.left, k.right), scratch);
                cells = grow(cells, reach);
            }
        }
        i = j;
    }
    cells
}

/// The x past which `line` draws nothing under a reveal of `shown` letters: `seen` counts the
/// letters of the lines before it and is advanced past this one. Without a reveal, `INFINITY`.
fn reveal_clip(faces: &Faces, line: &str, x0: f32, shown: Option<usize>, seen: &mut usize) -> f32 {
    let Some(shown) = shown else { return f32::INFINITY };
    let ink = line.chars().filter(|c| !c.is_whitespace()).count();
    let here = shown.saturating_sub(*seen).min(ink);
    *seen += ink;
    if here == ink {
        return f32::INFINITY;
    }
    if here == 0 {
        return f32::NEG_INFINITY;
    }
    // Cut where the first letter not yet drawn begins.
    let cut = line.char_indices().filter(|(_, c)| !c.is_whitespace()).nth(here).map_or(line.len(), |(i, _)| i);
    x0 + string_width(faces, &line[..cut])
}

/// `text` drawn at `(x, baseline)` in two colours: the part left of column `split.0` in `split.1`,
/// the rest in `split.2` (`None` is the ink, in the fill plane; a colour goes to the colour plane).
/// Only the pixels inside `reach` are looked at.
fn draw_split(
    out: &mut Overlay,
    faces: &Faces,
    text: &str,
    (x, baseline): (f32, f32),
    reach: Rect,
    (split, left, right): (f32, Option<[u8; 3]>, Option<[u8; 3]>),
    scratch: &mut Vec<u8>,
) {
    // The glyphs go into a clean fill plane, then each pixel to the plane of its side.
    let n = out.fill.len();
    scratch.clear();
    scratch.resize(n, 0);
    std::mem::swap(&mut out.fill, scratch);
    draw_line(out, faces, text, x, baseline, f32::INFINITY);
    std::mem::swap(&mut out.fill, scratch);
    let (w, h) = (out.width as usize, out.height as usize);
    let (x0, x1) = (reach.0.max(0.0) as usize, (reach.2.ceil().max(0.0) as usize).min(w));
    let (y0, y1) = (reach.1.max(0.0) as usize, (reach.3.ceil().max(0.0) as usize).min(h));
    for py in y0..y1 {
        for px in x0..x1 {
            let a = scratch[py * w + px];
            if a == 0 {
                continue;
            }
            match if (px as f32 + 0.5) < split { left } else { right } {
                None => out.fill[py * w + px] = out.fill[py * w + px].max(a),
                Some(rgb) => put_colour(out, py * w + px, rgb, a),
            }
        }
    }
}

/// `rgb` laid over the colour plane's pixel `i` with coverage `a`: the straight-alpha "over".
fn put_colour(out: &mut Overlay, i: usize, rgb: [u8; 3], a: u8) {
    if out.colour.is_empty() {
        out.colour = vec![[0; 4]; out.width as usize * out.height as usize];
    }
    let d = out.colour[i];
    let (sa, da) = (f32::from(a) / 255.0, f32::from(d[3]) / 255.0);
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return;
    }
    let mut v = [0u8; 4];
    for k in 0..3 {
        let c = (f32::from(rgb[k]) * sa + f32::from(d[k]) * da * (1.0 - sa)) / oa;
        v[k] = c.round().clamp(0.0, 255.0) as u8;
    }
    v[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
    out.colour[i] = v;
}

/// A rounded rectangle of `rgb` in the colour plane, its corners a quarter of its height round.
fn rounded_box(out: &mut Overlay, r: Rect, rgb: [u8; 3]) {
    let (w, h) = (out.width as usize, out.height as usize);
    let rad = (r.3 - r.1) * 0.28;
    let (cx, cy) = ((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0);
    let (hx, hy) = ((r.2 - r.0) / 2.0 - rad, (r.3 - r.1) / 2.0 - rad);
    let (x0, x1) = (r.0.floor().max(0.0) as usize, (r.2.ceil().max(0.0) as usize).min(w));
    let (y0, y1) = (r.1.floor().max(0.0) as usize, (r.3.ceil().max(0.0) as usize).min(h));
    for py in y0..y1 {
        for px in x0..x1 {
            let (dx, dy) = ((px as f32 + 0.5 - cx).abs() - hx, (py as f32 + 0.5 - cy).abs() - hy);
            let d = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - rad;
            let a = (0.5 - d).clamp(0.0, 1.0);
            if a > 0.0 {
                put_colour(out, py * w + px, rgb, (a * 255.0).round() as u8);
            }
        }
    }
}

/// Draws one line's glyphs into `out`, the pen starting at `x0` on `baseline`.
/// Returns the cell a colour glyph was painted in, when the line has one.
/// `clip` is the x past which nothing of the line is drawn (`f32::INFINITY` for all of it): a
/// title that is still typing on draws the letters before its cut and keeps the rest's room.
fn draw_line(out: &mut Overlay, faces: &Faces, line: &str, x0: f32, baseline: f32, clip: f32) -> Option<Rect> {
    let (width, height) = (out.width, out.height);
    let (w, h) = (width as i32, height as i32);
    let mut caret = x0;
    let mut cells: Option<Rect> = None;
    for p in run(faces, line) {
        caret += p.kern;
        match p.src {
            Src::Colour if caret >= clip => {}
            Src::Colour => {
                if let Some(cell) = draw_colour(out, faces.em, p.id, caret, baseline) {
                    cells = grow(cells, cell);
                }
            }
            Src::Cjk if caret >= clip => {}
            Src::Cjk => {
                if let Some(face) = cjk_face() {
                    let scale = faces.em / f32::from(face.units_per_em());
                    let cell = draw_outline(out, face, p.id, scale, caret, baseline);
                    if let Some(cell) = cell {
                        cells = grow(cells, cell);
                    }
                }
            }
            src => {
                if let Some(font) = faces.font(src) {
                    let at = point(caret + p.shift, baseline - p.lift);
                    let glyph = GlyphId(p.id).with_scale_and_position(font.scale(), at);
                    if let Some(og) = font.outline_glyph(glyph) {
                        let b = og.px_bounds();
                        // A face other than Archivo can reach past its line: its ink joins the bounds.
                        if src != Src::Outline(ARCHIVO) {
                            let ink = (b.min.x, b.min.y, b.max.x, b.max.y);
                            cells = grow(cells, ink);
                        }
                        let fill = &mut out.fill;
                        og.draw(|gx, gy, c| {
                            let x = b.min.x as i32 + gx as i32;
                            let y = b.min.y as i32 + gy as i32;
                            if x >= 0 && y >= 0 && x < w && y < h && (x as f32) < clip {
                                let i = y as usize * width as usize + x as usize;
                                fill[i] = fill[i].max((c * 255.0).round() as u8);
                            }
                        });
                    }
                }
            }
        }
        caret += p.advance;
    }
    cells
}

/// Rasterises glyph `id` of `face` at `scale` pixels a unit, its origin at `(x, baseline)`,
/// into `out`'s fill (the larger coverage wins). Returns the cell it covers. This is
/// the path for a face ab_glyph cannot set an axis of, so it reads the outline itself.
fn draw_outline(
    out: &mut Overlay,
    face: &ttf_parser::Face<'_>,
    id: u16,
    scale: f32,
    x: f32,
    baseline: f32,
) -> Option<Rect> {
    let bb = face.glyph_bounding_box(ttf_parser::GlyphId(id))?;
    let (ex0, ey0) = (
        x + f32::from(bb.x_min) * scale,
        baseline - f32::from(bb.y_max) * scale,
    );
    let (ex1, ey1) = (
        x + f32::from(bb.x_max) * scale,
        baseline - f32::from(bb.y_min) * scale,
    );
    // The cell, one pixel wider than the glyph and clipped to the frame.
    let (cx0, cy0) = ((ex0.floor() - 1.0).max(0.0), (ey0.floor() - 1.0).max(0.0));
    let (cx1, cy1) = (
        (ex1.ceil() + 1.0).min(out.width as f32),
        (ey1.ceil() + 1.0).min(out.height as f32),
    );
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    let (w, h) = ((cx1 - cx0) as usize, (cy1 - cy0) as usize);
    let mut ras = ab_glyph_rasterizer::Rasterizer::new(w, h);
    let mut path = CellPath {
        ras: &mut ras,
        t: IDENTITY,
        scale,
        origin: (x - cx0, baseline - cy0),
        last: ab_glyph_rasterizer::point(0.0, 0.0),
        start: ab_glyph_rasterizer::point(0.0, 0.0),
    };
    face.outline_glyph(ttf_parser::GlyphId(id), &mut path);
    // A CFF glyph may end on an implicitly closed contour: close it here, as ab_glyph does.
    ttf_parser::OutlineBuilder::close(&mut path);
    let stride = out.width as usize;
    let (ox, oy) = (cx0 as usize, cy0 as usize);
    let fill = &mut out.fill;
    ras.for_each_pixel_2d(|px, py, cov| {
        let i = (oy + py as usize) * stride + ox + px as usize;
        fill[i] = fill[i].max((cov.clamp(0.0, 1.0) * 255.0).round() as u8);
    });
    Some((cx0, cy0, cx1, cy1))
}

/// The font at one size, as [`FontRef::as_scaled`] hands it out.
type Scaled<'a> = ab_glyph::PxScaleFont<&'a FontRef<'static>>;

/// The width of `s` in pixels as the faces set it, kerning included.
fn string_width(faces: &Faces, s: &str) -> f32 {
    run(faces, s).iter().map(|p| p.kern + p.advance).sum()
}

/// The matrix of a rotation-free affine map `[a, b, c, d, e, f]`: `x' = a*x + c*y + e`,
/// `y' = b*x + d*y + f`. `outer` applied after `inner`.
fn compose(outer: [f32; 6], inner: [f32; 6]) -> [f32; 6] {
    let [a1, b1, c1, d1, e1, f1] = outer;
    let [a2, b2, c2, d2, e2, f2] = inner;
    [
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    ]
}

const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Paints one colour glyph of the system colour font: each layer's outline is
/// rasterised in its paint and composited over the layers below. Clip paths and
/// blend modes are not applied, and a gradient is painted as the mean of its
/// stops, so each layer is one flat colour.
struct ColourPainter<'a> {
    face: &'a ttf_parser::Face<'a>,
    /// Font units to cell pixels.
    scale: f32,
    /// The glyph origin in cell pixels.
    origin: (f32, f32),
    width: usize,
    height: usize,
    /// The transform stack, the top one applied to every outline.
    transforms: Vec<[f32; 6]>,
    /// The outline the next paint fills.
    glyph: Option<ttf_parser::GlyphId>,
    /// Premultiplied RGBA per cell pixel.
    canvas: Vec<[f32; 4]>,
}

impl ColourPainter<'_> {
    fn top(&self) -> [f32; 6] {
        self.transforms.last().copied().unwrap_or(IDENTITY)
    }
}

/// Feeds an outline to the rasteriser in cell pixels, through a transform.
struct CellPath<'r> {
    ras: &'r mut ab_glyph_rasterizer::Rasterizer,
    t: [f32; 6],
    scale: f32,
    origin: (f32, f32),
    last: ab_glyph_rasterizer::Point,
    start: ab_glyph_rasterizer::Point,
}

impl CellPath<'_> {
    fn map(&self, x: f32, y: f32) -> ab_glyph_rasterizer::Point {
        let [a, b, c, d, e, f] = self.t;
        let (tx, ty) = (a * x + c * y + e, b * x + d * y + f);
        ab_glyph_rasterizer::point(
            self.origin.0 + tx * self.scale,
            self.origin.1 - ty * self.scale,
        )
    }
}

impl ttf_parser::OutlineBuilder for CellPath<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.start = p;
        self.last = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.ras.draw_line(self.last, p);
        self.last = p;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.map(x1, y1), self.map(x, y));
        self.ras.draw_quad(self.last, c, p);
        self.last = p;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.map(x1, y1), self.map(x2, y2), self.map(x, y));
        self.ras.draw_cubic(self.last, c1, c2, p);
        self.last = p;
    }

    fn close(&mut self) {
        self.ras.draw_line(self.last, self.start);
        self.last = self.start;
    }
}

impl<'a> ttf_parser::colr::Painter<'a> for ColourPainter<'a> {
    fn outline_glyph(&mut self, glyph_id: ttf_parser::GlyphId) {
        self.glyph = Some(glyph_id);
    }

    fn paint(&mut self, paint: ttf_parser::colr::Paint<'a>) {
        use ttf_parser::colr::Paint;
        let Some(glyph) = self.glyph else {
            return;
        };
        let colour = match paint {
            Paint::Solid(c) => c,
            Paint::LinearGradient(g) => mean_stop(g.stops(0, &[])),
            Paint::RadialGradient(g) => mean_stop(g.stops(0, &[])),
            Paint::SweepGradient(g) => mean_stop(g.stops(0, &[])),
        };
        let (w, h) = (self.width, self.height);
        let mut ras = ab_glyph_rasterizer::Rasterizer::new(w, h);
        let mut path = CellPath {
            ras: &mut ras,
            t: self.top(),
            scale: self.scale,
            origin: self.origin,
            last: ab_glyph_rasterizer::point(0.0, 0.0),
            start: ab_glyph_rasterizer::point(0.0, 0.0),
        };
        self.face.outline_glyph(glyph, &mut path);
        ttf_parser::OutlineBuilder::close(&mut path);
        let alpha = f32::from(colour.alpha) / 255.0;
        let rgb = [
            f32::from(colour.red) / 255.0,
            f32::from(colour.green) / 255.0,
            f32::from(colour.blue) / 255.0,
        ];
        let canvas = &mut self.canvas;
        ras.for_each_pixel_2d(|x, y, cover| {
            let sa = cover.clamp(0.0, 1.0) * alpha;
            if sa <= 0.0 || x as usize >= w || y as usize >= h {
                return;
            }
            let d = &mut canvas[y as usize * w + x as usize];
            let keep = 1.0 - sa;
            *d = [
                rgb[0] * sa + d[0] * keep,
                rgb[1] * sa + d[1] * keep,
                rgb[2] * sa + d[2] * keep,
                sa + d[3] * keep,
            ];
        });
    }

    fn push_clip(&mut self) {}

    fn push_clip_box(&mut self, _clipbox: ttf_parser::colr::ClipBox) {}

    fn pop_clip(&mut self) {}

    fn push_layer(&mut self, _mode: ttf_parser::colr::CompositeMode) {}

    fn pop_layer(&mut self) {}

    fn push_transform(&mut self, t: ttf_parser::Transform) {
        let inner = [t.a, t.b, t.c, t.d, t.e, t.f];
        self.transforms.push(compose(self.top(), inner));
    }

    fn pop_transform(&mut self) {
        self.transforms.pop();
    }
}

/// The mean of a gradient's colour stops: the one flat colour a gradient is painted in.
fn mean_stop(stops: impl Iterator<Item = ttf_parser::colr::ColorStop>) -> ttf_parser::RgbaColor {
    let (mut n, mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32, 0u32);
    for s in stops {
        n += 1;
        r += u32::from(s.color.red);
        g += u32::from(s.color.green);
        b += u32::from(s.color.blue);
        a += u32::from(s.color.alpha);
    }
    let mean = |v: u32| (v / n.max(1)) as u8;
    ttf_parser::RgbaColor {
        red: mean(r),
        green: mean(g),
        blue: mean(b),
        alpha: mean(a),
    }
}

/// The scale that sets a face's units at `em`, the way ab_glyph sets its outlines:
/// the em over the face's height (ascender less descender), not its units per em.
fn em_scale(em: f32, face: &ttf_parser::Face<'_>) -> f32 {
    em / (f32::from(face.ascender()) - f32::from(face.descender()))
}

/// Paints the system colour glyph `id` with its origin at `pen_x` on `baseline`
/// into `out`'s colour plane. Returns the cell painted, or None when there is no
/// colour font, the glyph has no colour layers, or nothing landed on the frame.
fn draw_colour(out: &mut Overlay, em: f32, id: u16, pen_x: f32, baseline: f32) -> Option<Rect> {
    let face = colour_face()?;
    let scale = em_scale(em, face);
    // The cell: the em, with room for the overhang an emoji draws past it.
    let (x0, y0) = ((pen_x - 0.25 * em).floor(), (baseline - 1.5 * em).floor());
    let (x1, y1) = ((pen_x + 1.75 * em).ceil(), (baseline + 0.5 * em).ceil());
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut painter = ColourPainter {
        face,
        scale,
        origin: (pen_x - x0, baseline - y0),
        width: w,
        height: h,
        transforms: Vec::new(),
        glyph: None,
        canvas: vec![[0.0; 4]; w * h],
    };
    let black = ttf_parser::RgbaColor {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 255,
    };
    face.paint_color_glyph(ttf_parser::GlyphId(id), 0, black, &mut painter)?;
    let (ow, oh) = (out.width as i64, out.height as i64);
    let mut hit = false;
    for cy in 0..h {
        for cx in 0..w {
            let [r, g, b, a] = painter.canvas[cy * w + cx];
            let (x, y) = (x0 as i64 + cx as i64, y0 as i64 + cy as i64);
            if a <= 0.0 || x < 0 || y < 0 || x >= ow || y >= oh {
                continue;
            }
            if out.colour.is_empty() {
                out.colour = vec![[0; 4]; out.width as usize * out.height as usize];
            }
            let to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
            let straight = [r / a, g / a, b / a].map(|v| to_u8(v.min(1.0)));
            let alpha = to_u8(a);
            if alpha == 0 {
                continue;
            }
            out.colour[y as usize * out.width as usize + x as usize] =
                [straight[0], straight[1], straight[2], alpha];
            hit = true;
        }
    }
    hit.then_some((x0, y0, x1, y1))
}

/// Word wrap. A paragraph that fits the frame stays one line, so a cue's own
/// line breaks are kept. A paragraph that does not fit breaks into the fewest
/// lines it can, balanced by [`balanced`], so no line is left an orphan word.
/// A blank paragraph is an empty line, which keeps a deliberate gap in a title
/// card.
fn wrap(faces: &Faces, lines: &[String], max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    for para in lines {
        let words: Vec<&str> = para.split_whitespace().collect();
        if words.is_empty() {
            out.push(String::new());
            continue;
        }
        let whole = words.join(" ");
        if string_width(faces, &whole) <= max_w {
            out.push(whole);
        } else if words.iter().any(|w| string_width(faces, w) > max_w) {
            // A word wider than the frame has no balanced break: cut it greedily.
            out.extend(greedy(faces, &words, max_w));
        } else {
            let n = greedy(faces, &words, max_w).len();
            out.extend(balanced(faces, &words, n, max_w));
        }
    }
    out
}

/// Greedy word wrap: breaks at the spaces where a line would overflow `max_w`,
/// and a single word wider than the frame breaks by character.
fn greedy(faces: &Faces, words: &[&str], max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in words {
        let candidate = match line.is_empty() {
            true => word.to_string(),
            false => format!("{line} {word}"),
        };
        if string_width(faces, &candidate) <= max_w {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        // The word alone may still be wider than the frame: cut it.
        let mut piece = String::new();
        for ch in word.chars() {
            piece.push(ch);
            if string_width(faces, &piece) > max_w && piece.chars().count() > 1 {
                piece.pop();
                out.push(std::mem::take(&mut piece));
                piece.push(ch);
            }
        }
        line = piece;
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// Breaks `words` into exactly `n` lines that each fit `max_w`, choosing the
/// split that minimises the sum of squared line widths, so the lines come out
/// even. `n` must be the fewest lines that fit (what [`greedy`] returns); no
/// word may be wider than `max_w`. Falls back to [`greedy`] if no split fits.
fn balanced(faces: &Faces, words: &[&str], n: usize, max_w: f32) -> Vec<String> {
    let m = words.len();
    let width = |i: usize, j: usize| string_width(faces, &words[i..j].join(" "));
    // best[k][j]: least cost of covering the first `j` words with `k` lines.
    let mut best = vec![vec![f32::INFINITY; m + 1]; n + 1];
    let mut from = vec![vec![0usize; m + 1]; n + 1];
    best[0][0] = 0.0;
    for k in 1..=n {
        for j in 1..=m {
            for i in 0..j {
                if !best[k - 1][i].is_finite() {
                    continue;
                }
                let w = width(i, j);
                if w > max_w {
                    continue;
                }
                let cost = best[k - 1][i] + w * w;
                if cost < best[k][j] {
                    best[k][j] = cost;
                    from[k][j] = i;
                }
            }
        }
    }
    if !best[n][m].is_finite() {
        return greedy(faces, words, max_w);
    }
    let mut out = Vec::with_capacity(n);
    let (mut j, mut k) = (m, n);
    while k > 0 {
        let i = from[k][j];
        out.push(words[i..j].join(" "));
        j = i;
        k -= 1;
    }
    out.reverse();
    out
}

/// The coverage grown by `r` pixels in every direction: a square max filter,
/// separable, so two passes of a 1-D window. Clamped at the frame's edges.
fn dilate(src: &[u8], w: u32, h: u32, r: u32) -> Vec<u8> {
    let (w, h, r) = (w as usize, h as usize, r as usize);
    let mut tmp = vec![0u8; w * h];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        for x in 0..w {
            let lo = x.saturating_sub(r);
            let hi = (x + r).min(w - 1);
            tmp[y * w + x] = row[lo..=hi].iter().copied().max().unwrap_or(0);
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let lo = y.saturating_sub(r);
        let hi = (y + r).min(h - 1);
        for x in 0..w {
            out[y * w + x] = (lo..=hi).map(|yy| tmp[yy * w + x]).max().unwrap_or(0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 640;
    const H: u32 = 360;

    fn lines(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    /// The faces at the size a 1080p frame's caption is set at.
    fn caption_faces() -> Faces {
        Faces::new(60.0 / 1000.0 * 1920.0).unwrap()
    }

    /// The rows of `o`'s glyph coverage that hold any ink, top to bottom.
    fn ink_rows(o: &Overlay) -> Vec<u32> {
        let w = o.width as usize;
        (0..o.height as usize)
            .filter(|&y| o.fill[y * w..(y + 1) * w].iter().any(|&c| c > 0))
            .map(|y| y as u32)
            .collect()
    }

    #[test]
    fn draws_non_zero_pixels_over_a_black_canvas() {
        let mut canvas = vec![0u8; (W * H * 4) as usize];
        let style = TextStyle {
            boxed: false,
            ..TextStyle::default()
        };
        draw_text(&mut canvas, W, H, &style, &lines("Hello"));
        assert!(canvas.iter().any(|&b| b != 0), "no text was drawn");
        // Every lit pixel is white or a blend toward it: the colour channels
        // move together over black, so B, G and R agree.
        let lit = canvas
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 0)
            .count();
        assert!(lit > 100, "only {lit} lit pixels for five letters");
    }

    /// The x range `[min, max]` of `o`'s glyph coverage.
    fn ink_cols(o: &Overlay) -> (usize, usize) {
        let w = o.width as usize;
        let cols: Vec<usize> = (0..w)
            .filter(|&x| (0..o.height as usize).any(|y| o.fill[y * w + x] > 0))
            .collect();
        (cols[0], *cols.last().unwrap())
    }

    #[test]
    fn a_title_colour_tints_the_glyphs_in_bgra_and_in_chroma() {
        let style = TextStyle { ink: [255, 0, 0], boxed: false, ..TextStyle::default() };
        let o = overlay(&style, W, H, &lines("MMMM"));
        let mut canvas = vec![0u8; (W * H * 4) as usize];
        o.blend_bgra(&mut canvas);
        let i = o.fill.iter().position(|&c| c == 255).expect("a solid glyph pixel");
        assert_eq!(&canvas[i * 4..i * 4 + 3], &[0, 0, 255], "blue, green, red");
        // Planar: red pulls Cr far above 128 and Cb below it.
        let (mut y, mut u, mut v) = (vec![16u8; (W * H) as usize], vec![128u8; (W * H / 4) as usize], vec![128u8; (W * H / 4) as usize]);
        o.blend_i420(&mut y, &mut u, &mut v, false);
        assert!(v.iter().any(|&c| c > 200), "red text raises Cr");
        assert!(u.iter().all(|&c| c <= 128), "red text lowers Cb");
        // The default is white: chroma stays grey.
        let white = overlay(&TextStyle { boxed: false, ..TextStyle::default() }, W, H, &lines("MMMM"));
        let (mut y, mut u, mut v) = (vec![16u8; (W * H) as usize], vec![128u8; (W * H / 4) as usize], vec![128u8; (W * H / 4) as usize]);
        white.blend_i420(&mut y, &mut u, &mut v, false);
        assert!(u.iter().chain(&v).all(|&c| c == 128));
    }

    #[test]
    fn a_box_colour_and_opacity_paint_the_plate() {
        let style = TextStyle { box_color: [0, 0, 255, 255], ..TextStyle::default() };
        let o = overlay(&style, W, H, &lines("Hi"));
        let mut canvas = vec![0u8; (W * H * 4) as usize];
        o.blend_bgra(&mut canvas);
        let i = o.plate.iter().position(|&a| a == 255).expect("an opaque plate pixel");
        assert!(o.fill[i] == 0, "a plate pixel clear of ink");
        assert_eq!(&canvas[i * 4..i * 4 + 3], &[255, 0, 0], "a blue box is blue in BGRA");
        let faint = overlay(&TextStyle { box_color: [0, 0, 0, 40], ..TextStyle::default() }, W, H, &lines("Hi"));
        assert_eq!(faint.plate.iter().copied().max(), Some(40));
    }

    #[test]
    fn bold_is_wider_and_the_block_goes_to_the_side_asked() {
        let regular = overlay(&TextStyle { boxed: false, ..TextStyle::default() }, W, H, &lines("Heading"));
        let bold = overlay(&TextStyle { boxed: false, weight: Weight::Bold, ..TextStyle::default() }, W, H, &lines("Heading"));
        let sum = |o: &Overlay| o.fill.iter().map(|&c| u64::from(c)).sum::<u64>();
        assert!(sum(&bold) > sum(&regular), "bold has more ink");
        let at = |hplace| ink_cols(&overlay(&TextStyle { hplace, boxed: false, ..TextStyle::default() }, W, H, &lines("Heading")));
        let (l, c, r) = (at(HPlace::Left), at(HPlace::Centre), at(HPlace::Right));
        assert!(l.0 < c.0 && c.0 < r.0, "{l:?} {c:?} {r:?}");
        assert!(l.0 as f32 <= MARGIN * H as f32 + 8.0, "left sits on the margin");
        assert!((W as f32 - r.1 as f32) <= MARGIN * H as f32 + 8.0, "right sits on the margin");
    }

    #[test]
    fn lines_align_inside_the_block() {
        let two = vec!["a much longer line".to_string(), "short".to_string()];
        let short_left = |align| {
            let o = overlay(&TextStyle { align, hplace: HPlace::Left, boxed: false, ..TextStyle::default() }, W, H, &two[1..]);
            ink_cols(&o).0
        };
        // `short` alone is its own block, so alignment cannot move it; with the long line it can.
        assert_eq!(short_left(Align::Left), short_left(Align::Right));
        let both = |align| {
            // The bottom line of the two-line block is the second one (stack bottom-up is per block, lines go down).
            let o = overlay(&TextStyle { align, hplace: HPlace::Left, boxed: false, ..TextStyle::default() }, W, H, &two);
            let rows = ink_rows(&o);
            let bottom = *rows.last().unwrap() as usize;
            let w = W as usize;
            (0..w).find(|&x| o.fill[(bottom - 3) * w + x] > 0 || o.fill[(bottom - 6) * w + x] > 0).unwrap()
        };
        assert!(both(Align::Left) < both(Align::Centre) && both(Align::Centre) < both(Align::Right));
    }

    #[test]
    fn a_default_look_blends_as_it_always_did() {
        // White, regular, centred, on the 60% black box: the exact planes of the old rasteriser.
        let o = overlay(&TextStyle::default(), W, H, &lines("Hello"));
        assert_eq!(o.ink, [255; 3]);
        assert_eq!(o.plate_rgb, [0; 3]);
        assert_eq!(o.plate.iter().copied().max(), Some(BOX_ALPHA));
    }

    #[test]
    fn a_blank_string_or_an_empty_frame_draws_nothing() {
        let mut canvas = vec![7u8; (W * H * 4) as usize];
        draw_text(&mut canvas, W, H, &TextStyle::default(), &lines("   "));
        assert!(canvas.iter().all(|&b| b == 7));
        assert!(overlay(&TextStyle::default(), 0, 0, &lines("x")).is_blank());
    }

    #[test]
    fn a_long_line_wraps_into_at_least_two_lines() {
        // The em of a 640x360 frame at the default size, against a 320px width.
        let faces = Faces::new(DEFAULT_SIZE as f32 / 1000.0 * 360.0).unwrap();
        let long =
            "a considerably longer caption that cannot possibly fit on one line of a small frame";
        let wrapped = wrap(&faces, &[long.to_string()], 320.0);
        assert!(wrapped.len() >= 2, "wrapped to {wrapped:?}");
        assert!(
            wrapped
                .iter()
                .all(|l| string_width(&faces, l) <= 320.0 + 0.5)
        );
        // And the words survive the break, in order.
        assert_eq!(wrapped.join(" "), long);
    }

    /// The caption layout the burn uses: a 1080x1920 frame at size 60, where
    /// the text width is the frame less its margins.
    fn caption_wrap(lines: &[&str]) -> Vec<String> {
        let faces = caption_faces();
        let max_w = 1080.0 - 2.0 * MARGIN * 1920.0;
        let owned: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        wrap(&faces, &owned, max_w)
    }

    #[test]
    fn a_cue_line_that_fits_keeps_its_own_break() {
        assert_eq!(
            caption_wrap(&["Forty years ago", "this coast"]),
            ["Forty years ago", "this coast"]
        );
    }

    #[test]
    fn a_cue_line_too_wide_breaks_balanced_with_no_orphan_word() {
        let out = caption_wrap(&["Forty years ago, this", "coast was empty sand"]);
        // Both cue lines are too wide, so each splits in two, evenly, and no
        // line is left holding a single word.
        assert_eq!(
            out,
            ["Forty years", "ago, this", "coast was", "empty sand"],
            "wrapped to {out:?}"
        );
    }

    #[test]
    fn balanced_splits_evenly_and_never_past_the_width() {
        let faces = caption_faces();
        let words = ["aaaa", "bbbb", "cccc", "dddd", "eeee", "ffff"];
        let max_w = string_width(&faces, "aaaa bbbb cccc dddd") + 1.0;
        let out = balanced(&faces, &words, 2, max_w);
        assert_eq!(out, ["aaaa bbbb cccc", "dddd eeee ffff"]);
        assert!(out.iter().all(|l| string_width(&faces, l) <= max_w));
    }

    #[test]
    fn an_outline_puts_dark_pixels_around_white_glyphs() {
        // A mid-grey canvas, so the outline has something to darken and the
        // fill something to brighten.
        let style = TextStyle {
            outline: Outline::Thick,
            boxed: false,
            ..TextStyle::default()
        };
        let o = overlay(&style, W, H, &lines("Ill"));
        let fill = o.fill();
        let stroke = o.stroke();
        let ring = fill
            .iter()
            .zip(stroke)
            .filter(|(f, s)| **f == 0 && **s > 0)
            .count();
        assert!(ring > 50, "only {ring} outline pixels around the glyphs");
        let mut canvas = vec![128u8; (W * H * 4) as usize];
        o.blend_bgra(&mut canvas);
        let dark = fill
            .iter()
            .zip(stroke)
            .enumerate()
            .filter(|(_, (f, s))| **f == 0 && **s > 0)
            .filter(|(i, _)| canvas[i * 4] < 128)
            .count();
        assert_eq!(dark, ring, "every outline pixel must darken the grey");
    }

    #[test]
    fn the_i420_overlay_moves_luma_to_white_and_chroma_to_grey() {
        let style = TextStyle {
            boxed: false,
            ..TextStyle::default()
        };
        let o = overlay(&style, W, H, &lines("Hi"));
        let (cw, ch) = ((W / 2) as usize, (H / 2) as usize);
        let mut y = vec![40u8; (W * H) as usize];
        let mut u = vec![200u8; cw * ch];
        let mut v = vec![60u8; cw * ch];
        o.blend_i420(&mut y, &mut u, &mut v, false);
        let whitest = y.iter().copied().max().unwrap();
        assert_eq!(whitest, 235, "limited-range white is 235");
        assert!(
            u.iter().any(|&s| s != 200),
            "chroma under text must move to 128"
        );
        assert!(y.contains(&40), "pixels off the text must be untouched");
    }

    #[test]
    fn an_emoji_is_drawn_from_a_face_not_as_a_box() {
        // Archivo has no pictograph, so each of these goes to another source.
        for emoji in ["\u{1F3AC}", "\u{1F30D}", "\u{1F1F9}", "\u{1F1F7}"] {
            let (src, id) = pick(emoji.chars().next().unwrap());
            assert_ne!(src, Src::Outline(ARCHIVO), "{emoji} came from Archivo");
            assert_ne!(id, 0, "{emoji} has no glyph");
        }
        // A letter stays on Archivo.
        assert_eq!(pick('a').0, Src::Outline(ARCHIVO));
        // The emoji leaves ink on the frame, where a box would too: the test
        // is that it is not the empty glyph, so compare against a rare codepoint
        // that no face has. Ink is any pixel the canvas took.
        let ink = |s: &str| {
            let o = overlay(
                &TextStyle {
                    boxed: false,
                    ..TextStyle::default()
                },
                W,
                H,
                &lines(s),
            );
            let mut canvas = vec![0u8; (W * H * 4) as usize];
            o.blend_bgra(&mut canvas);
            canvas
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[0] | p[1] | p[2] > 0)
                .count()
        };
        let emoji_ink = ink("\u{1F3AC}");
        assert!(
            emoji_ink > 30,
            "only {emoji_ink} ink pixels for the clapper"
        );
        assert_ne!(emoji_ink, ink("\u{10FFFD}"), "emoji drew the empty box");
    }

    #[test]
    fn a_zero_width_joiner_or_selector_draws_nothing_and_is_not_missing() {
        // The woman-astronaut sequence and a heart with its selector: each
        // character is a glyph or invisible, and only the glyphs count.
        assert!(missing_glyphs(&lines("\u{1F469}\u{200D}\u{1F680}")).is_empty());
        assert!(missing_glyphs(&lines("\u{2764}\u{FE0F}")).is_empty());
        assert_eq!(string_width(&caption_faces(), "\u{FE0F}"), 0.0);
    }

    #[test]
    fn a_long_title_reports_its_wrapped_lines_and_its_share_of_the_frame() {
        let style = TextStyle::default();
        let (n, share) = layout_share(&style, 1280, 720, &lines("Hello"));
        assert_eq!(n, 1);
        assert!(share < 0.1, "{share}");
        assert_eq!(layout_share(&style, 1280, 720, &lines("   ")), (0, 0.0));
        let (n, share) = layout_share(&style, 1280, 720, &lines(&"word ".repeat(100)));
        assert!(n > 6 && share > 0.4, "{n} lines, share {share}");
    }

    #[test]
    fn cyrillic_arabic_and_the_other_scripts_have_faces_and_name_no_missing_glyphs() {
        assert!(missing_glyphs(&lines("Привет мир · مرحبا · שלום · नमस्ते · สวัสดี")).is_empty());
        assert_eq!(
            pick('П'),
            (
                Src::Outline(NOTO_SANS),
                outline_face(NOTO_SANS).unwrap().glyph_id('П').0
            )
        );
        assert_eq!(pick('م').0, Src::Outline(2), "Arabic is on its own face");
        assert_eq!(pick('ש').0, Src::Outline(3), "Hebrew is on its own face");
        assert_eq!(
            pick('न').0,
            Src::Outline(4),
            "Devanagari is on its own face"
        );
        assert_eq!(pick('ส').0, Src::Outline(5), "Thai is on its own face");
        assert!(missing_glyphs(&lines("Plain Latin, 1080p! 50%")).is_empty());
    }

    #[test]
    fn every_script_face_has_archivos_x_height_and_archivo_is_unscaled() {
        // Archivo is the reference: its factor is exactly 1, so Latin is untouched.
        let k = face_k();
        assert_eq!(k[ARCHIVO], 1.0);
        let x_px = |i: usize| {
            let face = ttf_parser::Face::parse(OUTLINE_BYTES[i], 0).unwrap();
            let x = f32::from(face.x_height().unwrap());
            x * k[i] / outline_face(i).unwrap().height_unscaled()
        };
        let text = x_px(ARCHIVO);
        for i in [NOTO_SANS, 2, 3, 4, 5] {
            assert!(
                (x_px(i) - text).abs() < 1e-4,
                "face {i}: {} vs {text}",
                x_px(i)
            );
        }
    }

    #[test]
    fn cyrillic_text_is_drawn_in_ink_not_boxes() {
        let style = TextStyle {
            boxed: false,
            ..TextStyle::default()
        };
        let ink = |s: &str| {
            let o = overlay(&style, W, H, &lines(s));
            o.fill().iter().filter(|&&c| c > 0).count()
        };
        // The empty box is the only thing a missing glyph would draw: a Cyrillic
        // letter draws ink of its own, not that box.
        let box_ink = ink("\u{10FFFD}");
        let letter = ink("\u{41F}");
        assert!(
            letter > 30,
            "only {letter} ink pixels for a Cyrillic capital"
        );
        assert_ne!(letter, box_ink, "the Cyrillic letter drew the empty box");
    }

    #[test]
    fn arabic_letters_join_so_a_medial_letter_is_not_its_isolated_form() {
        // The base letter of each word is the glyph with an advance; the dot marks
        // beside it have none. Alone the letter takes its isolated form, but set
        // twice in a row the first takes the joining form and the last the final.
        let faces = caption_faces();
        let bases = |s: &str| -> Vec<u16> {
            run(&faces, s)
                .iter()
                .filter(|p| p.advance > 0.0)
                .map(|p| p.id)
                .collect()
        };
        let alone = bases("\u{628}");
        let joined = bases("\u{628}\u{628}");
        assert_eq!(alone.len(), 1);
        assert_eq!(joined.len(), 2, "joined letters stay two letters");
        assert_ne!(
            joined[0], alone[0],
            "joining did not change the first letter"
        );
        assert_ne!(
            joined[1], alone[0],
            "the final letter is not in its isolated form"
        );
    }

    #[test]
    fn hebrew_is_drawn_right_to_left() {
        let faces = caption_faces();
        // "alef bet": in logical order alef comes first, so on the page bet is left.
        assert_eq!(
            visual_parts("\u{5D0}\u{5D1}"),
            vec![(true, "\u{5D0}\u{5D1}")]
        );
        let placed = run(&faces, "\u{5D0}\u{5D1}");
        let bet = outline_face(3).unwrap().glyph_id('\u{5D1}').0;
        let alef = outline_face(3).unwrap().glyph_id('\u{5D0}').0;
        assert_eq!(placed.len(), 2);
        assert_eq!(placed[0].id, bet, "bet should be drawn first, on the left");
        assert_eq!(
            placed[1].id, alef,
            "alef should follow, to the right of bet"
        );
    }

    #[test]
    fn devanagari_matra_is_reordered_before_its_consonant() {
        let faces = caption_faces();
        // KA with the vowel sign I: the sign is shaped into its pre-base form and
        // written before KA, so the first glyph on the page is not KA.
        let placed = run(&faces, "\u{915}\u{93F}");
        let ka = outline_face(4).unwrap().glyph_id('\u{915}').0;
        assert_eq!(placed.len(), 2, "{placed:?}");
        assert_eq!(placed[1].id, ka, "KA should come second on the page");
        assert_ne!(
            placed[0].id, ka,
            "the vowel sign is not reordered before KA"
        );
    }

    #[test]
    fn a_cjk_character_is_drawn_from_the_system_font_when_there_is_one() {
        let Some(_) = cjk_face() else {
            return; // no system CJK font here: the box is the right answer
        };
        assert_eq!(pick('你').0, Src::Cjk);
        let style = TextStyle {
            boxed: false,
            ..TextStyle::default()
        };
        let o = overlay(&style, W, H, &lines("你好"));
        assert!(
            o.fill().iter().filter(|&&c| c > 0).count() > 100,
            "CJK drew nothing"
        );
    }

    #[test]
    fn a_colour_emoji_paints_colour_pixels_when_there_is_a_system_colour_font() {
        let Some(_) = colour_face() else {
            return; // no colour font here: the monochrome outline is the answer
        };
        assert_eq!(pick('\u{1F389}').0, Src::Colour);
        let style = TextStyle {
            boxed: false,
            ..TextStyle::default()
        };
        let o = overlay(&style, W, H, &lines("\u{1F389}"));
        let mut canvas = vec![0u8; (W * H * 4) as usize];
        o.blend_bgra(&mut canvas);
        let coloured = canvas
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) > 40)
            .count();
        assert!(
            coloured > 30,
            "only {coloured} coloured pixels for the party popper"
        );
    }

    #[test]
    fn a_mixed_script_title_draws_and_keeps_its_glyphs() {
        let style = TextStyle {
            outline: Outline::Thin,
            ..TextStyle::default()
        };
        // The bundled faces cover every script but CJK, and the colour emoji
        // falls back to the bundled monochrome one, so only the CJK part needs
        // a system face; without it the CJK characters are the missing glyphs.
        let bundled = lines("Привет мир · مرحبا · \u{1F389}");
        assert!(missing_glyphs(&bundled).is_empty());
        let text = lines("Привет мир · مرحبا · 你好 · \u{1F389}");
        if cjk_face().is_some() {
            assert!(missing_glyphs(&text).is_empty());
        } else {
            assert_eq!(missing_glyphs(&text), vec!['你', '好']);
        }
        let (n, share) = layout_share(&style, W, H, &text);
        assert_eq!(n, 1, "the title should fit one line");
        assert!(share > 0.0);
        let o = overlay(&style, W, H, &text);
        assert!(o.fill().iter().any(|&c| c > 0));
    }

    #[test]
    fn missing_glyphs_names_only_what_no_face_draws_once_in_order() {
        let text = vec![
            "Emoji test \u{1F3AC} ok".to_string(),
            "\u{10FFFD} and \u{10FFFD} again \u{10FFFC}".to_string(),
        ];
        assert_eq!(missing_glyphs(&text), vec!['\u{10FFFD}', '\u{10FFFC}']);
        assert!(missing_glyphs(&lines("plain ascii, 1080p")).is_empty());
    }

    #[test]
    fn overlapping_cues_stack_upward_from_the_bottom_without_covering_each_other() {
        let style = TextStyle {
            boxed: false,
            ..TextStyle::default()
        };
        let first = overlay(&style, W, H, &lines("First cue"));
        let second = overlay(&style, W, H, &lines("Second cue"));
        let both = overlay_stack(&style, W, H, &[lines("First cue"), lines("Second cue")]);
        // The first cue keeps its place on the bottom edge, exactly as alone.
        let first_rows = ink_rows(&first);
        let both_rows = ink_rows(&both);
        assert_eq!(first_rows.last(), both_rows.last(), "first cue moved");
        // The second cue sits wholly above the first: the stack reaches higher
        // than the first cue alone by at least its own line height.
        let (top_alone, top_both) = (first_rows[0], both_rows[0]);
        assert!(
            top_both + 20 < top_alone,
            "second cue not stacked above: {top_both} vs {top_alone}"
        );
        // Each cue's ink is the same as when drawn alone (the same glyphs), so
        // the stack adds the two ink counts without any pixel shared.
        let ink = |o: &Overlay| o.fill().iter().filter(|&&c| c > 0).count();
        let (a, b, s) = (ink(&first), ink(&second), ink(&both));
        assert!(
            (s as i64 - (a + b) as i64).abs() <= ((a + b) / 50) as i64,
            "stack ink {s} is not the sum of its cues {a} + {b}"
        );
        // The gap between the two cues keeps their boxes apart too.
        let gap_rows = both_rows.windows(2).filter(|w| w[1] - w[0] > 1).count();
        assert!(gap_rows >= 1, "the two cues run into one another");
    }

    #[test]
    fn a_stacked_cue_with_no_words_takes_no_room() {
        let style = TextStyle::default();
        let alone = overlay(&style, W, H, &lines("Hello"));
        let with_blank = overlay_stack(&style, W, H, &[lines("   "), lines("Hello")]);
        assert_eq!(alone.fill(), with_blank.fill());
    }

    const LATIN_BGRA_HASH: u64 = 17148267790522720847;
    const LATIN_I420_HASHES: [u64; 3] = [
        3766897427118592358,
        14679306852776097573,
        14679306852776097573,
    ];

    /// FNV-1a, 64 bits: a hash that does not change with the Rust release.
    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    #[test]
    fn latin_text_renders_exactly_as_the_archivo_only_rasteriser_did() {
        // Pinned before the script faces, the shaper and the colour glyphs came in:
        // a Latin title must not move a pixel. The text is set in the default
        // size with outline, box and a second line of punctuation.
        let style = TextStyle {
            outline: Outline::Thick,
            boxed: true,
            ..TextStyle::default()
        };
        let text = lines("Hello, world! 1080p 50% “quotes” — dashes…\nSecond line: fi ffl AV");
        let o = overlay(&style, W, H, &text);
        let mut canvas = vec![90u8; (W * H * 4) as usize];
        o.blend_bgra(&mut canvas);
        assert_eq!(fnv1a(&canvas), LATIN_BGRA_HASH, "bgra hash moved");
        let (cw, ch) = ((W / 2) as usize, (H / 2) as usize);
        let (mut y, mut u, mut v) = (
            vec![60u8; (W * H) as usize],
            vec![128u8; cw * ch],
            vec![128u8; cw * ch],
        );
        o.blend_i420(&mut y, &mut u, &mut v, false);
        let planes = [fnv1a(&y), fnv1a(&u), fnv1a(&v)];
        assert_eq!(planes, LATIN_I420_HASHES, "i420 hashes moved");
    }
}
