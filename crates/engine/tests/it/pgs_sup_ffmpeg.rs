//! A PGS (bitmap) subtitle that goes through ffmpeg into Matroska, read back
//! through edith's own subtitle call site ([`subtitle::of_matroska`]).
//!
//! The source is a `.sup` written here byte for byte, with the `PG` header a
//! Blu-ray `.sup` carries on every segment: one display set that paints a 2x2
//! white square on an 8x4 canvas at 500 ms, and the erase that ends it at 2 s.
//! ffmpeg reads it with its PGS demuxer and muxes it with `-c:s copy` into an
//! `.mkv`, which is what a rip looks like when it reaches edith.
//!
//! The cue the test expects is the one the display sets state: the start and
//! end in microseconds, and the picture decoded to its pixels. Skipped loudly
//! when ffmpeg is not on the box.
//!
//! ```text
//! cargo test -p engine --release --test pgs_sup_ffmpeg -- --nocapture
//! ```

use std::path::Path;
use std::process::Command;

use engine::scratch::Scratch;
use engine::subtitle;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn s(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 path")
}

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One `.sup` segment: `PG`, the presentation and decoding times on the 90 kHz
/// clock, the segment type, a big-endian size, then the body.
fn sup_segment(pts_ms: u32, kind: u8, body: &[u8]) -> Vec<u8> {
    let pts = pts_ms * 90;
    let mut out = b"PG".to_vec();
    out.extend_from_slice(&pts.to_be_bytes());
    out.extend_from_slice(&pts.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// The display set that paints a 2x2 white square at (1, 1) of an 8x4 canvas,
/// at `pts_ms`: composition, palette, the object and the end of the set.
fn display_set(pts_ms: u32) -> Vec<u8> {
    // Canvas 8x4, a frame-rate byte, composition 0, epoch start, no palette
    // update, palette 0 -- and one object, id 0, at (1, 1) with no flags.
    #[rustfmt::skip]
    let pcs = [
        0, 8, 0, 4, 0x10, 0, 0, 0x80, 0, 0, 1,
        0, 0, 0, 0, 0, 1, 0, 1,
    ];
    // Palette 0, version 0, colour 1: white, fully opaque, limited-range YCbCr.
    let pds = [0, 0, 1, 235, 128, 128, 255];
    // Two rows of "two pixels of colour 1, end of line".
    let rle = [0x00, 0x82, 0x01, 0x00, 0x00, 0x00, 0x82, 0x01, 0x00, 0x00];
    // Object 0, version 0, first and last fragment; the length of what follows;
    // the object's own 2x2 box; the run-length data.
    let mut ods = vec![0, 0, 0, 0xC0];
    ods.extend_from_slice(&[0, 0, (4 + rle.len()) as u8, 0, 2, 0, 2]);
    ods.extend_from_slice(&rle);

    let mut out = sup_segment(pts_ms, 0x16, &pcs);
    out.extend(sup_segment(pts_ms, 0x14, &pds));
    out.extend(sup_segment(pts_ms, 0x15, &ods));
    out.extend(sup_segment(pts_ms, 0x80, &[]));
    out
}

/// The display set that composes nothing: the disc clearing the screen.
fn erase(pts_ms: u32) -> Vec<u8> {
    let mut out = sup_segment(pts_ms, 0x16, &[0, 8, 0, 4, 0x10, 0, 1, 0, 0, 0, 0]);
    out.extend(sup_segment(pts_ms, 0x80, &[]));
    out
}

#[test]
fn a_sup_muxed_into_matroska_reads_back_its_picture_and_its_timing() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to mux the .sup with");
        return;
    }
    let sup = Scratch::file("ve_pgs_source", "sup");
    let mut bytes = display_set(500);
    bytes.extend(erase(2000));
    std::fs::write(&*sup, &bytes).expect("write the .sup");

    let mkv = Scratch::file("ve_pgs_in_mkv", "mkv");
    // `-copyts` keeps the times the .sup states; without it ffmpeg moves the
    // whole stream to start at zero, which is a fact about ffmpeg, not the file.
    ffmpeg(&["-copyts", "-i", s(&sup), "-c:s", "copy", s(&mkv)]);

    let tracks = subtitle::of_matroska(&mkv).expect("the mkv is walked");
    assert_eq!(tracks.len(), 1, "one PGS track: {tracks:?}");
    let track = &tracks[0];
    assert_eq!(track.refused, None, "a PGS track is read, not refused");
    assert!(track.is_bitmap(), "a PGS track is pictures");
    assert_eq!(track.cues.len(), 1, "the erase is not a cue of its own");

    let cue = &track.cues[0];
    assert_eq!(
        (cue.start_us, cue.end_us),
        (500_000, 2_000_000),
        "the picture starts where its display set says and ends at the erase"
    );
    assert_eq!(cue.text, "", "a picture has no words");

    let image = cue.image.as_ref().expect("the cue is a picture");
    assert_eq!((image.width, image.height), (8, 4), "the disc's canvas");
    let rgba = image.rgba().expect("the display set decodes");
    let at = |x: usize, y: usize| rgba[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].to_vec();
    assert_eq!(at(1, 1), vec![235, 235, 235, 255]);
    assert_eq!(at(2, 2), vec![235, 235, 235, 255]);
    assert_eq!(at(0, 0), vec![0, 0, 0, 0]);
    assert_eq!(at(3, 1), vec![0, 0, 0, 0]);
    assert_eq!(
        rgba.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count(),
        4,
        "four painted pixels and no more"
    );
}
