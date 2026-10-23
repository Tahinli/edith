//! Parity of the decoding path against a snapshot. Every fixture in
//! `tests/data` and the Matroska files in `assets/` is read through
//! [`super::parse_file`] and [`super::cues_of`], and each cue, each picture and
//! each decoded canvas is written out and compared with the checked-in
//! `tests/data/subtitle_parity.txt`.
//!
//! That snapshot was taken from the oxideav path this module used to run beside
//! the `ec-*` one, before the oxideav crates were removed, so the snapshot is
//! the old behaviour and any drift from it is a failure. Regenerate it only with
//! `EC_WRITE_PARITY_SNAPSHOT=1` and `--nocapture`, and only when a difference
//! is intended.

use std::path::{Path, PathBuf};

use super::{Cue, CueImage, parse_file};
use crate::scratch::Scratch;

/// 64-bit FNV-1a: a hash the snapshot can carry, stable across toolchains.
/// The bytes a picture was made from, whatever its format: the display set or the unit.
fn source_bytes(source: &super::Source) -> &[u8] {
    match source {
        super::Source::Pgs(set) | super::Source::Dvb(set) => set,
        super::Source::Dvd { unit, .. } => unit,
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// One fixture's result as the snapshot writes it: every cue's times and text,
/// and for a picture the hash of its display set and of its canvas, with the
/// number of pixels the canvas paints.
fn describe(
    label: &str,
    cues: &Result<Vec<Cue>, String>,
    rgba: &dyn Fn(&CueImage) -> Option<Vec<u8>>,
) -> String {
    let cues = match cues {
        Ok(cues) => cues,
        Err(why) => return format!("{label}: error {why:?}\n"),
    };
    let mut out = format!("{label}: {} cues\n", cues.len());
    for cue in cues {
        out += &format!("  {} {} {:?}\n", cue.start_us, cue.end_us, cue.text);
        if let Some(image) = &cue.image {
            let canvas = match rgba(image) {
                Some(px) => format!(
                    "fnv {:016x} lit {}",
                    fnv(&px),
                    px.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count()
                ),
                None => "none".to_owned(),
            };
            out += &format!(
                "  image {}x{} set {:016x} canvas {canvas}\n",
                image.width,
                image.height,
                fnv(source_bytes(&image.set))
            );
        }
    }
    out
}

/// A Matroska case: every track of `path`, its cues read off the file and each
/// picture decoded to its canvas.
fn mkv_case(path: &Path, label: &str, out: &mut Vec<String>) {
    let tracks = crate::demux::matroska_subtitles(path).expect("the walk does not fail");
    for mut track in tracks {
        let name = format!("{label} track {} {}", track.number, track.codec);
        let cues = super::cues_of(&mut track);
        out.push(describe(&name, &cues, &|i| i.rgba()));
    }
}

/// The synthetic PGS fixture from `tests/subtitles.rs`: a 2x2 picture on an 8x4
/// canvas, one display set that paints and one that erases it.
fn pgs_segment(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn pgs_display_set() -> Vec<u8> {
    #[rustfmt::skip]
    let pcs = [
        0, 8, 0, 4, 0x10, 0, 0, 0x80, 0, 0, 1,
        0, 0, 0, 0, 0, 1, 0, 1,
    ];
    let pds = [0, 0, 1, 235, 128, 128, 255];
    let rle = [0x00, 0x82, 0x01, 0x00, 0x00, 0x00, 0x82, 0x01, 0x00, 0x00];
    let mut ods = vec![0, 0, 0, 0xC0];
    ods.extend_from_slice(&[0, 0, (4 + rle.len()) as u8, 0, 2, 0, 2]);
    ods.extend_from_slice(&rle);
    let mut set = pgs_segment(0x16, &pcs);
    set.extend(pgs_segment(0x14, &pds));
    set.extend(pgs_segment(0x15, &ods));
    set.extend(pgs_segment(0x80, &[]));
    set
}

fn pgs_erase() -> Vec<u8> {
    let mut set = pgs_segment(0x16, &[0, 8, 0, 4, 0x10, 0, 1, 0, 0, 0, 0]);
    set.extend(pgs_segment(0x80, &[]));
    set
}

fn element(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    let mut len = (body.len() as u64).to_be_bytes();
    len[0] |= 0x01;
    out.extend_from_slice(&len);
    out.extend_from_slice(body);
    out
}

/// A Matroska file with one PGS track: `first` at 500 ms, the erase at 2 s.
fn pgs_matroska(first: Vec<u8>) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend(element(&[0xD7], &[1])); // TrackNumber
    entry.extend(element(&[0x83], &[0x11])); // TrackType: subtitle
    entry.extend(element(&[0x86], b"S_HDMV/PGS")); // CodecID
    entry.extend(element(&[0x22, 0xB5, 0x9C], b"eng"));
    let tracks = element(&[0xAE], &entry);
    let block = |at: i16, data: Vec<u8>| {
        let mut body = vec![0x81, (at >> 8) as u8, at as u8, 0x00];
        body.extend(data);
        element(&[0xA3], &body)
    };
    let mut cluster = element(&[0xE7], &[0x00]);
    cluster.extend(block(500, first));
    cluster.extend(block(2000, pgs_erase()));
    let mut segment = element(&[0x16, 0x54, 0xAE, 0x6B], &tracks);
    segment.extend(element(&[0x1F, 0x43, 0xB6, 0x75], &cluster));
    element(&[0x18, 0x53, 0x80, 0x67], &segment)
}

/// Every fixture rendered through the decoding path: the checked-in fixtures
/// and inline ones that reach the markup they do not (a VTT, an ASS with
/// override tags, an SRT with markup and a line break), then the Matroska files.
fn render() -> String {
    let dir = Scratch::dir("ve_subs_parity");
    let data = |name: &str| -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)
    };
    let asset = |name: &str| -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    };
    let mut out = Vec::new();

    let mut file_case = |name: &str, path: PathBuf| {
        let got = parse_file(&path).map_err(|e| e.to_string());
        out.push(describe(name, &got, &|i| i.rgba()));
    };
    file_case("test_subs.srt", data("test_subs.srt"));
    file_case("test_subs.ass", data("test_subs.ass"));
    let inline: [(&str, &str); 3] = [
        (
            "inline.vtt",
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.500 line:10%\n<v Ann>Hello <i>there</i>\n\n\
             00:00:03.000 --> 00:00:04.000\nsecond\nline\n",
        ),
        (
            "inline.ass",
            "[Script Info]\nScriptType: v4.00+\nPlayResX: 1280\nPlayResY: 720\n\n\
             [V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,40\n\n\
             [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
             Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an8}top {\\i1}slant{\\i0}\\Nnext\n\
             Dialogue: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,plain {\\b1}bold{\\b0}\n",
        ),
        (
            "inline.srt",
            "1\n00:00:01,000 --> 00:00:02,000\n<i>tilted</i> and <font color=\"#ff0000\">red</font>\nsecond\n",
        ),
    ];
    for (name, body) in inline {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write the fixture");
        file_case(name, path);
    }

    for name in ["test_subs.mkv", "test_subs.mks", "test_subs_bcp47.mkv"] {
        mkv_case(&asset(name), name, &mut out);
    }
    let pgs = dir.join("pgs.mkv");
    std::fs::write(&pgs, pgs_matroska([pgs_display_set(), pgs_erase()].concat()))
        .expect("write the pgs fixture");
    mkv_case(&pgs, "pgs.mkv", &mut out);

    out.concat()
}

/// The one difference found between the two paths, pinned on its own so the
/// parity test above stays exact: an ASS drawing (`{\p1}` ... `{\p0}`) is
/// shapes, not words. The oxideav path read the drawing commands out as text;
/// `ec-ass` keeps them as `Segment::Raw`, which [`ec_subs::plain_text`] drops.
#[test]
fn an_ass_drawing_is_shapes_and_not_words() {
    let dir = Scratch::dir("ve_subs_parity_drawing");
    let path = dir.join("drawing.ass");
    std::fs::write(
        &path,
        "[Script Info]\nScriptType: v4.00+\n\n[Events]\n\
         Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
         Dialogue: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,{\\p1}m 0 0 l 10 10{\\p0}words\n",
    )
    .expect("write the fixture");
    let cues = parse_file(&path).expect("the ass parses");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].text, "words");
}

/// The decoding path matches the checked-in snapshot on every fixture. The
/// snapshot was written by the oxideav path before it was removed, so this is
/// still the old behaviour, to the byte, held in place.
#[test]
fn the_decoding_path_matches_the_snapshot_taken_from_the_oxideav_path() {
    let got = render();
    if std::env::var_os("EC_WRITE_PARITY_SNAPSHOT").is_some() {
        // The VPS keeps the tree it ran in, so the snapshot comes back on
        // stdout, between markers, and is checked in by hand.
        println!("SNAPSHOT-BEGIN\n{got}SNAPSHOT-END");
    }
    assert_eq!(got, include_str!("../../tests/data/subtitle_parity.txt"));
}
