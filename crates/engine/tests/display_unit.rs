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
        let dir = std::env::temp_dir().join(format!("edith-display-unit-{tag}"));
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