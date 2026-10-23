//! A still's own EXIF orientation, applied.
//!
//! A phone photographs a portrait picture and stores it as landscape plus a tag
//! saying which way up it is. Before this the engine read neither: the `image`
//! crate keeps the orientation in its decoder and applies nothing, so the
//! preview, the timeline clip box, the poster thumbnail, the canvas seed and an
//! export of that clip were all the stored shape -- a portrait photo on its
//! side, silently, in every one of them.
//!
//! The fixture is **built here**, because none of the ones in `assets/` carry
//! an orientation tag (`scripts/gen_fixtures.sh` writes PNG stills, and PNG has
//! no such tag). It is a real JPEG: four coloured quadrants, an APP1 segment
//! holding a TIFF IFD whose tag `0x0112` states the orientation. A single
//! sampled pixel per corner then names the layout, and a *wrong direction*
//! cannot pass -- 90 the other way, 180, or "ignore it" each produce a
//! different set of four corners.
//!
//! ```text
//! cargo test -p engine --test exif_orientation -- --test-threads=1
//! ```

use std::path::PathBuf;

use engine::scratch::Scratch;
use engine::{DecodeSession, PlaybackSession, Rotation, image_orientation, image_size};

/// A built fixture and the directory holding it, which is removed when the
/// guard drops -- [`Scratch`]'s own contract, so a run leaves nothing behind
/// and two runs cannot race for a name.
fn fixture(exif: u8) -> (Scratch, PathBuf) {
    let scratch = Scratch::file(&format!("ve_exif_{exif}"), "jpg");
    let path = PathBuf::from(&*scratch);
    std::fs::write(&path, build(exif)).expect("write the built fixture");
    (scratch, path)
}

/// Which of the four colours a pixel is, by name. The quadrants are far enough
/// apart that a 4:2:0 chroma boundary cannot colour a sample taken deep inside
/// one, and JPEG's own chroma subsampling cannot either.
fn tint(rgb: [u8; 3]) -> &'static str {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    match (r > 110, g > 90, b > 110) {
        (true, true, false) => "yellow",
        (true, false, false) => "red",
        (false, true, false) => "green",
        (false, false, true) => "blue",
        other => panic!("a quadrant centre is not one of the four colours: {other:?}"),
    }
}

/// The four corners of one tightly packed BGRA picture, top-left first and
/// clockwise -- sampled deep enough inside each quadrant that no boundary can
/// colour the sample.
fn corners(width: u32, height: u32, bgra: &[u8]) -> [&'static str; 4] {
    let at = |x: u32, y: u32| {
        let index = ((y * width + x) * 4) as usize;
        let (b, g, r) = (bgra[index], bgra[index + 1], bgra[index + 2]);
        tint([r, g, b])
    };
    let (qx, qy) = (width / 4, height / 4);
    [
        at(qx, qy),
        at(3 * qx, qy),
        at(3 * qx, 3 * qy),
        at(qx, 3 * qy),
    ]
}

/// The quadrants of the picture as it is **written into the file**, in the same
/// order [`corners`] reads them. The file is landscape 64x32 whatever the
/// orientation says, because that is what a phone does: it stores the sensor's
/// own shape and says the rest in the tag.
const STORED: [&str; 4] = ["red", "green", "blue", "yellow"];

/// The stored picture, as a JPEG with no orientation tag: 64x32, four coloured
/// quadrants (`tests/data/quadrants_64x32.jpg`, written once with a JPEG
/// encoder and checked in, so this suite needs no encoder at run time).
const BASE_JPEG: &[u8] = include_bytes!("data/quadrants_64x32.jpg");

/// The fixture with an APP1/EXIF segment stating `exif` as the orientation.
///
/// The EXIF is written by hand rather than through a crate: the segment is six
/// bytes of `Exif\0\0` followed by a little-endian TIFF header and one IFD
/// entry, which is the whole of what tag `0x0112` is, and writing it out is what
/// makes this a fixture rather than a dependency.
fn build(exif: u8) -> Vec<u8> {
    let jpeg = BASE_JPEG;
    // Inserted straight after SOI, which is where a camera's APP1 sits and
    // where a decoder's marker walk finds it first.
    let app1 = exif_app1(exif);
    let mut out = Vec::with_capacity(jpeg.len() + app1.len());
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// A complete APP1 segment carrying `exif` as EXIF tag `0x0112`: the `Exif\0\0`
/// identifier, a little-endian TIFF header pointing its IFD0 at offset 8, one
/// entry (tag, type SHORT, count 1, the value), and a zero "next IFD" pointer.
fn exif_app1(exif: u8) -> Vec<u8> {
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II\x2a\x00"); // little-endian, magic 42
    tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8
    tiff.extend_from_slice(&1u16.to_le_bytes()); // one entry
    tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_le_bytes()); // type SHORT
    tiff.extend_from_slice(&1u32.to_le_bytes()); // count 1
    tiff.extend_from_slice(&u16::from(exif).to_le_bytes());
    tiff.extend_from_slice(&[0, 0]); // the value's padding to an even offset
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no IFD1

    let mut payload = b"Exif\x00\x00".to_vec();
    payload.extend_from_slice(&tiff);

    let mut segment = vec![0xff, 0xe1];
    // The length field counts itself and the two marker bytes.
    segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    segment.extend_from_slice(&payload);
    segment
}

/// One frame of `path` and the size it came out at -- the same funnel
/// `PlaybackSession` paints from, so this is what a poster thumbnail and the
/// preview would each hold.
fn first_frame(path: &std::path::Path) -> (u32, u32, Vec<u8>) {
    let (_, frames, _) = DecodeSession::open_range(path, 0, 1).expect("open for a frame");
    let frame = frames.recv().expect("a first frame");
    (frame.width, frame.height, frame.bgra)
}

/// Software everywhere, so the suite proves the path every machine has.
fn pin_software() {
    static PIN: std::sync::Once = std::sync::Once::new();
    PIN.call_once(|| {
        engine::envcfg::set("VE_SW", "1");
        engine::envcfg::set("VE_SW_ENC", "1");
    });
}

/// The whole of the defect, asserted through **both** doors that used to
/// disagree: the header-only size a library row reads before anything places
/// the file, and the first decoded frame a preview shows after. They are
/// separate code paths over the same tag, so a fix to one that skipped the
/// other would leave a file that is one shape in the library and another on the
/// timeline.
///
/// EXIF 6 is the portrait case -- a quarter turn clockwise -- so the stored
/// 64x32 is shown 32x64, and each stored corner moves one place clockwise.
#[test]
fn a_portrait_photo_is_shown_the_way_it_was_photographed() {
    pin_software();
    let (_scratch, path) = fixture(6);

    assert_eq!(
        image_size(&path).expect("the header answers"),
        (32, 64),
        "a library row must describe the picture as it is shown, not as it is stored"
    );
    assert_eq!(
        image_orientation(&path).expect("the tag is readable"),
        Rotation::Cw90,
        "EXIF 6 is a quarter turn clockwise"
    );

    let (width, height, bgra) = first_frame(&path);
    assert_eq!(
        [width, height],
        [32, 64],
        "the decoded frame is the picture as it is shown"
    );
    assert_eq!(
        corners(width, height, &bgra),
        ["yellow", "red", "green", "blue"],
        "a quarter turn clockwise puts the stored bottom-left at the new top-left: \
         the file holds {STORED:?} top-left first"
    );
}

/// The other quarter turns, so a fix that turned *something* rather than
/// turning *right* cannot pass: EXIF 3 is 180 (opposite corners swap), 8 is a
/// quarter turn anticlockwise (the other way round), and 1 states no turn.
///
/// Each is asserted against the corners its own turn produces, indexed off the
/// stored layout rather than written out, so the expectation cannot drift away
/// from the fixture it describes.
#[test]
fn every_quarter_turn_is_made_in_its_own_direction() {
    pin_software();
    // Which stored corner each read corner is, and whether the axes swap with
    // the turn. A clockwise quarter turn walks the ring one place on, so the
    // read corner at index `i` is the stored one at `i + 3`; anticlockwise is
    // the other way round, and 180 is two places either way.
    for (exif, swaps, ring) in [
        (3u8, false, [2usize, 3, 0, 1]),
        (8, true, [1, 2, 3, 0]),
        (1, false, [0, 1, 2, 3]),
    ] {
        let (_scratch, path) = fixture(exif);
        let (stored_w, stored_h) = (64u32, 32u32);
        let (want_w, want_h) = match swaps {
            true => (stored_h, stored_w),
            false => (stored_w, stored_h),
        };
        assert_eq!(
            image_size(&path).expect("the header answers"),
            (want_w, want_h),
            "EXIF {exif}: the shown pair is the stored one, turned"
        );

        let (width, height, bgra) = first_frame(&path);
        assert_eq!(
            [width, height],
            [want_w, want_h],
            "EXIF {exif}: the frame is the picture as it is shown"
        );
        let want: Vec<&str> = ring.iter().map(|i| STORED[*i]).collect();
        assert_eq!(
            corners(width, height, &bgra),
            want[..],
            "EXIF {exif}: the corners read the stored ones in the order {ring:?}"
        );
    }
}

/// A mirroring is not a turn, and this engine has no flip anywhere in its
/// pipeline -- not in the still's decode, not in `Render`, not in an export's
/// `place_picture`. So EXIF 2, 4, 5 and 7 are refused **by name** at the door,
/// the way a display matrix that is not a quarter turn is
/// (`engine::UnsupportedRotation`): a mirrored photo shown the wrong way round
/// is a wrong picture, not a missing feature, and quietly treating a mirror as
/// a rotation is exactly the bug this door exists to stop.
///
/// All four values are checked, so a fix that refused only the first mirror is
/// caught.
#[test]
fn a_mirrored_photo_is_refused_by_name() {
    pin_software();
    for exif in [2u8, 4, 5, 7] {
        let (_scratch, path) = fixture(exif);
        let error = match image_orientation(&path) {
            Ok(rotation) => {
                panic!("EXIF {exif} is a mirror, not a turn, and must not read as {rotation:?}")
            }
            Err(error) => error,
        };
        assert!(
            engine::UnsupportedStillOrientation::is_it(&error),
            "EXIF {exif}: the refusal is the named one, not a generic failure: {error}"
        );
        let said = error.to_string();
        assert!(
            said.contains(&exif.to_string()) && said.contains("mirror"),
            "EXIF {exif}: the refusal names what it cannot make: {said}"
        );
        // ...and the door that places the picture refuses it too, rather than
        // showing the file the wrong way round.
        let error = match PlaybackSession::open(&path) {
            Ok(_) => panic!("EXIF {exif} must not open as a timeline"),
            Err(error) => error,
        };
        assert!(
            engine::UnsupportedStillOrientation::is_it(&error),
            "EXIF {exif}: opening the timeline refuses it by name too: {error}"
        );
    }
}

/// A picture that states no turn is the whole of an ordinary library, and it
/// must come out the way it always did: the turn is a branch on a bool, and
/// this is the assertion that it stays one.
#[test]
fn an_upright_photo_is_untouched_by_the_turn_door() {
    pin_software();
    let (_scratch, path) = fixture(1);
    assert_eq!(
        image_size(&path).expect("the header answers"),
        (64, 32),
        "a picture that states no turn is its own size"
    );
    let (width, height, bgra) = first_frame(&path);
    assert_eq!([width, height], [64, 32]);
    assert_eq!(
        corners(width, height, &bgra),
        STORED,
        "no turn means the stored quadrants, in the stored order"
    );
}

/// The export, which is the door the turn was most expensive to get wrong: it
/// is a re-encode of the *coded* planes, so a still whose turn was applied
/// only on the preview side would be written out sideways and would then play
/// sideways in every player that is not this one, forever, with no matrix left
/// to correct it.
///
/// The written file must therefore be portrait **and** state no matrix of its
/// own -- a file that kept the landscape pixels and declared a turn would
/// satisfy neither half.
#[test]
fn an_export_of_a_turned_photo_is_upright_and_portrait() {
    pin_software();
    let (_scratch, path) = fixture(6);
    let mut session = PlaybackSession::open(&path).expect("open the photo");
    session.pause();
    assert_eq!(
        [session.meta().width, session.meta().height],
        [32, 64],
        "the timeline is the displayed shape, not the stored one"
    );

    let out = engine::scratch::Scratch::file("ve_exif_export", "mp4");
    let handle = session.export_to_with(&out, &engine::export::ExportSettings::default());
    let started = std::time::Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the export did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    handle
        .result()
        .expect("a finished export has an outcome")
        .expect("the export succeeded");

    let (meta, _) = engine::demux::Demuxer::open(&out).expect("reopen the export");
    assert_eq!(
        [meta.width, meta.height],
        [32, 64],
        "the written file is portrait"
    );
    assert_eq!(
        meta.rotation,
        Rotation::None,
        "and states no matrix of its own: the turn is in the pixels"
    );
    let (width, height, bgra) = first_frame(&out);
    assert_eq!([width, height], [32, 64]);
    assert_eq!(
        corners(width, height, &bgra),
        ["yellow", "red", "green", "blue"],
        "the exported pixels are the ones the preview showed"
    );
}
