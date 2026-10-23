//! Karaoke in an imported `.ass`: `\k`, `\kf` and `\ko` syllables drawn as the cue plays.
//!
//! The file is written here, so the test owns the colours (primary white, secondary green) and
//! every timing. A frame is the caption planned at a time ([`captionanim::plan`]) and composed
//! over a flat canvas by [`text::overlay_blocks`], which is what a burn and the preview both do;
//! the witness is the colour of the pixels at the syllable.
//!
//! ```text
//! cargo test -p engine --test ass_karaoke
//! ```

use std::path::Path;

use engine::captionanim::plan;
use engine::scratch::Scratch;
use engine::subtitle::{self, Cue};
use engine::text::{Outline, TextStyle, overlay_blocks, overlay_stack};

const W: u32 = 640;
const H: u32 = 360;
const FPS: f64 = 25.0;

/// Each event starts at 1 s and lasts 4 s. The first has two one-second `\k` syllables, the
/// second one two-second `\kf` sweep, the third a one-second `\ko`, the fourth no karaoke at all.
const SCRIPT: &str = "\
[Script Info]
ScriptType: v4.00+
PlayResX: 640
PlayResY: 360

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,48,&H00FFFFFF,&H0000FF00,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,40,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,{\\k100}MMMM {\\k100}WWWW
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,{\\kf200}MMMMMMMM
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,{\\ko100}MMMM
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,MMMM WWWW
";

fn cues() -> (Scratch, Vec<Cue>) {
    let file = Scratch::file("ve_ass_karaoke", "ass");
    std::fs::write(&*file, SCRIPT).expect("write the .ass script");
    let track = subtitle::open(Path::new(&*file), None);
    assert_eq!(track.refused, None, "the .ass parses");
    (file, track.cues)
}

fn style(outline: Outline) -> TextStyle {
    TextStyle { boxed: false, outline, ..TextStyle::default() }
}

/// Cue `n` drawn at `secs` on a mid-grey canvas: the BGRA pixels.
fn frame(cues: &[Cue], n: usize, secs: f64, outline: Outline) -> Vec<u8> {
    let style = style(outline);
    let blocks = plan(&style, cues, &[n], (secs * FPS).round() as u32, FPS);
    let mut canvas = vec![128u8; (W * H * 4) as usize];
    overlay_blocks(&style, W, H, &blocks).blend_bgra(&mut canvas);
    canvas
}

/// `(min_x, max_x, count)` of the pixels `is` holds for, or `None` when there are none.
fn extent(canvas: &[u8], is: impl Fn(u8, u8, u8) -> bool) -> Option<(u32, u32, usize)> {
    let mut out: Option<(u32, u32, usize)> = None;
    for (i, px) in canvas.chunks(4).enumerate() {
        if is(px[2], px[1], px[0]) {
            let x = i as u32 % W;
            out = Some(out.map_or((x, x, 1), |(a, b, n)| (a.min(x), b.max(x), n + 1)));
        }
    }
    out
}

fn white(r: u8, g: u8, b: u8) -> bool {
    r > 235 && g > 235 && b > 235
}

fn green(r: u8, g: u8, b: u8) -> bool {
    g > 235 && r < 60 && b < 60
}

fn black(r: u8, g: u8, b: u8) -> bool {
    r < 40 && g < 40 && b < 40
}

#[test]
fn the_file_gives_its_syllables_their_colours_and_times() {
    let (_file, cues) = cues();
    let k = cues[0].real_karaoke().expect("the \\k line has karaoke");
    assert_eq!(k.len(), 2);
    assert_eq!((k[0].start_us, k[0].end_us), (1_000_000, 2_000_000));
    assert_eq!((k[1].start_us, k[1].end_us), (2_000_000, 3_000_000));
    assert_eq!(k[0].unsung, [0, 255, 0], "the style's secondary");
    assert_eq!(k[0].sung, None, "the primary is the text's own");
    assert!(cues[3].karaoke.is_empty(), "a plain line has none");
}

#[test]
fn a_k_syllable_is_secondary_until_it_starts_then_primary() {
    let (_file, cues) = cues();
    // Before the cue: both syllables unsung.
    let before = frame(&cues, 0, 0.5, Outline::None);
    assert!(extent(&before, white).is_none(), "nothing is sung yet");
    let (g0, g1, _) = extent(&before, green).expect("both syllables are secondary");

    // During the first syllable it has begun and the second has not: white left of green.
    let during = frame(&cues, 0, 1.5, Outline::None);
    let (_, w1, _) = extent(&during, white).expect("the first syllable is primary");
    let (g2, g3, _) = extent(&during, green).expect("the second is still secondary");
    assert!(w1 < g2, "sung on the left ({w1}) and unsung on the right ({g2})");
    assert_eq!(g3, g1, "the unsung one ends where the whole line did");
    assert!(g2 > g0, "and the sung one no longer is green");

    // After both: all primary.
    let after = frame(&cues, 0, 3.5, Outline::None);
    assert!(extent(&after, green).is_none(), "nothing is left unsung");
    assert!(extent(&after, white).is_some());
}

#[test]
fn a_kf_syllable_sweeps_across_its_width() {
    let (_file, cues) = cues();
    let before = frame(&cues, 1, 0.5, Outline::None);
    let (g0, g1, _) = extent(&before, green).expect("unsung before it starts");
    assert!(extent(&before, white).is_none());

    let half = frame(&cues, 1, 2.0, Outline::None);
    let (w0, w1, _) = extent(&half, white).expect("the left half is sung");
    let (g2, g3, _) = extent(&half, green).expect("the right half is not");
    let mid = f64::from(g0 + g1) / 2.0;
    let tol = f64::from(g1 - g0) * 0.08;
    assert!((f64::from(w1) - mid).abs() < tol, "split at {w1}, expected near {mid}");
    assert!((f64::from(g2) - mid).abs() < tol, "split at {g2}, expected near {mid}");
    assert_eq!((w0, g3), (g0, g1), "together they span the whole syllable");

    let quarter = frame(&cues, 1, 1.5, Outline::None);
    let (_, q1, _) = extent(&quarter, white).expect("a quarter is sung");
    assert!(
        (f64::from(q1) - (f64::from(g0) + f64::from(g1 - g0) / 4.0)).abs() < tol,
        "a quarter in: {q1}"
    );

    let after = frame(&cues, 1, 3.5, Outline::None);
    assert!(extent(&after, green).is_none(), "all sung");
}

#[test]
fn a_ko_syllable_keeps_its_fill_and_shows_the_outline_from_its_start() {
    let (_file, cues) = cues();
    let before = frame(&cues, 2, 0.5, Outline::Thick);
    assert!(extent(&before, white).is_some(), "the fill is primary already");
    assert!(extent(&before, green).is_none(), "and never the secondary");
    assert!(extent(&before, black).is_none(), "but it has no outline yet");

    let after = frame(&cues, 2, 1.5, Outline::Thick);
    assert!(extent(&after, white).is_some());
    let (_, _, outlined) = extent(&after, black).expect("the outline appears once it starts");
    assert!(outlined > 100, "a real outline, not a speck: {outlined}");
}

#[test]
fn a_plain_ass_line_draws_exactly_what_it_always_did() {
    let (_file, cues) = cues();
    let style = style(Outline::Thick);
    let blocks = plan(&style, &cues, &[3], 50, FPS);
    assert!(blocks[0].kara.is_empty() && blocks[0].fx.is_empty());
    let mut karaoke_free = vec![128u8; (W * H * 4) as usize];
    overlay_blocks(&style, W, H, &blocks).blend_bgra(&mut karaoke_free);
    let mut old = vec![128u8; (W * H * 4) as usize];
    overlay_stack(&style, W, H, &[vec!["MMMM WWWW".to_string()]]).blend_bgra(&mut old);
    assert!(karaoke_free == old, "no karaoke, the same pixels");

    // And a karaoke line whose words were retyped has lost its karaoke, drawing as plain text.
    let mut retyped = cues[0].clone();
    retyped.text = "MMXM WWWW".into();
    assert!(retyped.real_karaoke().is_none());
    let blocks = plan(&style, &[retyped], &[0], 50, FPS);
    assert!(blocks[0].kara.is_empty());
}
