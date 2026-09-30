//! What a container header claims is untrusted input, and the numbers it
//! claims them with reach every raster this engine sizes.
//!
//! An mp4 `pasp` box is eight bytes: two 32-bit big-endian integers, the ratio
//! one coded pixel is drawn at. Nothing in the file makes them sane. Four
//! chosen bytes are enough to make the probe report a zero width, or a width
//! that wrapped a `u32` on its way down from the multiply -- and both of those
//! go straight into the `resize` that sizes the decode worker's stretched
//! planes, before a single pixel is drawn.
//!
//! This test byte-patches a copy of the baseline fixture and opens it through
//! the ordinary door: the patched files live in a temp directory and are made
//! here, so no hostile bytes are committed to the repository.
//!
//! ```text
//! cargo test -p engine --test untrusted_header -- --test-threads=1
//! ```

use std::path::{Path, PathBuf};

use engine::demux::Demuxer;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// A temp directory that cleans itself up, so a failing test does not leave a
/// hostile mp4 lying around for the next one to trip over.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("edith-untrusted-header-{tag}"));
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

/// A copy of `source` with the eight bytes of its `pasp` payload replaced.
///
/// Returns the byte offset the payload was written at, so the test can name
/// the exact bytes it changed rather than asserting about a file whose
/// provenance is a fixture somebody might regenerate.
fn patch_pasp(source: &Path, into: &Path, h: u32, v: u32) -> usize {
    let bytes = std::fs::read(source).expect("read the source fixture");
    let at = bytes
        .windows(4)
        .position(|window| window == b"pasp")
        .expect("the fixture carries a pasp box")
        + 4; // the type is four bytes; the payload follows it
    let mut patched = bytes.clone();
    patched[at..at + 4].copy_from_slice(&h.to_be_bytes());
    patched[at + 4..at + 8].copy_from_slice(&v.to_be_bytes());
    std::fs::write(into, patched).expect("write the patched fixture");
    at
}

/// The claims an mp4 can make about its pixels that are not a picture: a zero
/// half, which divides to nothing, and a huge half, whose multiply wraps a
/// `u32` on the way down.
#[test]
fn a_pasp_box_cannot_declare_a_zero_or_wrapped_display_width() {
    // Read the untouched fixture first: the patches below are asserted
    // against *its* numbers, so a regenerated fixture that stops carrying a
    // `pasp` box fails here loudly instead of silently passing.
    let (baseline, _) = Demuxer::open(&asset("test_baseline.mp4")).expect("open the baseline");
    let coded = (baseline.coded_width, baseline.coded_height);
    assert_eq!(
        (baseline.width, baseline.height),
        coded,
        "the baseline fixture is square, so its display pair is its coded pair -- which is \
         what the patched files are asserted against"
    );
    assert!(
        baseline.pixel_aspect.is_square(),
        "the baseline fixture states 1:1, which is square"
    );

    let scratch = Scratch::new("pasp");
    for (tag, h, v) in [
        ("zero_h", 0u32, 1u32),
        ("zero_v", 1, 0),
        ("both_zero", 0, 0),
        // 1920 * 0x00FF_FFFF = 32_212_252_800, which narrowed to a u32 is
        // 2_147_481_728: a width that looks plausible in a log and cannot be
        // allocated on any machine this runs on.
        ("wrap_h", 0x00FF_FFFF, 1),
        // 1:0x00FF_FFFF is the other end: a two-column strip of a picture.
        ("wrap_v", 1, 0x00FF_FFFF),
    ] {
        let name = format!("pasp_{tag}.mp4");
        let into = scratch.path(&name);
        let at = patch_pasp(&asset("test_baseline.mp4"), &into, h, v);
        assert_eq!(
            std::fs::read(&into).unwrap()[at..at + 8],
            [h.to_be_bytes(), v.to_be_bytes()].concat(),
            "{tag}: the patch landed where it was aimed"
        );

        // The ordinary door: open the file the way a library open does.
        let (meta, _) = Demuxer::open(&into)
            .unwrap_or_else(|why| panic!("{tag} (pasp {h}:{v} at byte {at}): open failed: {why}"));

        // A header that claims an unusable ratio is read as the picture it
        // really is: the coded one, square, drawn one to one.
        assert_eq!(
            (meta.width, meta.height),
            (baseline.width, baseline.height),
            "{tag} (pasp {h}:{v}): an unusable ratio must read as square, not as a \
             zero-column or wrapped raster"
        );
        assert!(
            meta.pixel_aspect.is_square(),
            "{tag} (pasp {h}:{v}): the reported aspect must be one this engine can draw"
        );
        assert!(
            meta.width > 0 && meta.height > 0,
            "{tag} (pasp {h}:{v}): a zero dimension here is what sizes an allocation to nothing"
        );
    }
}

/// The half that matters is not the probe: it is the worker that sizes its
/// stretched planes from the same number. A decode that used to abort on a
/// capacity overflow here now returns the coded picture.
#[test]
fn a_hostile_pasp_decodes_the_coded_picture_instead_of_aborting() {
    let scratch = Scratch::new("decode");
    let into = scratch.path("pasp_wrap.mp4");
    patch_pasp(&asset("test_baseline.mp4"), &into, 0x00FF_FFFF, 1);

    let (baseline, _) = Demuxer::open(&asset("test_baseline.mp4")).expect("open the baseline");
    let coded = (baseline.width, baseline.height);
    let (meta, _) = Demuxer::open(&into).expect("a hostile header is still an openable file");
    let (_, frames, _) =
        engine::DecodeSession::open_at(&into, 0).expect("a hostile header does not stop the decoder");
    let frame = frames
        .recv()
        .expect("the first frame arrives rather than the worker aborting on its allocation");
    assert_eq!(
        frame.width,
        meta.width,
        "the decoded frame is drawn at the width the probe reported"
    );
    assert_eq!(
        (frame.width, frame.height),
        coded,
        "the coded picture, drawn square: a wrapped claim resizes nothing"
    );
}