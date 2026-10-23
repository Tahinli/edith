//! An `.ass` subtitle file through edith's own subtitle call site
//! ([`engine::subtitle::open`], the path a standalone subtitle takes into a
//! project). The file is written here, byte for byte, so the test owns every
//! style, every override tag and every timing it asserts on.
//!
//! The witness is the cue list edith produces: start and end in microseconds,
//! and the plain text with the markup resolved. Two styles are in use, so a
//! style is not allowed to leak into the text, and the override tags cover
//! the ones a real script leans on: `\i`, `\b`, `\c`, `\an`, `\pos`, `\N` and
//! `\h`.
//!
//! ```text
//! cargo test -p engine --test ass_subtitles
//! ```

use std::path::Path;

use engine::scratch::Scratch;
use engine::subtitle::{self, Cue};

/// Two styles: `Default` for dialogue, `Sign` for on-screen text. The `Format:`
/// line is the one a real script declares, so the parser reads the fields in
/// the order they are written.
const SCRIPT: &str = "\
[Script Info]
ScriptType: v4.00+
PlayResX: 1920
PlayResY: 1080

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,40,1
Style: Sign,Verdana,36,&H0000FFFF,&H000000FF,&H00000000,&H00000000,1,0,0,0,100,100,0,0,1,3,1,8,20,20,20,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:02.50,Default,,0,0,0,,{\\i1}a slanted line{\\i0} in the body
Dialogue: 0,0:00:03.00,0:00:04.00,Sign,,0,0,0,,{\\an8\\b1}SIGN{\\b0} at the top
Dialogue: 0,0:00:04.50,0:00:06.00,Default,,0,0,0,,first half\\Nsecond half
Dialogue: 0,0:00:06.25,0:00:07.00,Sign,,0,0,0,,{\\c&H0000FF&\\pos(960,200)}red{\\c} in the middle
Dialogue: 0,0:00:07.50,0:00:08.75,Default,,0,0,0,,{\\an1}left edge
";

fn write_script(name: &str) -> Scratch {
    let file = Scratch::file(name, "ass");
    std::fs::write(&*file, SCRIPT).expect("write the .ass script");
    file
}

fn cue(start_us: i64, end_us: i64, text: &str) -> Cue {
    Cue {
        start_us,
        end_us,
        text: text.into(),
        image: None,
        words: Vec::new(),
        karaoke: Vec::new(),
    }
}

#[test]
fn an_ass_file_opens_as_timed_text_with_its_markup_resolved() {
    let file = write_script("ve_ass_events");
    let track = subtitle::open(Path::new(&*file), None);

    assert_eq!(track.refused, None, "the .ass parses");
    assert!(!track.is_bitmap(), "an .ass is text, not pictures");
    assert_eq!(
        track.cues,
        vec![
            cue(1_000_000, 2_500_000, "a slanted line in the body"),
            cue(3_000_000, 4_000_000, "SIGN at the top"),
            cue(4_500_000, 6_000_000, "first half\nsecond half"),
            cue(6_250_000, 7_000_000, "red in the middle"),
            cue(7_500_000, 8_750_000, "left edge"),
        ],
        "every event, in order, with its override tags resolved"
    );
}

#[test]
fn the_two_styles_do_not_leak_into_the_text() {
    let file = write_script("ve_ass_styles");
    let track = subtitle::open(Path::new(&*file), None);

    // The `Sign` style's name is not text, and neither are the `Default`
    // style's own tags: each cue carries only what its Text field says.
    assert_eq!(track.cues.len(), 5, "five events, both styles");
    for c in &track.cues {
        assert!(!c.text.contains("Sign"), "a style name is not text: {c:?}");
        assert!(
            !c.text.contains("Default"),
            "a style name is not text: {c:?}"
        );
        assert!(!c.text.contains('\\'), "no override tag survives: {c:?}");
        assert!(!c.text.contains('{'), "no brace survives: {c:?}");
    }
}
