//! Where a title card sits over the preview's picture, how much of it shows at
//! a frame, and which titles are up. Pure geometry, so the preview
//! ([`crate::ui::preview`]) draws what this says and the tests pin it without a
//! window. The numbers are the engine's ([`engine::text`]): the same margins,
//! the same size as a share of the picture height, the same box padding and
//! outline weights, so a title reads where the export burns it.

use engine::Title;
use engine::text::{Outline, TextStyle};

/// The title-safe margin, as a share of the picture height, on all four sides.
pub(crate) const MARGIN: f32 = 0.05;

/// The engine's line pitch in em: Archivo's ascent plus descent and line gap
/// (878 + 210 + 0 over 1000 units), times the engine's 1.1 leading.
pub(crate) const LINE_EM: f32 = (0.878 + 0.210) * 1.1;

/// The box behind the words, in em: the horizontal and vertical padding.
pub(crate) const BOX_PAD_X_EM: f32 = 0.3;
pub(crate) const BOX_PAD_Y_EM: f32 = 0.15;

/// The smallest em the engine draws at, in px.
const MIN_EM_PX: f32 = 6.0;

/// A rectangle in window pixels, or in the region's own local pixels: the
/// caller says which.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
}

/// The rectangle a `frame_w` x `frame_h` picture takes when it is fitted inside
/// `region` and centred, the way the picture itself is drawn
/// ([`crate::render::letterboxed_image`], `ObjectFit::Contain`). An empty
/// picture or region has no picture to fit: the region comes back as it is.
pub(crate) fn contain(region: Rect, frame_w: f32, frame_h: f32) -> Rect {
    if frame_w <= 0. || frame_h <= 0. || region.w <= 0. || region.h <= 0. {
        return region;
    }
    let scale = (region.w / frame_w).min(region.h / frame_h);
    let (w, h) = (frame_w * scale, frame_h * scale);
    Rect {
        x: region.x + (region.w - w) / 2.,
        y: region.y + (region.h - h) / 2.,
        w,
        h,
    }
}

/// The em size in px for a style: its size in permille of the picture's
/// displayed height, as the engine reads it (`size / 1000 * frame height`),
/// with the engine's six-pixel floor.
pub(crate) fn em_px(style: &TextStyle, picture_h: f32) -> f32 {
    (style.clamped().size as f32 / 1000. * picture_h).max(MIN_EM_PX)
}

/// The pitch of one line of a title at `em`.
pub(crate) fn line_px(em: f32) -> f32 {
    em * LINE_EM
}

/// The area a title's words are set in over `picture`: the picture inset by
/// the title-safe margin on all four sides. Words wrap to its width, and sit
/// at the top, the middle or the bottom of its height as the style's place
/// says: the drawn block's own height depends on how the words wrap, so the
/// vertical placement is the caller's to make inside this rectangle.
pub(crate) fn title_area(picture: Rect) -> Rect {
    let margin = MARGIN * picture.h;
    Rect {
        x: picture.x + margin,
        y: picture.y + margin,
        w: (picture.w - 2. * margin).max(0.),
        h: (picture.h - 2. * margin).max(0.),
    }
}

/// The copies that draw the outline, in px offsets off the text, the engine's
/// stroke radius for the style's em: a radius of em/24 (thin) or em/12 (thick),
/// at least one and two px. Four diagonals for thin, eight around for thick.
pub(crate) fn outline_offsets(outline: Outline, em: f32) -> Vec<(f32, f32)> {
    let r = match outline {
        Outline::None => return Vec::new(),
        Outline::Thin => (em / 24.).round().max(1.),
        Outline::Thick => (em / 12.).round().max(2.),
    };
    match outline {
        Outline::Thick => vec![
            (-r, 0.),
            (r, 0.),
            (0., -r),
            (0., r),
            (-r, -r),
            (r, -r),
            (-r, r),
            (r, r),
        ],
        _ => vec![(-r, -r), (r, -r), (-r, r), (r, r)],
    }
}

/// The opacity of a title at its frame: its 0 to 255 alpha as a fraction.
pub(crate) fn opacity(alpha: u8) -> f32 {
    f32::from(alpha) / 255.
}

/// The titles up at timeline `frame`, in list order (a later one draws over an
/// earlier one), each with its alpha. A title whose fade has it at nothing is
/// not up.
pub(crate) fn active_titles(titles: &[Title], frame: u32) -> Vec<(&Title, u8)> {
    titles
        .iter()
        .filter_map(|t| match t.alpha_at(frame) {
            0 => None,
            a => Some((t, a)),
        })
        .collect()
}
