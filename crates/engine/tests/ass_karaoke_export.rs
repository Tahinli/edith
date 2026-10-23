//! An imported `.ass` with `\k` karaoke, burned into an export: the secondary colour before a
//! syllable, the primary after it, in the pictures decoded back from the file.
//!
//! Set `KARA_OUT` to a directory to keep the exported file there.
//!
//! ```text
//! cargo test -p engine --release --test ass_karaoke_export
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::decode::{DecodeSession, Frame};
use engine::export::{self, ExportSettings, Format};
use engine::project::Project;
use engine::scratch::Scratch;
use engine::subtitle;
use engine::text::{Outline, TextStyle};

const MEDIA: &str = "test_av.mp4";
const FRAMES: u32 = 150;

/// A cue from 0.5 s: `\k` syllables that start at 0.5 s and 1.5 s, then a one-second `\kf` sweep
/// from 2.5 s; a green secondary and a white primary.
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
Dialogue: 0,0:00:00.50,0:00:04.50,Default,,0,0,0,,{\\k100}MMMM {\\k100}WWWW {\\kf100}KKKK
";

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

fn wait(handle: &engine::ExportHandle) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(1800), "export did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// Bottom-third pixels that are green, white in `f` and were not in `plain`: the caption's own.
fn lit(f: &Frame, plain: &Frame) -> (usize, usize) {
    let row = f.width as usize * 4;
    let start = (f.height as usize * 2 / 3) * row;
    let (mut g, mut w) = (0, 0);
    let px = |f: &Frame| f.bgra[start..].as_chunks::<4>().0.iter().map(|p| (p[2], p[1], p[0])).collect::<Vec<_>>();
    for ((r, gr, b), (pr, pg, pb)) in px(f).into_iter().zip(px(plain)) {
        let green = |r: u8, g: u8, b: u8| g > 200 && r < 90 && b < 90;
        let white = |r: u8, g: u8, b: u8| r > 235 && g > 235 && b > 235;
        g += usize::from(green(r, gr, b) && !green(pr, pg, pb));
        w += usize::from(white(r, gr, b) && !white(pr, pg, pb));
    }
    (g, w)
}

/// Exports the whole project and returns the frames `want` of the file, in order.
fn exported(project: Project, burn: Option<(usize, TextStyle)>, keep: Option<&str>, want: &[u32]) -> Vec<Frame> {
    let out = Scratch::file("ve_kara_export", "mp4");
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let handle = export::start(
        project,
        meta,
        &out,
        &ExportSettings { format: Format::Mp4, burn_subtitles: burn, ..Default::default() },
        None,
    );
    wait(&handle).expect("the export finishes");
    if let (Some(name), Ok(dir)) = (keep, std::env::var("KARA_OUT")) {
        std::fs::copy(&*out, Path::new(&dir).join(name)).expect("keep the export");
    }
    let (_, rx) = DecodeSession::open(&out).expect("decode the export back");
    let mut got: Vec<Frame> = rx.into_iter().filter(|f| want.contains(&f.index)).take(want.len()).collect();
    got.sort_by_key(|f| f.index);
    assert_eq!(got.len(), want.len());
    got
}

#[test]
fn karaoke_is_burned_into_the_export() {
    engine::envcfg::set("VE_SW", "1");
    engine::envcfg::set("VE_SW_ENC", "1");
    let script = Scratch::file("ve_kara_export", "ass");
    std::fs::write(&*script, SCRIPT).expect("write the script");
    let track = subtitle::open(Path::new(&*script), None);
    assert_eq!(track.refused, None);
    let project = Project::single(asset(MEDIA), FRAMES).with_subtitles(vec![track]);
    let style = TextStyle { outline: Outline::Thin, boxed: false, ..TextStyle::default() };
    // 30 fps: frame 27 is 0.9 s (the first syllable is sung, the other two not), 90 is 3.0 s
    // (two sung, the sweep half way), 120 is 4.0 s (all sung).
    let want = [27u32, 90, 120];
    let plain = exported(project.clone(), None, None, &want);
    let burned = exported(project, Some((0, style)), Some("kara.mp4"), &want);
    let seen: Vec<_> = burned.iter().zip(&plain).map(|(f, p)| lit(f, p)).collect();
    let (early, half, done) = (seen[0], seen[1], seen[2]);
    assert!(early.0 > 100 && early.1 > 100, "one sung, two not: {seen:?}");
    assert!(half.0 > 50 && half.1 > early.1, "more sung, the sweep still green on its right: {seen:?}");
    assert!(half.0 < early.0, "less green than before: {seen:?}");
    assert!(done.0 < 20 && done.1 > half.1, "all sung: {seen:?}");
}
