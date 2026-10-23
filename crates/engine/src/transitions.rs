//! Transition kinds: how a clip's tail ([`crate::Clip::transition_out`]) is
//! drawn into the clip after it.
//!
//! The kind changes the picture only. The sound under the overlap is the
//! crossfade it always was. Preview ([`crate::PlaybackSession`]) and export both
//! come here: [`blend_planes`] for I420 at 8 or 10 bits, [`blend_bgra`] for the
//! preview's screen pictures, and both read the same [`Geo::mix`], so the two
//! cannot disagree about a kind.
//!
//! `t` is the share of the transition done, from just above 0 (A) to just below
//! 1 (B), as [`crate::export::dissolve_weight`] ramps it. A direction names the
//! way things *move*: `wipe-left` moves its edge leftward, so B shows from the
//! right edge and at the midpoint the left half is still A and the right half is
//! B; `slide-left` brings B in from the right over A; `push-left` pushes A out to
//! the left with B behind it.

use crate::scale::{Sample, chroma_dims};

/// Where a wipe, slide or push moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    pub const ALL: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];

    pub fn name(self) -> &'static str {
        match self {
            Dir::Left => "left",
            Dir::Right => "right",
            Dir::Up => "up",
            Dir::Down => "down",
        }
    }

    pub fn parse(s: &str) -> Option<Dir> {
        Dir::ALL.into_iter().find(|d| d.name() == s)
    }
}

/// What a transition looks like. [`TransitionKind::Dissolve`] is the default
/// and what every project before the library meant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransitionKind {
    #[default]
    Dissolve,
    DipBlack,
    DipWhite,
    Wipe(Dir),
    Slide(Dir),
    Push(Dir),
    Zoom,
}

impl TransitionKind {
    /// One of each shape (a direction shape with `Left`), for pickers.
    pub const SHAPES: [TransitionKind; 7] = [
        TransitionKind::Dissolve,
        TransitionKind::DipBlack,
        TransitionKind::DipWhite,
        TransitionKind::Wipe(Dir::Left),
        TransitionKind::Slide(Dir::Left),
        TransitionKind::Push(Dir::Left),
        TransitionKind::Zoom,
    ];

    /// The shape's name without a direction: `dissolve`, `dip-black`,
    /// `dip-white`, `wipe`, `slide`, `push`, `zoom`.
    pub fn shape_name(self) -> &'static str {
        match self {
            TransitionKind::Dissolve => "dissolve",
            TransitionKind::DipBlack => "dip-black",
            TransitionKind::DipWhite => "dip-white",
            TransitionKind::Wipe(_) => "wipe",
            TransitionKind::Slide(_) => "slide",
            TransitionKind::Push(_) => "push",
            TransitionKind::Zoom => "zoom",
        }
    }

    pub fn dir(self) -> Option<Dir> {
        match self {
            TransitionKind::Wipe(d) | TransitionKind::Slide(d) | TransitionKind::Push(d) => Some(d),
            _ => None,
        }
    }

    /// The shape with direction `dir`; a shape without a direction is unchanged.
    pub fn with_dir(self, dir: Dir) -> TransitionKind {
        match self {
            TransitionKind::Wipe(_) => TransitionKind::Wipe(dir),
            TransitionKind::Slide(_) => TransitionKind::Slide(dir),
            TransitionKind::Push(_) => TransitionKind::Push(dir),
            other => other,
        }
    }

    /// The shape named `shape` (see [`Self::shape_name`]) moving `dir`
    /// (`Left` when not given).
    pub fn from_parts(shape: &str, dir: Option<Dir>) -> Option<TransitionKind> {
        let d = dir.unwrap_or(Dir::Left);
        Some(match shape {
            "dissolve" => TransitionKind::Dissolve,
            "dip-black" => TransitionKind::DipBlack,
            "dip-white" => TransitionKind::DipWhite,
            "wipe" => TransitionKind::Wipe(d),
            "slide" => TransitionKind::Slide(d),
            "push" => TransitionKind::Push(d),
            "zoom" => TransitionKind::Zoom,
            _ => return None,
        })
    }

    /// The full name, as the project file writes it: `wipe-left`, `dip-black`.
    pub fn name(self) -> String {
        match self.dir() {
            Some(d) => format!("{}-{}", self.shape_name(), d.name()),
            None => self.shape_name().to_string(),
        }
    }

    pub fn parse(s: &str) -> Option<TransitionKind> {
        if let Some((shape, dir)) = s.rsplit_once('-')
            && let Some(d) = Dir::parse(dir)
        {
            return TransitionKind::from_parts(shape, Some(d)).filter(|k| k.dir().is_some());
        }
        TransitionKind::from_parts(s, None).filter(|k| k.dir().is_none())
    }

    /// The shape's name for a card: `Dissolve`, `Dip to black`, `Wipe`.
    pub fn title(self) -> &'static str {
        match self {
            TransitionKind::Dissolve => "Dissolve",
            TransitionKind::DipBlack => "Dip to black",
            TransitionKind::DipWhite => "Dip to white",
            TransitionKind::Wipe(_) => "Wipe",
            TransitionKind::Slide(_) => "Slide",
            TransitionKind::Push(_) => "Push",
            TransitionKind::Zoom => "Zoom",
        }
    }

    /// A number that differs for every kind and direction, for telling two
    /// kinds apart without comparing them (the GUI's before/after check).
    pub fn ordinal(self) -> u32 {
        let shape = TransitionKind::SHAPES
            .iter()
            .position(|k| k.shape_name() == self.shape_name())
            .unwrap_or(0) as u32;
        let dir = self.dir().and_then(|d| Dir::ALL.iter().position(|&x| x == d)).unwrap_or(0) as u32;
        shape * 4 + dir
    }

    /// The words the timeline shows on the overlap: `Dissolve`, `Dip to black`,
    /// `Wipe left`.
    pub fn label(self) -> String {
        match self.dir() {
            Some(d) => format!("{} {}", self.title(), d.name()),
            None => self.title().to_string(),
        }
    }
}

/// The kind of the transition the clip on screen at timeline `frame` owes its
/// successor (the one [`crate::export::dissolve_window`] found).
pub(crate) fn kind_at(project: &crate::Project, frame: u32) -> TransitionKind {
    project
        .composite_clip_at(frame)
        .and_then(|(lane, idx)| project.lane(lane).get(idx))
        .map_or_else(TransitionKind::default, |c| c.transition_kind)
}

/// The softness of a wipe's edge, as a share of the picture's width or height.
const SOFT: f32 = 0.1;
/// The cross-zoom's scale: A grows from 1 to `ZOOM_A`, B shrinks from `ZOOM_B` to 1.
const ZOOM_A: f32 = 1.25;
const ZOOM_B: f32 = 1.2;

/// What one output pixel is made of: `wa` of A at (`ax`, `ay`), `wb` of B at
/// (`bx`, `by`) and `wd` of the dip colour. The weights sum to 1.
struct Mix {
    ax: usize,
    ay: usize,
    bx: usize,
    by: usize,
    wa: f32,
    wb: f32,
    wd: f32,
}

/// One plane's geometry. A slide or push shifts by an even number of luma
/// pixels, so a plane with `sub` times fewer pixels moves by exactly its share.
struct Geo {
    kind: TransitionKind,
    t: f32,
    w: usize,
    h: usize,
    /// Luma pixels per plane pixel: 1, or 2 for chroma.
    sub: usize,
    luma_w: usize,
    luma_h: usize,
}

impl Geo {
    fn new(kind: TransitionKind, t: f32, luma_w: usize, luma_h: usize, sub: usize) -> Geo {
        let (w, h) = if sub == 2 {
            chroma_dims(luma_w, luma_h)
        } else {
            (luma_w, luma_h)
        };
        Geo {
            kind,
            t: t.clamp(0.0, 1.0),
            w,
            h,
            sub,
            luma_w,
            luma_h,
        }
    }

    /// The shift along an axis of `luma_n` luma pixels, in this plane's pixels.
    fn shift(&self, luma_n: usize, n: usize) -> usize {
        let luma = ((self.t * luma_n as f32 / 2.0).round() as usize * 2).min(luma_n);
        (luma / self.sub).min(n)
    }

    fn mix(&self, x: usize, y: usize) -> Mix {
        let same = |wa: f32, wb: f32, wd: f32| Mix {
            ax: x,
            ay: y,
            bx: x,
            by: y,
            wa,
            wb,
            wd,
        };
        let t = self.t;
        match self.kind {
            TransitionKind::Dissolve => same(1.0 - t, t, 0.0),
            TransitionKind::DipBlack | TransitionKind::DipWhite => {
                if t < 0.5 {
                    same(1.0 - 2.0 * t, 0.0, 2.0 * t)
                } else {
                    same(0.0, 2.0 * t - 1.0, 2.0 - 2.0 * t)
                }
            }
            TransitionKind::Zoom => {
                let sa = 1.0 + (ZOOM_A - 1.0) * t;
                let sb = ZOOM_B + (1.0 - ZOOM_B) * t;
                let at = |s: f32, p: usize, n: usize| -> usize {
                    let c = n as f32 / 2.0;
                    ((c + ((p as f32 + 0.5) - c) / s).floor().max(0.0) as usize).min(n - 1)
                };
                Mix {
                    ax: at(sa, x, self.w),
                    ay: at(sa, y, self.h),
                    bx: at(sb, x, self.w),
                    by: at(sb, y, self.h),
                    wa: 1.0 - t,
                    wb: t,
                    wd: 0.0,
                }
            }
            TransitionKind::Wipe(d) | TransitionKind::Slide(d) | TransitionKind::Push(d) => {
                let horizontal = matches!(d, Dir::Left | Dir::Right);
                let (c, n, luma_n) = if horizontal {
                    (x, self.w, self.luma_w)
                } else {
                    (y, self.h, self.luma_h)
                };
                // Work as if moving toward increasing c': a leftward or upward
                // move has its axis flipped, and is flipped back at the end.
                let flip = matches!(d, Dir::Left | Dir::Up);
                let cp = if flip { n - 1 - c } else { c };
                let back = |v: usize| if flip { n - 1 - v } else { v };
                // (a, b, wb): the position each side reads, and B's weight.
                let (a, b, wb) = match self.kind {
                    TransitionKind::Wipe(_) => {
                        let front = t * (1.0 + SOFT);
                        let u = (cp as f32 + 0.5) / n as f32;
                        (cp, cp, ((front - u) / SOFT).clamp(0.0, 1.0))
                    }
                    TransitionKind::Slide(_) => {
                        let s = self.shift(luma_n, n);
                        if cp < s {
                            (cp, cp + n - s, 1.0)
                        } else {
                            (cp, cp, 0.0)
                        }
                    }
                    _ => {
                        let s = self.shift(luma_n, n);
                        if cp < s {
                            (0, cp + n - s, 1.0)
                        } else {
                            (cp - s, 0, 0.0)
                        }
                    }
                };
                let (a, b) = (back(a.min(n - 1)), back(b.min(n - 1)));
                let (ax, ay, bx, by) = if horizontal { (a, y, b, y) } else { (x, a, x, b) };
                Mix {
                    ax,
                    ay,
                    bx,
                    by,
                    wa: 1.0 - wb,
                    wb,
                    wd: 0.0,
                }
            }
        }
    }
}

/// The dip colour's code for a luma or chroma plane of sample type `T`.
fn dip_code<T: Sample>(kind: TransitionKind, luma: bool) -> f32 {
    match (kind, luma) {
        (TransitionKind::DipWhite, true) => (235 * (T::MAX + 1) / 256) as f32,
        (_, true) => T::BLACK.code() as f32,
        (_, false) => T::NEUTRAL.code() as f32,
    }
}

fn plane<T: Sample>(geo: &Geo, a: &[T], b: &[T], dip: f32) -> Vec<T> {
    assert_eq!(a.len(), geo.w * geo.h, "transition: plane size");
    assert_eq!(b.len(), a.len(), "transition: mismatched plane sizes");
    let mut out = Vec::with_capacity(a.len());
    for y in 0..geo.h {
        for x in 0..geo.w {
            let m = geo.mix(x, y);
            let v = m.wa * a[m.ay * geo.w + m.ax].code() as f32
                + m.wb * b[m.by * geo.w + m.bx].code() as f32
                + m.wd * dip;
            out.push(T::from_f32(v));
        }
    }
    out
}

/// `a` and `b` (two `width` x `height` I420 pictures at `T`'s depth) mixed
/// `t` of the way from A to B in the way `kind` draws it.
pub fn blend_planes<T: Sample>(
    kind: TransitionKind,
    a: (&[T], &[T], &[T]),
    b: (&[T], &[T], &[T]),
    t: f32,
    width: usize,
    height: usize,
) -> (Vec<T>, Vec<T>, Vec<T>) {
    if kind == TransitionKind::Dissolve {
        return crate::scale::blend_i420(a, b, t);
    }
    let luma = Geo::new(kind, t, width, height, 1);
    let chroma = Geo::new(kind, t, width, height, 2);
    (
        plane(&luma, a.0, b.0, dip_code::<T>(kind, true)),
        plane(&chroma, a.1, b.1, dip_code::<T>(kind, false)),
        plane(&chroma, a.2, b.2, dip_code::<T>(kind, false)),
    )
}

/// [`blend_planes`] for two BGRA pictures.
pub fn blend_bgra(kind: TransitionKind, a: &[u8], b: &[u8], t: f32, width: usize, height: usize) -> Vec<u8> {
    assert_eq!(a.len(), b.len(), "blend_bgra: mismatched frame sizes");
    assert_eq!(a.len(), width * height * 4, "blend_bgra: frame size");
    let geo = Geo::new(kind, t, width, height, 1);
    let dip = if kind == TransitionKind::DipWhite { 255.0 } else { 0.0 };
    let mut out = Vec::with_capacity(a.len());
    for y in 0..height {
        for x in 0..width {
            let m = geo.mix(x, y);
            let (ai, bi) = ((m.ay * width + m.ax) * 4, (m.by * width + m.bx) * 4);
            for c in 0..4 {
                // Alpha is 255 on both sides and in the dip.
                let d = if c == 3 { 255.0 } else { dip };
                let v = m.wa * f32::from(a[ai + c]) + m.wb * f32::from(b[bi + c]) + m.wd * d;
                out.push(v.round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 32;
    const H: usize = 16;

    /// A solid I420 picture of luma `y`.
    fn solid(y: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (cw, ch) = chroma_dims(W, H);
        (vec![y; W * H], vec![128; cw * ch], vec![128; cw * ch])
    }

    fn run(kind: TransitionKind, t: f32) -> Vec<u8> {
        let (a, b) = (solid(50), solid(200));
        blend_planes(kind, (&a.0, &a.1, &a.2), (&b.0, &b.1, &b.2), t, W, H).0
    }

    fn at(p: &[u8], x: usize, y: usize) -> u8 {
        p[y * W + x]
    }

    #[test]
    fn names_round_trip() {
        for shape in TransitionKind::SHAPES {
            let kinds: Vec<_> = match shape.dir() {
                Some(_) => Dir::ALL.iter().map(|&d| shape.with_dir(d)).collect(),
                None => vec![shape],
            };
            for k in kinds {
                assert_eq!(TransitionKind::parse(&k.name()), Some(k), "{}", k.name());
            }
        }
        assert_eq!(TransitionKind::parse("wipe"), None);
        assert_eq!(TransitionKind::parse("zoom-left"), None);
        assert_eq!(TransitionKind::parse("nonsense"), None);
    }

    #[test]
    fn timeline_labels_are_plain_words() {
        assert_eq!(TransitionKind::Dissolve.label(), "Dissolve");
        assert_eq!(TransitionKind::DipBlack.label(), "Dip to black");
        assert_eq!(TransitionKind::DipWhite.label(), "Dip to white");
        assert_eq!(TransitionKind::Wipe(Dir::Left).label(), "Wipe left");
        assert_eq!(TransitionKind::Slide(Dir::Up).label(), "Slide up");
        assert_eq!(TransitionKind::Push(Dir::Down).label(), "Push down");
        assert_eq!(TransitionKind::Zoom.label(), "Zoom");
    }

    #[test]
    fn dissolve_is_the_plain_lerp() {
        assert!(run(TransitionKind::Dissolve, 0.5).iter().all(|&v| v == 125));
    }

    #[test]
    fn wipe_left_shows_a_on_the_left_and_b_on_the_right_at_the_midpoint() {
        let p = run(TransitionKind::Wipe(Dir::Left), 0.5);
        assert_eq!(at(&p, 2, 3), 50);
        assert_eq!(at(&p, W - 3, 3), 200);
        let q = run(TransitionKind::Wipe(Dir::Right), 0.5);
        assert_eq!((at(&q, 2, 3), at(&q, W - 3, 3)), (200, 50));
        let u = run(TransitionKind::Wipe(Dir::Up), 0.5);
        assert_eq!((at(&u, 3, 1), at(&u, 3, H - 2)), (50, 200));
        let d = run(TransitionKind::Wipe(Dir::Down), 0.5);
        assert_eq!((at(&d, 3, 1), at(&d, 3, H - 2)), (200, 50));
    }

    #[test]
    fn a_wipe_has_a_soft_edge_between_the_two() {
        let p = run(TransitionKind::Wipe(Dir::Left), 0.5);
        let mid = at(&p, W / 2, 0);
        assert!(mid > 50 && mid < 200, "{mid}");
        let wide: Vec<u8> = (0..W).map(|x| at(&p, x, 0)).collect();
        assert!(wide.windows(2).all(|w| w[0] <= w[1]), "monotone: {wide:?}");
    }

    #[test]
    fn slide_left_brings_b_in_from_the_right_over_a() {
        let p = run(TransitionKind::Slide(Dir::Left), 0.5);
        assert_eq!((at(&p, 2, 3), at(&p, W - 3, 3)), (50, 200));
        let p = run(TransitionKind::Slide(Dir::Down), 0.5);
        assert_eq!((at(&p, 3, 1), at(&p, 3, H - 2)), (200, 50));
    }

    #[test]
    fn push_moves_a_out_with_b_behind_it() {
        // Texture A so the shift shows: a ramp across, B flat.
        let (cw, ch) = chroma_dims(W, H);
        let ramp: Vec<u8> = (0..W * H).map(|i| (i % W) as u8 * 4).collect();
        let a = (ramp.clone(), vec![128; cw * ch], vec![128; cw * ch]);
        let b = solid(200);
        let push = |d| {
            blend_planes(
                TransitionKind::Push(d),
                (&a.0, &a.1, &a.2),
                (&b.0, &b.1, &b.2),
                0.5,
                W,
                H,
            )
            .0
        };
        // A shifted left by 16: output x shows A's x + 16, then B.
        let p = push(Dir::Left);
        assert_eq!((at(&p, 0, 0), at(&p, 15, 0), at(&p, 16, 0)), (ramp[16], ramp[31], 200));
        let p = push(Dir::Right);
        assert_eq!((at(&p, 16, 0), at(&p, 31, 0), at(&p, 15, 0)), (ramp[0], ramp[15], 200));
    }

    #[test]
    fn dips_go_through_their_colour_at_the_midpoint_at_both_depths() {
        assert!(run(TransitionKind::DipBlack, 0.5).iter().all(|&v| v == 16));
        assert!(run(TransitionKind::DipWhite, 0.5).iter().all(|&v| v == 235));
        let (cw, ch) = chroma_dims(W, H);
        let a = (vec![300u16; W * H], vec![512u16; cw * ch], vec![512u16; cw * ch]);
        let w = blend_planes(TransitionKind::DipWhite, (&a.0, &a.1, &a.2), (&a.0, &a.1, &a.2), 0.5, W, H);
        assert!(w.0.iter().all(|&v| v == 940));
        assert!(w.1.iter().all(|&v| v == 512));
    }

    #[test]
    fn zoom_is_a_fade_with_a_scale() {
        let p = run(TransitionKind::Zoom, 0.5);
        assert!(p.iter().all(|&v| v == 125), "solids stay a plain mix");
    }

    #[test]
    fn bgra_matches_the_planes_geometry() {
        let (a, b) = (vec![10u8; W * H * 4], vec![250u8; W * H * 4]);
        let p = blend_bgra(TransitionKind::Wipe(Dir::Left), &a, &b, 0.5, W, H);
        assert_eq!(p[(3 * W + 2) * 4], 10);
        assert_eq!(p[(3 * W + W - 3) * 4], 250);
        let d = blend_bgra(TransitionKind::DipBlack, &a, &b, 0.5, W, H);
        assert_eq!(&d[..4], &[0, 0, 0, 255]);
    }
}
