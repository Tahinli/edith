//! Animated caption styles: karaoke highlight, word-by-word pop, bounce and one word at a time.
//!
//! A burned caption is a [`Cue`] drawn by [`crate::text::overlay_blocks`]. With the default
//! [`CaptionStyle`] (animation none, no re-chunking, no uppercase) it is the static cue it always
//! was, drawn by exactly the path it always took. Otherwise [`plan`] turns the cues on screen at
//! one timeline frame into [`Block`]s whose words carry an effect: which are visible, which one is
//! being spoken, how big and how high each is. The export and the still frame both call [`plan`]
//! with the same arguments and [`crate::text::overlay_blocks`] on the result, so a frame looks the
//! same in either.
//!
//! **Word times.** A cue made by `auto_captions` carries the transcript's own word times
//! ([`crate::subtitle::Cue::words`], saved in the project). [`cue_words`] is the one door every
//! effect reads them through. A cue without them (an imported SRT or VTT, or one whose words were
//! retyped, which [`crate::subtitle::Cue::real_words`] notices) gets the documented fallback: the
//! words spread over the cue *by their letter counts* (a long word is spoken for longer than a
//! short one). A cue is never drawn static for lack of times. Cue times are the timeline's already
//! ([`crate::export::burn_cues`] maps a cue through the clip's speed and ramp), so a retimed clip
//! animates at its own pace with no further mapping here.

use std::hash::{Hash, Hasher};

use ec_subs::KaraokeMode;

use crate::subtitle::{Cue, Syllable};
use crate::text::{Block, Syl, TextStyle, WordFx};

/// How the words of a caption move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptionAnim {
    /// The static cue.
    #[default]
    None,
    /// The whole chunk is shown and the word being spoken is highlighted.
    Karaoke,
    /// Words appear as they are spoken, each popping from 125% down to 100%.
    Pop,
    /// The whole chunk is shown and the word being spoken is lifted and enlarged, easing out.
    Bounce,
    /// One big word at a time.
    Word,
}

impl CaptionAnim {
    pub const ALL: [CaptionAnim; 5] = [Self::None, Self::Karaoke, Self::Pop, Self::Bounce, Self::Word];

    /// The word the file, the MCP and the GUI spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Karaoke => "karaoke",
            Self::Pop => "pop",
            Self::Bounce => "bounce",
            Self::Word => "word",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// The highest `words_per_line`: a caption of more is a paragraph, not a chunk.
pub const MAX_WORDS_PER_LINE: u8 = 12;

/// The yellow a highlight is when nobody picked one.
pub const DEFAULT_HIGHLIGHT: [u8; 3] = [255, 214, 0];

/// A pop starts this big and settles to 1.0 over [`POP_US`].
pub const POP_FROM: f32 = 1.25;
pub const POP_US: i64 = 120_000;

/// A pause between two words longer than this ends a chunk, whatever its length.
const PAUSE_US: i64 = 700_000;

/// How a burned caption is animated and chunked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptionStyle {
    pub anim: CaptionAnim,
    /// The colour of the highlighted word (or of its box).
    pub highlight: [u8; 3],
    /// The highlight is a rounded box of the colour behind white text, not coloured text.
    pub hl_box: bool,
    pub uppercase: bool,
    /// At most this many words on screen at once, the cue re-chunked by word time; 0 is the
    /// cue as it is. The one-word animation is always one.
    pub words_per_line: u8,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        Self {
            anim: CaptionAnim::None,
            highlight: DEFAULT_HIGHLIGHT,
            hl_box: false,
            uppercase: false,
            words_per_line: 0,
        }
    }
}

impl CaptionStyle {
    /// Draws the cue as it always was: the default path, not a special case of this one.
    pub fn is_plain(&self) -> bool {
        self.anim == CaptionAnim::None && !self.uppercase && self.words_per_line == 0
    }

    pub fn clamped(&self) -> Self {
        Self { words_per_line: self.words_per_line.min(MAX_WORDS_PER_LINE), ..*self }
    }
}

/// One word of a cue and when it is spoken, microseconds on the same clock as the cue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimedWord {
    pub start_us: i64,
    pub end_us: i64,
    pub text: String,
}

/// The words of `cue` with their times: the cue's own ([`Cue::real_words`]) when it has them,
/// else spread over the cue by letter count (see the module docs).
pub fn cue_words(cue: &Cue) -> Vec<TimedWord> {
    // The transcript's own times, when they still describe the text.
    if let Some(real) = cue.real_words() {
        return real
            .iter()
            .map(|(s, e, w)| TimedWord { start_us: *s, end_us: (*e).max(*s), text: w.clone() })
            .collect();
    }
    let words: Vec<&str> = cue.text.split_whitespace().collect();
    let total: usize = words.iter().map(|w| w.chars().count().max(1)).sum();
    let span = (cue.end_us - cue.start_us).max(0);
    let mut done = 0usize;
    words
        .iter()
        .map(|w| {
            let from = cue.start_us + span * done as i64 / total.max(1) as i64;
            done += w.chars().count().max(1);
            let to = cue.start_us + span * done as i64 / total.max(1) as i64;
            TimedWord { start_us: from, end_us: to, text: (*w).to_string() }
        })
        .collect()
}

/// `words` cut into runs of at most `per` (0 is one run), starting a new run after a long pause.
/// Words stay in order and keep their own times.
pub fn chunk_words(words: &[TimedWord], per: usize) -> Vec<Vec<TimedWord>> {
    let mut out: Vec<Vec<TimedWord>> = Vec::new();
    for w in words {
        let start_new = match out.last() {
            None => true,
            Some(run) => {
                (per > 0 && run.len() >= per)
                    || run.last().is_some_and(|l| w.start_us - l.end_us > PAUSE_US)
            }
        };
        if start_new {
            out.push(Vec::new());
        }
        if let Some(run) = out.last_mut() {
            run.push(w.clone());
        }
    }
    out
}

fn ease_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    1.0 - (1.0 - x).powi(3)
}

/// The size of a word `since_us` after it began: [`POP_FROM`] at the start, 1.0 from [`POP_US`] on.
pub fn pop_scale(since_us: i64) -> f32 {
    POP_FROM + (1.0 - POP_FROM) * ease_out(since_us as f32 / POP_US as f32)
}

/// The size and lift (in ems) of the bounced word `since_us` after it began: it is a little
/// larger while it is spoken, and starts raised and eases down.
pub fn bounce_fx(since_us: i64) -> (f32, f32) {
    const LIFT_US: f32 = 200_000.0;
    (1.12, 0.2 * (1.0 - ease_out(since_us as f32 / LIFT_US)))
}

/// What to draw for the cues `on` (indices into `cues`, as [`crate::export::cues_on_screen`]
/// gives them) at timeline frame `frame`. One block per cue that shows anything.
pub fn plan(style: &TextStyle, cues: &[Cue], on: &[usize], frame: u32, fps: f64) -> Vec<Block> {
    let cs = style.caption.clamped();
    let t = (f64::from(frame) / fps * 1e6).round() as i64;
    on.iter()
        .filter_map(|&i| {
            let cue = &cues[i];
            if cs.is_plain() {
                let lines = cue.text.split('\n').map(str::to_string).collect();
                let kara = cue.real_karaoke().map(|k| karaoke_at(k, t)).unwrap_or_default();
                return Some(Block { lines, fx: Vec::new(), kara });
            }
            block_at(&cs, cue, t)
        })
        .collect()
}

/// The look of each karaoke syllable of an ASS cue at time `t`: a fill beat switches from its
/// unsung to its sung colour at its start, a sweep crosses its width by the time it lasts, and
/// an outline beat keeps its letters but draws their outline from its start.
fn karaoke_at(syllables: &[Syllable], t: i64) -> Vec<Syl> {
    syllables
        .iter()
        .map(|s| {
            let (from, len) = (s.from, s.text.chars().count());
            let begun = t >= s.start_us;
            let (left, right, split, bare) = match s.mode {
                KaraokeMode::Fill => (s.sung, Some(s.unsung), if begun { 1.0 } else { 0.0 }, false),
                KaraokeMode::Sweep => {
                    let span = (s.end_us - s.start_us).max(1) as f32;
                    (s.sung, Some(s.unsung), ((t - s.start_us) as f32 / span).clamp(0.0, 1.0), false)
                }
                KaraokeMode::Outline => (s.sung, s.sung, 1.0, !begun),
            };
            Syl { from, len, left, right, split, bare }
        })
        .collect()
}

/// The block of one animated or re-chunked cue at time `t`, or `None` when nothing of it shows yet.
fn block_at(cs: &CaptionStyle, cue: &Cue, t: i64) -> Option<Block> {
    let words = cue_words(cue);
    if words.is_empty() {
        return None;
    }
    let per = if cs.anim == CaptionAnim::Word { 1 } else { usize::from(cs.words_per_line) };
    let chunks = chunk_words(&words, per);
    // The chunk on screen: the last one begun, the first before any has.
    let run = chunks.iter().rev().find(|c| c[0].start_us <= t).unwrap_or(&chunks[0]);
    // The word whose turn it is stays active through a pause after it. A word squeezed to nothing
    // (cut away by a trim) is never the one.
    let active = run
        .iter()
        .rposition(|w| w.start_us <= t && w.end_us > w.start_us)
        .or_else(|| run.iter().rposition(|w| w.start_us <= t));
    let case = |s: &str| if cs.uppercase { s.to_uppercase() } else { s.to_string() };
    let lines: Vec<String> = if per == 0 {
        // The cue's own line breaks stay.
        cue.text.split('\n').map(case).collect()
    } else {
        vec![run.iter().map(|w| case(&w.text)).collect::<Vec<_>>().join(" ")]
    };
    let highlight = Some((cs.highlight, cs.hl_box));
    let fx: Vec<WordFx> = run
        .iter()
        .enumerate()
        .map(|(k, w)| {
            let is_active = active == Some(k);
            let since = t - w.start_us;
            match cs.anim {
                CaptionAnim::None => WordFx::default(),
                CaptionAnim::Karaoke => WordFx { highlight: is_active.then_some(()).and(highlight), ..WordFx::default() },
                CaptionAnim::Pop | CaptionAnim::Word => WordFx {
                    hidden: since < 0,
                    scale: pop_scale(since),
                    highlight: is_active.then_some(()).and(highlight),
                    ..WordFx::default()
                },
                CaptionAnim::Bounce => {
                    if is_active {
                        let (scale, lift) = bounce_fx(since);
                        WordFx { scale, lift, highlight, ..WordFx::default() }
                    } else {
                        WordFx::default()
                    }
                }
            }
        })
        .collect();
    // Before the first word is spoken a pop has nothing to show.
    if fx.iter().all(|f| f.hidden) {
        return None;
    }
    Some(Block { lines, fx, kara: Vec::new() })
}

/// A number that changes whenever `blocks` would draw differently: what a cache of the overlay
/// of one cue set is keyed on.
pub fn blocks_key(blocks: &[Block]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for b in blocks {
        b.lines.hash(&mut h);
        for f in &b.fx {
            (f.hidden, (f.scale * 1000.0).round() as i32, (f.lift * 1000.0).round() as i32, f.highlight).hash(&mut h);
        }
        for k in &b.kara {
            (k.from, k.len, k.left, k.right, (k.split * 1000.0).round() as i32, k.bare).hash(&mut h);
        }
        0xB10Cu16.hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{overlay_blocks, overlay_stack};

    const W: u32 = 640;
    const H: u32 = 360;
    const FPS: f64 = 25.0;

    fn cue(text: &str, start_s: f64, end_s: f64) -> Cue {
        Cue { start_us: (start_s * 1e6) as i64, end_us: (end_s * 1e6) as i64, text: text.into(), image: None, words: Vec::new(), karaoke: Vec::new() }
    }

    fn style(anim: CaptionAnim) -> TextStyle {
        TextStyle {
            boxed: false,
            caption: CaptionStyle { anim, ..CaptionStyle::default() },
            ..TextStyle::default()
        }
    }

    /// The frame drawn on black: `(x, is_highlight)` of every strongly covered pixel.
    fn inked(style: &TextStyle, cues: &[Cue], frame: u32) -> Vec<(u32, bool)> {
        let blocks = plan(style, cues, &[0], frame, FPS);
        let mut canvas = vec![0u8; (W * H * 4) as usize];
        overlay_blocks(style, W, H, &blocks).blend_bgra(&mut canvas);
        let mut out = Vec::new();
        for (i, px) in canvas.chunks(4).enumerate() {
            let (b, g, r) = (px[0], px[1], px[2]);
            let yellow = r > 200 && g > 150 && b < 80;
            let white = r > 200 && g > 200 && b > 200;
            if yellow || white {
                out.push((i as u32 % W, yellow));
            }
        }
        out
    }

    #[test]
    fn karaoke_colours_the_word_being_spoken_and_no_other() {
        // alpha beta gamma over 3 s, 5/4/5 letters: beta is spoken from 1.07 s to 1.93 s.
        let cues = [cue("alpha beta gamma", 0.0, 3.0)];
        let px = inked(&style(CaptionAnim::Karaoke), &cues, 40);
        let yellow: Vec<u32> = px.iter().filter(|p| p.1).map(|p| p.0).collect();
        let white: Vec<u32> = px.iter().filter(|p| !p.1).map(|p| p.0).collect();
        assert!(yellow.len() > 100, "the second word has highlight pixels: {}", yellow.len());
        let (ymin, ymax) = (*yellow.iter().min().unwrap(), *yellow.iter().max().unwrap());
        assert!(white.iter().any(|&x| x < ymin), "the first word is white, left of the highlight");
        assert!(white.iter().any(|&x| x > ymax), "the third word is white, right of the highlight");
        assert!(!white.iter().any(|&x| x > ymin && x < ymax), "no white ink inside the highlighted word");
        // At the start it is the first word that carries the colour, and it is further left.
        let first = inked(&style(CaptionAnim::Karaoke), &cues, 2);
        let first_max = first.iter().filter(|p| p.1).map(|p| p.0).max().unwrap();
        assert!(first_max < ymin, "the highlight moved right from word 1 to word 2");
    }

    #[test]
    fn a_highlight_box_puts_the_colour_behind_white_text() {
        let mut s = style(CaptionAnim::Karaoke);
        s.caption.hl_box = true;
        let cues = [cue("alpha beta gamma", 0.0, 3.0)];
        let px = inked(&s, &cues, 40);
        let yellow = px.iter().filter(|p| p.1).count();
        let white = px.iter().filter(|p| !p.1).count();
        assert!(yellow > 300, "a box is a lot of colour: {yellow}");
        assert!(white > 100, "the words are white");
    }

    #[test]
    fn a_pop_is_biggest_when_the_word_begins_and_settled_after_200_ms() {
        assert_eq!(pop_scale(0), POP_FROM);
        assert!(pop_scale(0) > pop_scale(200_000));
        assert_eq!(pop_scale(200_000), 1.0);
        assert!(pop_scale(60_000) < pop_scale(0) && pop_scale(60_000) > 1.0);
        // In a plan: words not yet spoken are hidden, the newest is mid-pop.
        let cues = [cue("alpha beta gamma", 0.0, 3.0)];
        let blocks = plan(&style(CaptionAnim::Pop), &cues, &[0], 27, FPS); // 1.08 s: 10 ms into beta
        let fx = &blocks[0].fx;
        assert_eq!(fx.len(), 3);
        assert!(!fx[0].hidden && fx[0].scale == 1.0);
        assert!(!fx[1].hidden && fx[1].scale > 1.2);
        assert!(fx[2].hidden, "gamma is not spoken yet");
    }

    #[test]
    fn bounce_lifts_the_spoken_word_and_eases_down() {
        let (s0, l0) = bounce_fx(0);
        let (_, l1) = bounce_fx(100_000);
        let (_, l2) = bounce_fx(400_000);
        assert!(s0 > 1.0 && l0 > l1 && l1 > l2 && l2 == 0.0);
    }

    #[test]
    fn one_word_mode_draws_only_the_word_being_spoken() {
        let cues = [cue("alpha beta gamma", 0.0, 3.0)];
        let s = style(CaptionAnim::Word);
        let blocks = plan(&s, &cues, &[0], 40, FPS);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines, ["beta"]);
        assert_eq!(blocks[0].fx.len(), 1);
        // On screen it is one word: the ink spans less than a three-word line would.
        let px = inked(&s, &cues, 40);
        let (min, max) = (px.iter().map(|p| p.0).min().unwrap(), px.iter().map(|p| p.0).max().unwrap());
        let all = inked(&style(CaptionAnim::Karaoke), &cues, 40);
        let (amin, amax) = (all.iter().map(|p| p.0).min().unwrap(), all.iter().map(|p| p.0).max().unwrap());
        assert!(max - min < (amax - amin) / 2 + 20, "one word {min}..{max} vs the line {amin}..{amax}");
    }

    #[test]
    fn chunks_follow_the_word_times() {
        let w = |s: f64, e: f64, t: &str| TimedWord { start_us: (s * 1e6) as i64, end_us: (e * 1e6) as i64, text: t.into() };
        let words = [
            w(0.0, 0.3, "one"), w(0.3, 0.6, "two"), w(0.6, 0.9, "three"), w(0.9, 1.2, "four"),
            w(3.0, 3.3, "five"), w(3.3, 3.6, "six"),
        ];
        let texts = |c: Vec<Vec<TimedWord>>| -> Vec<String> {
            c.iter().map(|r| r.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).collect()
        };
        assert_eq!(texts(chunk_words(&words, 3)), ["one two three", "four", "five six"], "a pause ends a chunk early");
        assert_eq!(texts(chunk_words(&words, 0)), ["one two three four", "five six"]);
        assert_eq!(texts(chunk_words(&words, 1)).len(), 6);
        let chunks = chunk_words(&words, 3);
        assert_eq!(chunks[2][0].start_us, 3_000_000, "words keep their own times");
        // The plan shows the chunk whose words are being spoken, with its own words only.
        let cues = [cue("one two three four five six", 0.0, 3.6)];
        let mut s = style(CaptionAnim::Karaoke);
        s.caption.words_per_line = 2;
        let at = |f: u32| plan(&s, &cues, &[0], f, FPS)[0].lines.clone();
        assert_eq!(at(0), ["one two"]);
        assert_eq!(at(60), ["three four"]); // 2.4 s; letter-weighted: "three" begins at 1.1 s
    }

    #[test]
    fn a_cue_without_word_times_spreads_its_words_by_letters() {
        let words = cue_words(&cue("a bbb cc", 1.0, 2.2));
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].start_us, 1_000_000);
        assert_eq!(words.last().unwrap().end_us, 2_200_000);
        assert!(words.windows(2).all(|p| p[0].end_us == p[1].start_us), "contiguous");
        let span = |k: usize| words[k].end_us - words[k].start_us;
        assert!(span(1) > span(2) && span(2) > span(0), "longer words are spoken for longer");
        assert!(cue_words(&cue("  ", 0.0, 1.0)).is_empty());
        // A zero-length cue still has its words, all at one instant.
        assert_eq!(cue_words(&cue("x y", 1.0, 1.0)).len(), 2);
    }

    #[test]
    fn karaoke_follows_real_word_times_that_are_uneven() {
        // One long word, then short ones: letter-weighted spacing would put "extraordinarily"
        // (15 letters of 21) in the first 2.14 s of 3 s; the real times give it 0.2 s to 2.5 s.
        let mut c = cue("extraordinarily big cat", 0.0, 3.0);
        c.words = vec![
            (200_000, 2_500_000, "extraordinarily".into()),
            (2_500_000, 2_800_000, "big".into()),
            (2_800_000, 3_000_000, "cat".into()),
        ];
        assert!(c.real_words().is_some());
        let s = style(CaptionAnim::Karaoke);
        let active = |frame: u32| {
            let fx = &plan(&s, &[c.clone()], &[0], frame, FPS)[0].fx;
            fx.iter().position(|f| f.highlight.is_some())
        };
        assert_eq!(active(58), Some(0), "2.32 s: still the long word (letter spacing would say big)");
        assert_eq!(active(64), Some(1), "2.56 s: big");
        assert_eq!(active(71), Some(2), "2.84 s: cat");
        // Retyped words drop the times: the letter-weighted fallback answers instead.
        let mut retyped = c.clone();
        retyped.text = "extraordinarily small cat".into();
        assert!(retyped.real_words().is_none());
        // A capital added after timing is still the same words.
        retyped.text = "Extraordinarily big cat".into();
        assert!(retyped.real_words().is_some());
    }

    #[test]
    fn real_words_are_moved_and_clipped_with_their_cue() {
        use crate::subtitle::map_words;
        let w = vec![(1_000_000, 2_000_000, "a".to_string()), (2_000_000, 3_000_000, "b".to_string())];
        // Shifted: the cue [1 s, 3 s] now at [11 s, 13 s].
        let m = map_words(&w, (1_000_000, 3_000_000), (11_000_000, 13_000_000));
        assert_eq!((m[0].0, m[1].1), (11_000_000, 13_000_000));
        // Sped up 2x: [1 s, 3 s] now takes 1 s.
        let m = map_words(&w, (1_000_000, 3_000_000), (0, 1_000_000));
        assert_eq!((m[0].1, m[1].1), (500_000, 1_000_000));
        // Trimmed at the front: the window starts at 2 s, so word "a" is held at the edge, and
        // still counts as a word of the text.
        let m = map_words(&w, (2_000_000, 3_000_000), (0, 1_000_000));
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].0, m[0].1), (0, 0));
        assert_eq!((m[1].0, m[1].1), (0, 1_000_000));
    }

    #[test]
    fn a_cue_cut_by_a_range_end_drops_the_words_past_it_and_clamps_the_last() {
        use crate::subtitle::map_words;
        let w = vec![
            (0, 1_000_000, "a".to_string()),
            (1_000_000, 3_000_000, "b".to_string()),
            (3_000_000, 4_000_000, "c".to_string()),
        ];
        // The cue [0, 4 s] is cut at 2 s: "c" starts past the cut and goes, "b" ends at the cut.
        let m = map_words(&w, (0, 2_000_000), (5_000_000, 7_000_000));
        assert_eq!(m.iter().map(|x| x.2.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!((m[1].0, m[1].1), (6_000_000, 7_000_000), "the last word ends at the cue's end");
    }

    #[test]
    fn the_plain_style_never_reads_word_times() {
        let plain = TextStyle::default();
        let bare = cue("alpha beta gamma", 0.0, 3.0);
        let mut timed = bare.clone();
        timed.words = vec![
            (0, 200_000, "alpha".into()),
            (200_000, 2_500_000, "beta".into()),
            (2_500_000, 3_000_000, "gamma".into()),
        ];
        for frame in [1, 20, 60] {
            let a = plan(&plain, std::slice::from_ref(&bare), &[0], frame, FPS);
            let b = plan(&plain, std::slice::from_ref(&timed), &[0], frame, FPS);
            assert_eq!(a, b, "same blocks at frame {frame}");
            assert_eq!(overlay_blocks(&plain, W, H, &a), overlay_blocks(&plain, W, H, &b));
        }
        // The burn path keys a plain style by 0 without planning at all (`Burn::paint`).
        assert!(plain.caption.is_plain());
    }

    #[test]
    fn the_default_style_draws_two_multi_line_cues_exactly_as_the_old_path_did() {
        let cues = [cue("alpha beta\ngamma delta", 0.0, 3.0), cue("one\ntwo three", 0.0, 3.0)];
        let s = TextStyle::default();
        let old: Vec<Vec<String>> =
            [0usize, 1].iter().map(|&i| cues[i].text.split('\n').map(str::to_string).collect()).collect();
        let blocks = plan(&s, &cues, &[0, 1], 10, FPS);
        assert_eq!(overlay_blocks(&s, W, H, &blocks), overlay_stack(&s, W, H, &old));
    }

    #[test]
    fn the_default_style_draws_the_static_cue_exactly() {
        let cues = [cue("alpha beta\ngamma", 0.0, 3.0)];
        let s = TextStyle::default();
        assert!(s.caption.is_plain());
        let blocks = plan(&s, &cues, &[0], 10, FPS);
        let lines = vec![vec!["alpha beta".to_string(), "gamma".to_string()]];
        assert_eq!(overlay_blocks(&s, W, H, &blocks), overlay_stack(&s, W, H, &lines));
    }

    #[test]
    fn uppercase_applies_to_a_static_cue_and_keeps_its_lines() {
        let cues = [cue("alpha beta\ngamma", 0.0, 3.0)];
        let mut s = style(CaptionAnim::None);
        s.caption.uppercase = true;
        let blocks = plan(&s, &cues, &[0], 10, FPS);
        assert_eq!(blocks[0].lines, ["ALPHA BETA", "GAMMA"]);
    }

    #[test]
    fn a_pop_with_nothing_spoken_yet_draws_nothing() {
        let cues = [cue("alpha", 2.0, 3.0)];
        // Before the cue's first word is begun (a cue is only on screen from its start, but the
        // plan is also asked at the instant before the first word).
        assert!(plan(&style(CaptionAnim::Pop), &cues, &[0], 10, FPS).is_empty());
    }

    #[test]
    fn the_key_changes_when_the_picture_would() {
        let cues = [cue("alpha beta gamma", 0.0, 3.0)];
        let s = style(CaptionAnim::Karaoke);
        let key = |f| blocks_key(&plan(&s, &cues, &[0], f, FPS));
        assert_eq!(key(40), key(41), "the same word, the same picture");
        assert_ne!(key(40), key(60), "another word, another picture");
    }

    /// The frame's canvas, black under the caption, as BGRA.
    fn painted(style: &TextStyle, cues: &[Cue], frame: u32) -> Vec<u8> {
        let blocks = plan(style, cues, &[0], frame, FPS);
        let mut canvas = vec![0u8; (W * H * 4) as usize];
        overlay_blocks(style, W, H, &blocks).blend_bgra(&mut canvas);
        canvas
    }

    #[test]
    fn an_animated_caption_is_drawn_in_the_named_family() {
        // The first serif the system has; a machine with none of them skips the test.
        let serif = ["DejaVu Serif", "Liberation Serif", "Noto Serif", "Tinos", "Times New Roman"]
            .into_iter()
            .find(|name| crate::text::family_face(name).is_some());
        let Some(serif) = serif else {
            eprintln!("no serif family is installed: skipped");
            return;
        };
        let cues = [cue("Serif words", 0.0, 3.0)];
        let bundled = painted(&style(CaptionAnim::Karaoke), &cues, 40);
        let named = TextStyle { family: Some(serif.to_string()), ..style(CaptionAnim::Karaoke) };
        assert_ne!(painted(&named, &cues, 40), bundled, "{serif} draws its own letters");
        // A family the system does not have draws exactly as the bundled face does.
        let missing = TextStyle { family: Some("No Such Family 0451".to_string()), ..style(CaptionAnim::Karaoke) };
        assert_eq!(painted(&missing, &cues, 40), bundled, "a missing family falls back to the bundled face");
    }
}
