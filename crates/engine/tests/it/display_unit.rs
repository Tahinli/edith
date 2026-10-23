//! Matroska `DisplayUnit` 0: the `Display*` pair is in *pixels*, so a pair
//! beside the coded size is the stretch, not a crop.
//!
//! The Matroska spec is explicit that unit 0 means the `DisplayWidth` and
//! `DisplayHeight` pair *is* a pixel size. So `DisplayWidth=1920`,
//! `DisplayHeight=1080` beside a `PixelWidth=1440`, `PixelHeight=1080` track
//! is exactly the 4/3 anamorphic stretch -- the same picture
//! `test_anamorphic.mkv` states the other two ways round, in SAR (4/3) and in
//! DAR (16/9).
//!
//! No fixture stated it that way until this one: `scripts/gen_fixtures.sh`
//! produces unit 3 (ffmpeg spells the aspect as a DAR), so the unit-0 arm of
//! the reader had never been seen by a test, and it read as square -- the
//! same answer as a file that says nothing at all. The picture was shown at
//! 1440x1080 instead of 1920x1080, and nothing said so.
//!
//! The fixture is patched here from `test_anamorphic.mkv` rather than
//! committed as a binary: the `Video` element keeps its exact byte length, so
//! every absolute offset in the file -- the SeekHead and every CueCluster-
//! Position -- stays correct and the copy seeks exactly as the original does.
//!
//! ```text
//! cargo test -p engine --test display_unit -- --test-threads=1
//! ```

use std::path::{Path, PathBuf};

use engine::demux::{Demuxer, PixelAspect};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("edith-display-unit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp directory for the patched fixture");
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The bytes of the `Video` element of `test_anamorphic.mkv`, as ffmpeg's
/// Matroska muxer wrote them: a 1440x1080 track with its aspect stated as a
/// 16/9 display aspect ratio.
const VIDEO_UNIT_3: &str = concat!(
    "e09e", // Video, 30 bytes
    "b08205a0", // PixelWidth  1440
    "ba820438", // PixelHeight 1080
    "9a8102", // FlagInterlaced
    "54b08110", // DisplayWidth  16
    "54ba8109", // DisplayHeight  9
    "54b28103", // DisplayUnit   3 -- a display aspect ratio
    "55b084", "55b98101" // Colour { MatrixCoefficients 1 }
);

/// The same 30 bytes with the aspect stated the third way Matroska allows:
/// unit 0, where the `Display*` pair is in pixels and so *is* the drawn
/// size. 1920x1080 over a 1440x1080 coded track is the same 4/3 stretch.
///
/// Byte-for-byte the same length as [`VIDEO_UNIT_3`], which is what keeps
/// the rest of the file's offsets honest: the `Display*` values grow from
/// one-byte to two-byte payloads (+3 bytes with the wider unit encoding),
/// `FlagInterlaced` is dropped (-3), and `MatrixCoefficients` is restated
/// over two bytes instead of one (+1) inside the `Colour` element.
const VIDEO_UNIT_0: &str = concat!(
    "e09e", // Video, 30 bytes -- unchanged
    "b08205a0", // PixelWidth  1440, unchanged: the samples are still these
    "ba820438", // PixelHeight 1080, unchanged
    "54b0820780", // DisplayWidth  1920 -- in *pixels*
    "54ba820438", // DisplayHeight 1080 -- in *pixels*
    "54b28100", // DisplayUnit   0 -- pixels
    "55b085", "55b9820001" // Colour { MatrixCoefficients 1 }
);

/// The same `DisplayUnit 0` element with a different `Display*` pixel pair,
/// for the ratios whose reduced form is not a small pair of integers.
fn video_unit_0(display_width: u16, display_height: u16) -> String {
    // Both `Display*` values are two-byte payloads, so a width past 0x7FFF
    // would need three and change the element's length. The lengths here are
    // all inside that range, which is what keeps every offset in the file
    // honest.
    format!(
        "e09e\
         b08205a0\
         ba820438\
         54b082{display_width:04x}\
         54ba82{display_height:04x}\
         54b28100\
         55b085\
         55b9820001"
    )
}
/// A `DisplayUnit 3` (DAR) `Video` element: the coded size and the display
/// aspect ratio, both length-preserving.
fn video_unit_3(dar_width: u16, dar_height: u16, coded_width: u16, coded_height: u16) -> String {
    format!(
        "e09e\
         b082{coded_width:04x}\
         ba82{coded_height:04x}\
         9a8102\
         54b081{dar_width:02x}\
         54ba81{dar_height:02x}\
         54b28103\
         55b084\
         55b98101"
    )
}

fn with_video_element(source: &Path, into: &Path, element: &str) {
    let from = hex(VIDEO_UNIT_3);
    let to = hex(element);
    assert_eq!(
        from.len(),
        to.len(),
        "a length-preserving patch: {element} is {} bytes, {} are needed",
        to.len(),
        from.len()
    );
    let bytes = std::fs::read(source).expect("read the source fixture");
    let at = bytes
        .windows(from.len())
        .position(|window| window == from)
        .unwrap_or_else(|| panic!("the fixture carries the expected {VIDEO_UNIT_3} element"));
    let mut patched = bytes.clone();
    patched[at..at + from.len()].copy_from_slice(&to);
    std::fs::write(into, patched).expect("write the patched fixture");
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).expect("hex in the fixture"))
        .collect()
}

/// The reduced form of a ratio, so a ratio stated unreduced (the DAR arm
/// multiplies through by the coded size) compares equal to its simplest
/// spelling -- the same comparison the anamorphic suite makes.
fn reduced(aspect: PixelAspect) -> (u32, u32) {
    let gcd = |mut a: u32, mut b: u32| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a.max(1)
    };
    let g = gcd(aspect.h, aspect.v);
    (aspect.h / g, aspect.v / g)
}

/// The three spellings of one anamorphic picture must name one picture.
#[test]
fn a_display_unit_of_zero_states_the_stretch_in_pixels() {
    let scratch = Scratch::new("pixels");
    let unit0 = scratch.path("display_unit0.mkv");
    with_video_element(&asset("test_anamorphic.mkv"), &unit0, VIDEO_UNIT_0);

    // The patch landed, and it landed on a file the demuxer opens.
    let patched = std::fs::read(&unit0).expect("read back the patched fixture");
    let original = std::fs::read(asset("test_anamorphic.mkv")).expect("read the original");
    assert_eq!(
        patched.len(),
        original.len(),
        "the patch preserves the file length, so no offset in it moved"
    );
    let wanted = hex(VIDEO_UNIT_0);
    assert_eq!(
        patched
            .windows(wanted.len())
            .position(|window| window == wanted),
        Some(369),
        "the DisplayUnit-0 element is where it was aimed"
    );

    let (meta, _) = Demuxer::open(&unit0).expect("open the DisplayUnit-0 fixture");

    // The samples are stored at the coded size, exactly as before the patch.
    assert_eq!(
        (meta.coded_width, meta.coded_height),
        (1440, 1080),
        "unit 0 changes what the file *says* about the picture, not how it is stored"
    );

    // Unit 0 means the pair IS a pixel size, so 1920x1080 beside a 1440x1080
    // coded track is the 4/3 stretch -- and this is the whole defect: read as
    // square, the picture was drawn at 1440x1080, a quarter-width picture of
    // the same frame, with nothing to say so.
    assert!(
        !meta.pixel_aspect.is_square(),
        "DisplayUnit 0 with a Display pair beside the coded one is a stretch, \
         not a crop: got square ({:?})",
        meta.pixel_aspect
    );
    assert_eq!(
        (meta.width, meta.height),
        (1920, 1080),
        "the Display pair is the drawn size: 1920x1080 over a 1440x1080 coded track"
    );

    // ...and it is the *same* ratio the other two spellings name, so all three
    // fixtures of one source agree on one drawn picture. Compared reduced,
    // because a ratio is named by the picture and not by how it was spelled:
    // the DAR arm multiplies through by the coded size and says 2073600 /
    // 1555200 where the SAR arm says 4 / 3.
    assert_eq!(
        reduced(meta.pixel_aspect),
        (4, 3),
        "1920x1080 over a 1440x1080 coded track is the same 4/3 the SAR and \
         DAR spellings name"
    );
    for sibling in ["test_anamorphic.mp4", "test_anamorphic.mkv"] {
        let (other, _) = Demuxer::open(&asset(sibling)).expect("open the sibling fixture");
        assert_eq!(
            (other.width, other.height),
            (meta.width, meta.height),
            "{sibling} states the same picture the DisplayUnit spelling states"
        );
    }
}

/// Units 1 and 2 are physical sizes -- centimetres, inches -- which a picture
/// this engine shows on a screen cannot mean anything by. They stay square,
/// and saying so is the deliberate answer rather than an accident of the
/// match falling through.
#[test]
fn a_physical_display_unit_is_read_as_square() {
    let scratch = Scratch::new("physical");
    for (unit, tag) in [(1u8, "cm"), (2u8, "in")] {
        let element = VIDEO_UNIT_0.replacen("54b28100", &format!("54b2810{unit}"), 1);
        let into = scratch.path(&format!("display_unit{unit}.mkv"));
        with_video_element(&asset("test_anamorphic.mkv"), &into, &element);
        let (meta, _) = Demuxer::open(&into).expect("open the physical-unit fixture");
        assert!(
            meta.pixel_aspect.is_square(),
            "DisplayUnit {unit} ({tag}) is a physical size this engine cannot interpret, \
             so the pixels stay square: got {:?}",
            meta.pixel_aspect
        );
    }
}
/// The coded size is untrusted too, and it reaches an allocation directly:
/// `decode.rs` sizes its stretched planes as `display_width * height`. The
/// engine already has a bound for exactly this -- [`engine::MAX_RESOLUTION`],
/// 8K, the size a per-frame buffer stops being sane at -- and the demux
/// readers never applied it to a container-stated width or height.
///
/// The fixture patches the `PixelWidth`/`PixelHeight` payloads to 65535 each,
/// which is a legal two-byte EBML integer, so the element keeps its exact
/// length and the copy stays seekable.
#[test]
fn a_coded_size_past_the_resolution_bound_is_refused_not_allocated() {
    let scratch = Scratch::new("huge");
    let into = scratch.path("huge_dims.mkv");
    with_video_element(
        &asset("test_anamorphic.mkv"),
        &into,
        &VIDEO_UNIT_3
            .replacen("b08205a0", "b082ffff", 1)
            .replacen("ba820438", "ba82ffff", 1),
    );

    // Refused, and refused by name: the open fails with a message that says
    // what the file claimed and what the maximum is, rather than succeeding
    // and handing a caller a 65535x65535 to allocate.
    let Err(why) = Demuxer::open(&into) else {
        panic!("a 65535x65535 header is refused by name, never opened");
    };
    let said = why.to_string();
    assert!(
        said.contains("65535") && said.contains(&engine::MAX_RESOLUTION.to_string()),
        "the refusal names the size claimed and the maximum: {said}"
    );
    assert!(
        engine::demux::UnsupportedDisplaySize::is_it(&why),
        "and it is the named refusal a caller can match on, not a string"
    );
}

/// The bound is a ceiling on a real picture, not a floor on a file: the
/// largest size a real source states still opens, untouched.
#[test]
fn a_real_size_up_to_the_bound_still_opens() {
    let (meta, _) = Demuxer::open(&asset("test_anamorphic.mkv")).expect("open the fixture");
    assert!(
        engine::is_resolution(meta.coded_width, meta.coded_height),
        "the ordinary fixture is inside the bound, which is what keeps the \
         bound from refusing the library"
    );
}

/// The regression this arm must not carry: a `DisplayUnit 0` pair is a pixel
/// SIZE, so its *reduced* form is the size against the coded size, not a small
/// pair of integers -- and the common cinematic display ratios do not reduce
/// small. A 1920x803 scope over a 1440x1080 coded track reduces to 1440:803,
/// and 1920x1037 (1.85:1 flat) likewise.
///
/// The reduced-term bound is the right test for an mp4 `pasp`, where the pair
/// *is* the ratio, and the wrong test here: applying it made the two most
/// common letterbox ratios open square, on exactly the arm this fix newly
/// enabled. The quantity that needs bounding on this door is the resulting
/// display WIDTH, which `width_factor` already caps.
#[test]
fn a_cinematic_display_unit_zero_stretches_rather_than_reading_square() {
    let scratch = Scratch::new("cinema");
    for (dw, dh, ratio, tag) in [
        (1920u16, 803u16, "2.39:1 scope", "scope"),
        (1920, 1037, "1.85:1 flat", "flat"),
    ] {
        let into = scratch.path(&format!("{tag}.mkv"));
        with_video_element(
            &asset("test_anamorphic.mkv"),
            &into,
            &video_unit_0(dw, dh),
        );

        let (meta, _) = Demuxer::open(&into)
            .unwrap_or_else(|why| panic!("{tag} ({ratio}): open failed: {why}"));
        assert!(
            !meta.pixel_aspect.is_square(),
            "{tag} ({ratio}): a DisplayUnit 0 pixel pair of {dw}x{dh} over a \
             1440x1080 coded track is a stretch, not square: got {:?}",
            meta.pixel_aspect
        );
        // The stated DISPLAY WIDTH is the drawn width: unit 0 states it in
        // pixels, so it is taken as stated, not re-derived. The height stays
        // the coded one because a pixel aspect moves the width only in this
        // engine -- `VideoMeta::height` is the coded height for every file, by
        // design. A 1920x803 scope is therefore drawn 1920x1080 here: the
        // width is the file's own claim and the height is this engine's
        // documented model, and the two are not the same thing.
        assert_eq!(
            meta.width,
            u32::from(dw),
            "{tag} ({ratio}): the stated Display width is the drawn width"
        );
        assert_eq!(
            meta.height,
            1080,
            "{tag} ({ratio}): a pixel aspect never moves the height; the coded one stands"
        );
        assert_eq!(
            (meta.coded_width, meta.coded_height),
            (1440, 1080),
            "{tag} ({ratio}): the samples are still stored at the coded size"
        );
    }
}

/// The unit-3 arm, which is the same rule as unit 0 one arm away: a DAR is
/// not a ratio either, because bringing it onto the coded frame multiplies by
/// the coded size, and what reduces afterwards carries the *sizes*.
///
/// So the same 2.39:1 scope is judged differently by the frame it sits on,
/// which is a bound measuring the wrong thing. Over a 1920x1080 coded track a
/// 239:100 DAR derives 2151:1600 and over 3840x2160 the same -- both past any
/// term bound -- while over 1440x1080 it derives 717:400 and passes. Base
/// `9f780f7` opened all three at the widths below; the bound made the first
/// two open square, a measured regression rather than a latent gap.
///
/// The expected widths are the ones BASE produced, measured by running base
/// over these same fixtures. This is the property that matters: the arm must
/// read the DAR the way it always did, so the test pins base's answers rather
/// than a fresh computation of them.
#[test]
fn a_display_unit_three_dar_reads_the_same_on_every_coded_frame() {
    let scratch = Scratch::new("dar");
    for (coded, expected, tag) in [
        ((1920u16, 1080u16), 2582u32, "1920x1080"),  // 239:100 -> 2151:1600
        ((3840, 2160), 5162, "3840x2160"),           // 239:100 -> 2151:1600
        ((720, 576), 1376, "720x576"),               // 239:100 -> 239:125
    ] {
        let into = scratch.path(&format!("dar_{tag}.mkv"));
        with_video_element(
            &asset("test_anamorphic.mkv"),
            &into,
            &video_unit_3(239, 100, coded.0, coded.1),
        );

        let (meta, _) = Demuxer::open(&into)
            .unwrap_or_else(|why| panic!("{tag}: open failed: {why}"));
        assert_eq!(
            (meta.coded_width, meta.coded_height),
            (u32::from(coded.0), u32::from(coded.1)),
            "{tag}: the coded frame the DAR is stated over"
        );
        assert!(
            !meta.pixel_aspect.is_square(),
            "{tag}: a 239:100 DAR is a 2.39:1 stretch whatever frame it sits \
             on: got square ({:?})",
            meta.pixel_aspect
        );
        assert_eq!(
            meta.width, expected,
            "{tag}: the display width base 9f780f7 produced for this DAR"
        );
        assert_eq!(
            meta.height,
            u32::from(coded.1),
            "{tag}: a pixel aspect never moves the height; the coded one stands"
        );
    }
}

/// The term bound must survive where it belongs. The unit-4 SAR pair IS a
/// ratio, so a hostile one is still refused -- this arm is what stops the
/// split from reading as "the bound was dropped".
#[test]
fn a_hostile_sar_pair_is_still_refused_by_the_term_bound() {
    // Reduced terms far past the bound, stated as a SAR pair: four chosen
    // bytes, not a ratio.
    for hostile in [(0xFFFF_FFFFu32, 0xFFFF_FFFFu32 - 1), (1, 0x00FF_FFFF)] {
        assert_eq!(
            PixelAspect::stated(hostile.0, hostile.1),
            None,
            "{hostile:?} is chosen bytes, not a ratio, and is refused as one"
        );
    }
    // ...and a real SAR pair is still taken, so the bound is not simply
    // refusing everything on this arm.
    assert_eq!(
        PixelAspect::stated(4, 3),
        Some(PixelAspect { h: 4, v: 3 }),
        "a 4:3 SAR is a ratio and is read as one"
    );
    assert_eq!(
        PixelAspect::stated(16, 12),
        Some(PixelAspect { h: 16, v: 12 }),
        "an unreduced 4:3 spelling is judged by the ratio it names"
    );
}
