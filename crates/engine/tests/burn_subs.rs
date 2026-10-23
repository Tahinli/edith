//! Burned-in captions: the cue on screen is drawn into the picture itself, so
//! an export, a composed frame and anything decoded from the file all carry it,
//! where a soft track is only there for a player that reads it.
//!
//! Witnesses: the decoded frame of an export made with and without the burn
//! (the same timeline, the same encoder), and `compose_at_with` against
//! `compose_at`. A frame with a caption must differ in the bottom quarter, and
//! a frame with no caption on screen must not differ anywhere a caption could
//! sit.
//!
//! ```text
//! cargo test -p engine --release --test burn_subs -- --nocapture
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::ExportHandle;
use engine::decode::{DecodeSession, Frame};
use engine::export::{self, ExportSettings, Format};
use engine::project::Project;
use engine::scratch::Scratch;
use engine::subtitle::{Cue, SubtitleTrack};
use engine::captionanim::{CaptionAnim, CaptionStyle};
use engine::text::{Outline, Place, TextStyle};

/// Five seconds at 30 fps, a picture this box decodes with nothing installed.
const MEDIA: &str = "test_av.mp4";
const FRAMES: u32 = 150;
/// The cue `test_subs.srt` puts on screen from 0.5 s to 1.5 s: timeline frames
/// 15 to 44. Frame 20 is inside it, frame 5 well before it.
const IN_CUE: u32 = 20;
const BEFORE_CUE: u32 = 5;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn pin_software() {
    engine::envcfg::set("VE_SW", "1");
    engine::envcfg::set("VE_SW_ENC", "1");
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < limit,
            "export did not finish in {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// The first cue of `tests/data/test_subs.srt`, on the media's own clock.
fn track() -> SubtitleTrack {
    SubtitleTrack {
        path: asset(MEDIA),
        track: Some(2),
        language: "eng".into(),
        name: String::new(),
        label: "eng".into(),
        bitmap: false,
        cues: vec![
            Cue {
                start_us: 500_000,
                end_us: 1_500_000,
                text: "first line".into(),
                image: None,
                words: Vec::new(),
                karaoke: Vec::new(),
            },
            Cue {
                start_us: 2_000_000,
                end_us: 3_250_000,
                text: "second line\nwith a break".into(),
                image: None,
                words: Vec::new(),
                karaoke: Vec::new(),
            },
        ],
        refused: None,
    }
}

fn project() -> Project {
    Project::single(asset(MEDIA), FRAMES).with_subtitles(vec![track()])
}

fn style() -> TextStyle {
    TextStyle {
        outline: Outline::Thin,
        boxed: true,
        place: Place::Bottom,
        ..TextStyle::default()
    }
}

/// Pixels whose brightest channel moved by more than the noise an encoder
/// leaves, counted inside the bottom quarter of the frame -- where a caption at
/// [`Place::Bottom`] sits. Its height is the frame's own, so the answer does not
/// depend on the output size.
fn bottom_changes(a: &[u8], b: &[u8], w: u32, h: u32) -> usize {
    let row = w as usize * 4;
    let start = (h as usize * 3 / 4) * row;
    a[start..]
        .as_chunks::<4>().0.iter()
        .zip(b[start..].as_chunks::<4>().0)
        .filter(|(p, q)| {
            p.iter()
                .zip(q.iter())
                .any(|(x, y)| (i32::from(*x) - i32::from(*y)).abs() > 64)
        })
        .count()
}

/// Exports one second of the project (frames 0 to 30) with `burn` set or not,
/// and returns the file's frames at `want`, decoded back.
fn exported(name: &str, burn: Option<(usize, TextStyle)>, want: &[u32]) -> Vec<Frame> {
    pin_software();
    let out = Scratch::file(&format!("ve_burn_{name}"), "mp4");
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let handle = export::start(
        project(),
        meta,
        &out,
        &ExportSettings {
            format: Format::Mp4,
            range: Some((0, 30)),
            burn_subtitles: burn,
            ..Default::default()
        },
        None,
    );
    wait(&handle, Duration::from_secs(1800)).expect("the export finishes");
    let (_, rx) = DecodeSession::open(&out).expect("decode the export back");
    let mut frames = Vec::new();
    for f in rx {
        if want.contains(&f.index) {
            frames.push(f);
        }
        if frames.len() == want.len() {
            break;
        }
    }
    assert_eq!(frames.len(), want.len(), "every wanted frame decodes");
    // The decoder hands frames over in file order, which is not the order asked.
    frames.sort_by_key(|f| want.iter().position(|w| *w == f.index));
    frames
}

#[test]
fn a_burned_caption_is_in_the_exported_picture_and_only_while_on_screen() {
    let plain = exported("plain", None, &[IN_CUE, BEFORE_CUE]);
    let burned = exported("burned", Some((0, style())), &[IN_CUE, BEFORE_CUE]);
    let (w, h) = (plain[0].width, plain[0].height);
    let in_cue = bottom_changes(&plain[0].bgra, &burned[0].bgra, w, h);
    assert!(
        in_cue > 200,
        "the caption left no mark in the bottom quarter ({in_cue} pixels)"
    );
    // No caption is on screen at frame 5, so the picture must match the plain
    // export in the very region a caption would have drawn into.
    let before = bottom_changes(&plain[1].bgra, &burned[1].bgra, w, h);
    assert_eq!(
        before, 0,
        "a frame with no caption on screen was changed ({before} pixels)"
    );
}

#[test]
fn compose_at_with_draws_the_cue_on_screen_and_nothing_else() {
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let p = project();
    let (w, h) = (640, 360);
    let plain = export::compose_at(&p, &meta, IN_CUE, w, h).expect("compose");
    let burned = export::compose_at_with(&p, &meta, IN_CUE, w, h, Some((0, style())))
        .expect("compose with a burn");
    let drawn = bottom_changes(&plain.bgra, &burned.bgra, w, h);
    assert!(drawn > 200, "caption not drawn ({drawn} pixels)");
    let quiet = export::compose_at_with(&p, &meta, BEFORE_CUE, w, h, Some((0, style())))
        .expect("compose before the cue");
    let plain_quiet = export::compose_at(&p, &meta, BEFORE_CUE, w, h).expect("compose");
    assert!(
        quiet.bgra == plain_quiet.bgra,
        "no caption is on screen at frame {BEFORE_CUE}"
    );
}

#[test]
fn a_pick_that_is_not_a_track_is_named() {
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let Err(err) = export::compose_at_with(&project(), &meta, IN_CUE, 640, 360, Some((7, style())))
    else {
        panic!("track 7 does not exist, yet a frame came back");
    };
    assert!(err.to_string().contains("no subtitle track 7"), "{err}");
}

/// A title past the last clip exports over black: the file runs on to the
/// title's end, and the frame it shows is white letters on a dark picture.
#[test]
fn a_title_past_the_clip_is_exported_over_black() {
    use engine::project::Title;
    pin_software();
    let mut p = Project::single(asset(MEDIA), FRAMES);
    p.add_title(Title {
        start: 150,
        frames: 30,
        text: "Episode 17".into(),
        style: TextStyle {
            place: Place::Centre,
            boxed: false,
            ..TextStyle::default()
        },
        fade_in: 0,
        fade_out: 0,
        in_anim: Default::default(),
        out_anim: Default::default(),
        anim_frames: 0,
    })
    .expect("the title is added");
    let out = Scratch::file("ve_title_gap", "mp4");
    let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
    let handle = export::start(
        p,
        meta,
        &out,
        &ExportSettings {
            format: Format::Mp4,
            range: Some((150, 180)),
            ..Default::default()
        },
        None,
    );
    wait(&handle, Duration::from_secs(1800)).expect("the export finishes");
    let (_, rx) = DecodeSession::open(&out).expect("decode the export back");
    let frame = rx
        .into_iter()
        .take(60)
        .find(|f| f.index == 10)
        .expect("the export holds the title's tenth frame");
    let total = (frame.width * frame.height) as usize;
    let white = frame
        .bgra
        .as_chunks::<4>().0.iter()
        .filter(|px| px[0].max(px[1]).max(px[2]) > 200)
        .count();
    let lit = frame
        .bgra
        .as_chunks::<4>().0.iter()
        .filter(|px| px[0].max(px[1]).max(px[2]) > 40)
        .count();
    assert!(white > 100, "no white text in the exported title ({white} pixels)");
    assert!(lit * 4 < total, "the exported title is not over black ({lit} of {total})");
}

/// A caption set in a named family and animated (karaoke) is burned in that face: the same export in
/// the bundled face differs inside the caption. Needs the `Noto Serif` family on the system. Set
/// `EDITH_FONT_EXPORT` to keep the serif export's file for a frame grab.
#[test]
fn a_named_family_is_burned_into_an_animated_caption() {
    pin_software();
    let animated = |family: Option<&str>| TextStyle {
        outline: Outline::Thick,
        family: family.map(str::to_string),
        caption: CaptionStyle { anim: CaptionAnim::Karaoke, ..CaptionStyle::default() },
        ..style()
    };
    let run = |out: &std::path::Path, burn: TextStyle| {
        let (meta, _) = engine::demux::Demuxer::open(&asset(MEDIA)).expect("probe the media");
        let handle = export::start(
            project(),
            meta,
            out,
            &ExportSettings { format: Format::Mp4, range: Some((0, 30)), burn_subtitles: Some((0, burn)), ..Default::default() },
            None,
        );
        wait(&handle, Duration::from_secs(1800)).expect("the export finishes");
        let (_, rx) = DecodeSession::open(out).expect("decode the export back");
        rx.into_iter().find(|f| f.index == IN_CUE).expect("the caption frame decodes")
    };
    let bundled = run(&Scratch::file("ve_burn_bundled", "mp4"), animated(None));
    let scratch_serif = Scratch::file("ve_burn_serif", "mp4");
    let serif_out = std::env::var_os("EDITH_FONT_EXPORT").map_or_else(|| scratch_serif.to_path_buf(), PathBuf::from);
    let serif = run(&serif_out, animated(Some("Noto Serif")));
    let changed = bottom_changes(&bundled.bgra, &serif.bgra, serif.width, serif.height);
    assert!(changed > 100, "the named family draws different letters: {changed} pixels changed");
}
