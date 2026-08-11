//! An HDR stream reaching the window tone-mapped, through the door the window
//! itself opens ([`PlaybackSession`]) rather than through [`engine::tonemap`]
//! alone -- which is the whole of the claim: a PQ file used to be shown as if
//! its curve were SDR, i.e. grey-washed and flat, and the map has to run inside
//! the decode funnel for that to stop.
//!
//! `test_pq.mp4` is four flat patches whose codes `scripts/gen_fixtures.sh`
//! writes into the planes by hand, so every number below is named rather than
//! measured-and-blessed: BT.2408 diffuse white (203 cd/m^2 is PQ code 144), the
//! 1000 cd/m^2 highlight (code 181) and a saturated BT.2020 red.
//!
//! ```text
//! cargo test -p engine --release --test tonemap_render -- --test-threads=1
//! ```

use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::color::{ColorParams, apply_yuv};
use engine::colorspace::{ColorDescription, Matrix, Transfer};
use engine::convert::i420_to_bgra;
use engine::project::Lane;
use engine::tonemap::{Preset, ToneMapper, Transfer as Hdr};

/// The fixture's patches: the source codes, and where the centre of each one
/// sits in the 320x240 picture.
const DIFFUSE: ([u8; 3], (usize, usize)) = ([144, 128, 128], (80, 60));
const HIGHLIGHT: ([u8; 3], (usize, usize)) = ([181, 128, 128], (240, 60));
const RED: ([u8; 3], (usize, usize)) = ([100, 90, 200], (160, 180));

/// What the tone map hands the rest of the funnel: BT.709, SDR, limited range.
const SDR: ColorDescription = ColorDescription {
    matrix: Matrix::Bt709,
    transfer: Transfer::Sdr,
    full_range: false,
};

/// Frame 0 of `test_h264.mkv` as this engine rendered it before the tone map
/// existed -- passthrough, ungraded -- and the same file on a 640x360 canvas
/// with a grade on it, which is the other branch of the funnel. Measured on
/// `main`; an SDR stream may not move by one byte.
const SDR_PASSTHROUGH: u64 = 0x9EF1_6F6F_5310_BCEE;
const SDR_GRADED_AND_PLACED: u64 = 0xAFA6_6394_3937_DE5B;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x100_0000_01b3)
    })
}

/// Silent and software-decoded: the fixture is H.264 on purpose, so the bytes
/// are the same on a machine with a VA-API plugin and one without (hence
/// `--test-threads=1`).
fn open(name: &str) -> PlaybackSession {
    unsafe { std::env::set_var("VE_SW", "1") };
    let session = PlaybackSession::open(asset(name)).expect("open");
    session.set_gain(0.0);
    session
}

fn next_frame(session: &mut PlaybackSession, what: &str) -> engine::Frame {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(frame) = session.try_frame() {
            return frame;
        }
        assert!(Instant::now() < deadline, "no frame after {what}");
        sleep(Duration::from_millis(4));
    }
}

/// One pixel of a rendered frame, as (B, G, R).
fn pixel(frame: &engine::Frame, (x, y): (usize, usize)) -> [u8; 3] {
    let at = (y * frame.width as usize + x) * 4;
    [frame.bgra[at], frame.bgra[at + 1], frame.bgra[at + 2]]
}

/// The limited-range BT.709 luma code a rendered pixel carries: the unit the
/// tone map's own anchors are named in, read back off the BGRA the window gets.
fn luma_code(bgra: [u8; 3]) -> u8 {
    let (b, g, r) = (f32::from(bgra[0]), f32::from(bgra[1]), f32::from(bgra[2]));
    (16.0 + 219.0 * (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0).round() as u8
}

/// How far one pixel is from grey.
fn spread(px: [u8; 3]) -> u8 {
    px.iter().max().unwrap() - px.iter().min().unwrap()
}

/// One flat patch of `codes`, rendered by hand the way the funnel is supposed
/// to render it: tone-mapped first, then graded, then converted as BT.709 SDR.
/// `grade` is applied in whichever domain `before` says -- which is the point of
/// [`the_grade_lands_after_the_tone_map`].
fn by_hand(
    codes: [u8; 3],
    grade: &ColorParams,
    before: bool,
    preset: Preset,
    peak: Option<f32>,
) -> [u8; 3] {
    let (w, h) = (8usize, 8usize);
    let mut y = vec![codes[0]; w * h];
    let mut u = vec![codes[1]; w / 2 * h / 2];
    let mut v = vec![codes[2]; w / 2 * h / 2];
    if before {
        apply_yuv(grade, &mut y, &mut u, &mut v);
    }
    ToneMapper::new(Hdr::Pq, preset, peak).map(&mut y, &mut u, &mut v, w, h);
    if !before {
        apply_yuv(grade, &mut y, &mut u, &mut v);
    }
    let bgra = i420_to_bgra(&SDR, &y, &u, &v, w, h);
    [bgra[0], bgra[1], bgra[2]]
}

/// The claim itself: a PQ stream is shown as an SDR display can show it.
/// Diffuse white lands where BT.2446 Method A puts it at the mastering peak the
/// content really has (~166, not the 144 an untouched code would convert to and
/// not clipped white -- the reference rendition, which is what a project nobody
/// has picked for is shown in), the 1000 cd/m^2 highlight stays a highlight, and
/// the saturated red is still red -- a grey wash is exactly what the missing map
/// looked like.
#[test]
fn a_pq_stream_is_tone_mapped_on_its_way_to_the_window() {
    let mut session = open("test_pq.mp4");
    assert_eq!(session.meta().color.transfer, Transfer::Pq, "fixture tags");
    assert_eq!(
        session.meta().color.matrix,
        Matrix::Bt2020Ncl,
        "fixture tags"
    );

    let frame = next_frame(&mut session, "an open");
    assert_eq!((frame.width, frame.height), (320, 240));

    let diffuse = luma_code(pixel(&frame, DIFFUSE.1));
    assert!(
        (161..=171).contains(&diffuse),
        "diffuse white (code {}) rendered at {diffuse}, wanted 161..=171",
        DIFFUSE.0[0]
    );
    let highlight = luma_code(pixel(&frame, HIGHLIGHT.1));
    assert!(
        (225..=239).contains(&highlight) && highlight > diffuse,
        "the 1000 cd/m^2 highlight rendered at {highlight}"
    );
    let red = pixel(&frame, RED.1);
    assert!(
        spread(red) > 60 && red[2] > red[0] && red[2] > red[1],
        "the saturated red washed out to {red:?}"
    );
}

/// The preset is a thing a viewer *sees*: picked on the running session, the
/// very next frame out of the funnel is the new rendition, and the three are in
/// the order their names promise -- reference darkest, vivid brightest and
/// richest. Measured on the rendered BGRA the window gets, so it covers the
/// whole chain (project setting -> reseek -> decode worker -> LUT), not the
/// table alone.
#[test]
fn picking_a_preset_changes_the_picture_the_window_shows() {
    let mut session = open("test_pq.mp4");
    assert_eq!(session.tone(), Preset::Reference, "nobody has picked yet");
    let mut shown = Vec::new();
    for preset in Preset::ALL {
        if preset != Preset::Reference {
            assert!(session.set_tone(preset), "pick {preset:?}");
            assert!(!session.set_tone(preset), "picking it again changes nothing");
        }
        let frame = next_frame(&mut session, "a preset");
        // Still a picture, not a wash: the fixture's saturated red has to stay
        // red under every rendition, which is what a mistuned preset breaks
        // first.
        assert!(spread(pixel(&frame, RED.1)) > 60, "{preset:?} washed the red out");
        shown.push(luma_code(pixel(&frame, DIFFUSE.1)));
    }
    let [reference, standard, vivid] = [shown[0], shown[1], shown[2]];
    assert!(
        reference < standard && standard < vivid,
        "diffuse white by preset: reference {reference}, standard {standard}, vivid {vivid}"
    );
    // The vivid preset's *chroma* gain is asserted on the codes rather than
    // here: this fixture's red is saturated enough that it clips to 255 in BGRA
    // under both presets, so a rendered patch cannot measure it
    // (`tonemap::tests::a_brighter_preset_is_brighter_and_a_vivid_one_is_richer`).
}

/// ...and an SDR stream is not any of this: no clip of it builds a tone map at
/// all, so all three presets are the same bytes. The hash is the passthrough
/// branch's own, from the test below -- if a preset ever reached an SDR picture,
/// this is where it would show.
#[test]
fn an_sdr_stream_is_the_same_bytes_under_every_preset() {
    let mut session = open("test_h264.mkv");
    for preset in Preset::ALL {
        if preset != Preset::Reference {
            assert!(session.set_tone(preset), "pick {preset:?}");
        }
        let frame = next_frame(&mut session, "a preset");
        assert_eq!(fnv(&frame.bgra), SDR_PASSTHROUGH, "{preset:?} moved an SDR picture");
    }
}

/// The other half: an SDR stream may not move by one byte for any of this.
/// Both branches of the funnel -- the fused passthrough conversion and the
/// graded-then-placed one -- against hashes measured before the tone map was
/// wired in.
#[test]
fn an_sdr_stream_is_byte_for_byte_the_picture_it_was() {
    let mut session = open("test_h264.mkv");
    assert_eq!(session.meta().color.transfer, Transfer::Sdr);
    let plain = next_frame(&mut session, "an open");
    assert_eq!(fnv(&plain.bgra), SDR_PASSTHROUGH, "the passthrough branch");

    assert!(session.set_resolution(640, 360), "shrink the canvas");
    assert!(
        session.set_color(
            Lane::V1,
            0,
            Some(ColorParams {
                saturation: 1.4,
                brightness: 0.1,
                ..Default::default()
            })
        ),
        "grade the clip"
    );
    let placed = next_frame(&mut session, "a resize and a grade");
    assert_eq!((placed.width, placed.height), (640, 360));
    assert_eq!(
        fnv(&placed.bgra),
        SDR_GRADED_AND_PLACED,
        "the graded-and-placed branch"
    );
}

/// Order: the grade is a look applied to the picture the viewer sees, so it
/// lands on the *tone-mapped* samples. Graded before the map instead, the same
/// numbers give a different pixel -- the second assert is what makes the first
/// one mean anything.
#[test]
fn the_grade_lands_after_the_tone_map() {
    let grade = ColorParams {
        saturation: 1.5,
        brightness: 0.08,
        ..Default::default()
    };
    let mut session = open("test_pq.mp4");
    assert!(
        session.set_color(Lane::V1, 0, Some(grade)),
        "grade the clip"
    );
    let frame = next_frame(&mut session, "a grade");

    for (name, patch) in [("diffuse white", DIFFUSE), ("red", RED)] {
        let shown = pixel(&frame, patch.1);
        let after = by_hand(patch.0, &grade, false, Preset::default(), None);
        let before = by_hand(patch.0, &grade, true, Preset::default(), None);
        for (i, channel) in ["B", "G", "R"].iter().enumerate() {
            assert!(
                shown[i].abs_diff(after[i]) <= 2,
                "{name} {channel}: shown {shown:?}, tone-map-then-grade {after:?}"
            );
        }
        assert!(
            after.iter().zip(&before).any(|(a, b)| a.abs_diff(*b) > 4),
            "{name}: grading before the map gives the same pixel, so this proves nothing"
        );
    }
}

/// The film's own declared peak, all the way to the window: `test_hdr_bright.mkv`
/// carries the same four patches as `test_pq.mp4` and a container MaxCLL of
/// 4000 cd/m^2, so a funnel that read the metadata renders it at 4000 and one
/// that ignored it renders it at the 1000 an undeclared file is assumed at --
/// two plainly different pictures, which is why that fixture says 4000 and the
/// older metadata ones say the 1000 nothing can tell apart from the fallback.
///
/// Both sides are computed by hand through the same `ToneMapper`, so what is
/// asserted is which number the *funnel* built its table with, not where Method
/// A puts a patch.
#[test]
fn a_films_declared_peak_reaches_the_window() {
    let mut session = open("test_hdr_bright.mkv");
    assert_eq!(session.meta().color.transfer, Transfer::Pq, "fixture tags");
    assert_eq!(session.tone(), Preset::Reference, "nobody has picked yet");
    let frame = next_frame(&mut session, "an open");
    let flat = ColorParams::default();
    for (patch, name) in [(DIFFUSE, "diffuse white"), (HIGHLIGHT, "the highlight")] {
        let shown = luma_code(pixel(&frame, patch.1));
        let declared = luma_code(by_hand(patch.0, &flat, false, Preset::Reference, Some(4000.0)));
        let ignored = luma_code(by_hand(patch.0, &flat, false, Preset::Reference, None));
        assert!(
            shown.abs_diff(declared) <= 2,
            "{name} rendered at {shown}, {declared} at the film's declared 4000 cd/m^2"
        );
        assert!(
            shown.abs_diff(ignored) > 8,
            "{name}: {shown} is indistinguishable from the {ignored} of an ignored peak"
        );
    }
}

/// ...and both tiers a peak can live in reach the table, on the fixtures that
/// exist for each: `test_hdr_meta.mkv` declares its 1000 in the container,
/// `test_hdr_sei.mkv` only inside the HEVC bitstream. Neither can be told apart
/// from the fallback in a *picture* (1000 is what an undeclared file is assumed
/// at, and both are black), so what is asserted is the number the table is
/// built at -- the same `Demuxer::light().peak()` hand-off the decode funnel
/// and the export table make.
#[test]
fn either_tier_of_metadata_is_what_the_table_is_built_at() {
    for (name, tier) in [
        ("test_hdr_meta.mkv", "container"),
        ("test_hdr_meta.mp4", "container"),
        ("test_hdr_sei.mkv", "bitstream SEI"),
        ("test_hdr_bright.mkv", "container"),
    ] {
        let (_, demuxer) = engine::demux::Demuxer::open(&asset(name)).expect("open");
        let peak = demuxer.light().peak();
        let want = match name {
            "test_hdr_bright.mkv" => 4000.0,
            _ => 1000.0,
        };
        assert_eq!(peak, Some(want), "{name} ({tier} tier)");
        let mapper = ToneMapper::new(Hdr::Pq, Preset::Reference, peak);
        assert_eq!(mapper.peak(), want, "{name}: the table's assumed peak");
    }
    // An SDR file declares nothing, and nothing is what the map is told.
    let (_, demuxer) = engine::demux::Demuxer::open(&asset("test_av.mp4")).expect("open");
    assert_eq!(demuxer.light().peak(), None, "an SDR file's peak");
}
