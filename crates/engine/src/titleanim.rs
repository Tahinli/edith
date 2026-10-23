//! A title's entrance and exit animations: which one it plays, and how far along it is at a
//! frame. The export ([`crate::export`]) and the preview both read the same [`Motion`] from
//! [`crate::project::Title::motion_at`], so the two agree on when a word is up, where it sits and
//! how big it is. An exit is its entrance played backwards: the same effect runs from its
//! resting state down to nothing, with the easing reversed (ease-out in, ease-in out).

use crate::project::Title;

/// One of the animations a title plays on the way in or out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TitleAnim {
    /// The title is simply there, and simply gone.
    #[default]
    None,
    /// The title fades in or out.
    Fade,
    /// The letters appear one by one (and go one by one on the way out).
    Typewriter,
    /// The title rises into place from below.
    SlideUp,
    /// The title moves in from the right.
    SlideLeft,
    /// The title grows from 60% to full size with a slight overshoot.
    Pop,
    /// The words appear one by one.
    WordByWord,
}

impl TitleAnim {
    pub const ALL: [TitleAnim; 7] = [
        Self::None,
        Self::Fade,
        Self::Typewriter,
        Self::SlideUp,
        Self::SlideLeft,
        Self::Pop,
        Self::WordByWord,
    ];

    /// The word the file, the MCP and the GUI spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Fade => "fade",
            Self::Typewriter => "typewriter",
            Self::SlideUp => "slide_up",
            Self::SlideLeft => "slide_left",
            Self::Pop => "pop",
            Self::WordByWord => "word_by_word",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// The length an animation gets when none is given, in seconds.
pub const DEFAULT_SECS: f64 = 0.5;

/// How far a slide travels, as a share of the picture's height (up) or width (left).
pub const SLIDE: f32 = 0.08;

/// The size a pop starts at, as a share of the title's full size.
pub const POP_FROM: f32 = 0.6;

/// Which unit a reveal counts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Letters, not counting spaces and line breaks.
    Letters,
    /// Words, as a run of non-space characters.
    Words,
}

/// What an animation does to a title at one frame. Every field is at rest when the title is in
/// place: full alpha, no offset, full size, all of the text.
#[derive(Clone, Debug, PartialEq)]
pub struct Motion {
    /// The opacity, 0 to 255, with the title's fades already in.
    pub alpha: u8,
    /// Shift to the right, as a share of the picture's width.
    pub dx: f32,
    /// Shift down, as a share of the picture's height.
    pub dy: f32,
    /// The size as a share of the title's full size (1 at rest).
    pub scale: f32,
    /// The part of the text that shows, as `(unit, share)`, or `None` for all of it.
    pub reveal: Option<(Unit, f32)>,
}

/// The part of a title's text a reveal lets through at one frame: its first `chars` characters
/// are drawn and the rest are not. The text is always laid out whole, so the drawn part stays
/// where it is and nothing moves while it types on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    /// The characters drawn, counted from the start of the text (spaces included).
    pub chars: usize,
    /// The letters among them: non-whitespace characters, the count a wrapped layout can use.
    pub ink: usize,
}

impl Motion {
    /// The part of `text` drawn at this frame, or `None` when all of it is (no reveal, or one
    /// that has finished). A letter reveal shows letters one by one; a word reveal shows whole
    /// words, the next one from its first letter.
    pub fn revealed(&self, text: &str) -> Option<Shown> {
        let (unit, share) = self.reveal?;
        let share = share.clamp(0.0, 1.0);
        let units = match unit {
            Unit::Letters => text.chars().filter(|c| !c.is_whitespace()).count(),
            Unit::Words => word_starts(text).count(),
        };
        // A share above nothing shows at least one unit, so the first frame of a reveal has a letter.
        let show = ((share * units as f32).ceil() as usize).min(units);
        if show == units {
            return None;
        }
        let total = text.chars().count();
        let stop = match unit {
            Unit::Letters => text
                .chars()
                .enumerate()
                .filter(|(_, c)| !c.is_whitespace())
                .nth(show)
                .map_or(total, |(i, _)| i),
            Unit::Words => word_starts(text).nth(show).unwrap_or(total),
        };
        let ink = text.chars().take(stop).filter(|c| !c.is_whitespace()).count();
        Some(Shown { chars: stop, ink })
    }
}

/// The character index where each word of `text` starts.
fn word_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    let mut prev_space = true;
    text.chars().enumerate().filter_map(move |(i, c)| {
        let start = prev_space && !c.is_whitespace();
        prev_space = c.is_whitespace();
        start.then_some(i)
    })
}

/// Eased out: quick at first, settling into place.
fn ease_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    1.0 - (1.0 - x).powi(3)
}

/// Eased in: slow at first, quick at the end.
fn ease_in(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * x
}

/// The overshoot curve a pop grows along: it passes 1 a little before it settles there.
fn overshoot(p: f32) -> f32 {
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.0;
    let q = p - 1.0;
    1.0 + C3 * q * q * q + C1 * q * q
}

/// One animation's effect at presence `p` (0 gone, 1 in place), before it is combined with the
/// other end's. The same function serves an entrance (presence climbs) and an exit (it falls).
struct Part {
    alpha: f32,
    dx: f32,
    dy: f32,
    scale: f32,
    reveal: Option<(Unit, f32)>,
}

impl Part {
    const REST: Part = Part { alpha: 1.0, dx: 0.0, dy: 0.0, scale: 1.0, reveal: None };
}

fn effect(anim: TitleAnim, p: f32) -> Part {
    let rest = Part::REST;
    match anim {
        TitleAnim::None => rest,
        TitleAnim::Fade => Part { alpha: p, ..rest },
        TitleAnim::Typewriter => Part { reveal: Some((Unit::Letters, p)), ..rest },
        TitleAnim::WordByWord => Part { reveal: Some((Unit::Words, p)), ..rest },
        TitleAnim::SlideUp => Part { dy: (1.0 - p) * SLIDE, ..rest },
        TitleAnim::SlideLeft => Part { dx: (1.0 - p) * SLIDE, ..rest },
        TitleAnim::Pop => Part { scale: POP_FROM + (1.0 - POP_FROM) * overshoot(p), ..rest },
    }
}

impl Title {
    /// The entrance the title plays, and its length in frames. A title with a fade-in and no
    /// entrance animation reads as a fade of that length, which is what it always was.
    pub fn entrance(&self) -> (TitleAnim, u32) {
        match self.in_anim {
            TitleAnim::None if self.fade_in > 0 => (TitleAnim::Fade, self.fade_in),
            anim => (anim, self.anim_frames),
        }
    }

    /// The exit the title plays, and its length in frames, read as [`Title::entrance`] reads it.
    pub fn exit(&self) -> (TitleAnim, u32) {
        match self.out_anim {
            TitleAnim::None if self.fade_out > 0 => (TitleAnim::Fade, self.fade_out),
            anim => (anim, self.anim_frames),
        }
    }

    /// Moves a plain fade into the entrance and exit it is: a title that has one reads and edits as
    /// the animations it plays. Its length is the longer of the two fades, since they share one.
    pub fn adopt_fades(&mut self) {
        if self.in_anim == TitleAnim::None && self.fade_in > 0 {
            self.in_anim = TitleAnim::Fade;
        }
        if self.out_anim == TitleAnim::None && self.fade_out > 0 {
            self.out_anim = TitleAnim::Fade;
        }
        if (self.in_anim != TitleAnim::None && self.fade_in > 0) || (self.out_anim != TitleAnim::None && self.fade_out > 0) {
            self.anim_frames = self.fade_in.max(self.fade_out);
            self.fade_in = 0;
            self.fade_out = 0;
        }
    }

    /// What the title looks like at timeline frame `frame`, or `None` outside its span.
    pub fn motion_at(&self, frame: u32) -> Option<Motion> {
        if frame < self.start || frame >= self.end() {
            return None;
        }
        let k = frame - self.start;
        let left = self.frames - k;
        // The fades the title always had, linear and counted as they always were.
        let mut alpha = 1.0f32;
        if self.fade_in > 0 && k < self.fade_in {
            alpha = alpha.min((k + 1) as f32 / (self.fade_in + 1) as f32);
        }
        if self.fade_out > 0 && left <= self.fade_out {
            alpha = alpha.min(left as f32 / (self.fade_out + 1) as f32);
        }
        // The fades above are the old linear ones; the animations are the new ones, read as set.
        let (in_anim, in_frames) = (self.in_anim, self.anim_frames);
        let (out_anim, out_frames) = (self.out_anim, self.anim_frames);
        let presence_in = match in_anim {
            TitleAnim::None => 1.0,
            _ if k < in_frames => ease_out((k + 1) as f32 / in_frames as f32),
            _ => 1.0,
        };
        let presence_out = match out_anim {
            TitleAnim::None => 1.0,
            _ if left <= out_frames => ease_in(left as f32 / out_frames as f32),
            _ => 1.0,
        };
        let (a, b) = (effect(in_anim, presence_in), effect(out_anim, presence_out));
        let reveal = match (a.reveal, b.reveal) {
            (Some(x), Some(y)) => Some(if x.1 <= y.1 { x } else { y }),
            (x, y) => x.or(y),
        };
        Some(Motion {
            alpha: (alpha * a.alpha * b.alpha * 255.0) as u8,
            dx: a.dx + b.dx,
            dy: a.dy + b.dy,
            scale: a.scale * b.scale,
            reveal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Title;
    use crate::text::TextStyle;

    fn title(anim: TitleAnim, frames: u32) -> Title {
        Title {
            start: 0,
            frames,
            text: "Hello big world".into(),
            style: TextStyle::default(),
            fade_in: 0,
            fade_out: 0,
            in_anim: anim,
            out_anim: TitleAnim::None,
            anim_frames: 12,
        }
    }

    #[test]
    fn a_typewriter_title_shows_more_letters_from_its_start_to_its_middle_to_its_end() {
        // A 26-frame typewriter in a 60-frame title: the letters come up over its first 26 frames.
        let t = Title { anim_frames: 26, ..title(TitleAnim::Typewriter, 60) };
        let letters = |f: u32| {
            let m = t.motion_at(f).expect("up");
            m.revealed(&t.text).map_or(13, |s| s.ink)
        };
        let (start, middle, end) = (letters(0), letters(6), letters(25));
        assert!(start >= 1 && start < middle && middle < end, "{start} {middle} {end}");
        assert_eq!(end, 13, "all 13 letters are up at the end of the animation");
        // The typed part is drawn exactly where the whole line is drawn: nothing moves while it types on.
        let style = TextStyle::default();
        let lines = vec![t.text.clone()];
        let full = crate::text::overlay(&style, 640, 360, &lines);
        let part = crate::text::overlay_revealed(&style, 640, 360, &lines, 5);
        let (f, p) = (full.fill(), part.fill());
        assert!(p.iter().zip(f).all(|(&p, &f)| p == 0 || p == f), "the typed part sits where the whole line does");
        let drawn = |v: &[u8]| v.iter().filter(|&&x| x > 0).count();
        assert!(0 < drawn(p) && drawn(p) < drawn(f), "{} {}", drawn(p), drawn(f));
    }
}
