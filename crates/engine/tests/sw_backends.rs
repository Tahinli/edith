//! The H.264 software decode seat's witnesses, after the swap that made
//! `ec-h264` -- the in-project decoder -- the seat. The independent one is
//! ffmpeg: 8-bit H.264 decode is bit-exact by conformance, so a correct decoder
//! handed a file must return the pictures the outside decoder returns, byte for
//! byte. (The witness these tests were first written against, `rusty_h264`,
//! retired with the crate on 2026-09-12 when the encode seat moved too; ffmpeg
//! took the outside role.) The seat's own two output orders must also agree:
//! display order is a reordering, not a rewrite.
//!
//! ```text
//! cargo test -p engine --test sw_backends -- --test-threads=1
//! ```

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
use ec_core::{Packet, TimeBase};
use ec_h264::H264Decoder;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Every access unit of the file, in container (decode) order.
fn access_units(path: &Path) -> Vec<Vec<u8>> {
    let (_, mut demuxer) = engine::demux::Demuxer::open(path).expect("open");
    let mut aus = Vec::new();
    while let Ok(Some(au)) = demuxer.next_access_unit() {
        aus.push(au);
    }
    assert!(!aus.is_empty(), "{}: no access units", path.display());
    aus
}

/// One decoded picture as plain planes, whatever the decoder calls them.
struct Picture {
    width: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Picture {
    fn hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.width.hash(&mut hasher);
        self.y.hash(&mut hasher);
        self.u.hash(&mut hasher);
        self.v.hash(&mut hasher);
        hasher.finish()
    }
}

/// `ec-h264` in the requested output order, drained to end of stream.
fn decode_ec(aus: &[Vec<u8>], order: ec_h264::OutputOrder) -> Vec<Picture> {
    let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).expect("ec decoder");
    decoder.set_output_order(order);
    let mut pictures = Vec::new();
    for au in aus {
        decoder
            .send_packet(&Packet::new(0, TimeBase::new(1, 1), au.as_slice()))
            .expect("ec decode");
        while let Ok(frame) = decoder.receive_frame() {
            let ec_core::frame::Frame::Video(pic) = frame else {
                continue;
            };
            let [y, u, v] = [&pic.planes[0], &pic.planes[1], &pic.planes[2]];
            pictures.push(Picture {
                width: pic.width as usize,
                y: y.data[..].to_vec(),
                u: u.data[..].to_vec(),
                v: v.data[..].to_vec(),
            });
        }
    }
    decoder.flush().expect("ec flush");
    while let Ok(frame) = decoder.receive_frame() {
        let ec_core::frame::Frame::Video(pic) = frame else {
            continue;
        };
        let [y, u, v] = [&pic.planes[0], &pic.planes[1], &pic.planes[2]];
        pictures.push(Picture {
            width: pic.width as usize,
            y: y.data[..].to_vec(),
            u: u.data[..].to_vec(),
            v: v.data[..].to_vec(),
        });
    }
    pictures
}

/// Every picture ffmpeg releases for the file, in display order, as I420
/// planes. `None` where ffmpeg is not installed -- the same skip the export
/// suite's `ffmpeg_complaints` takes.
fn decode_ffmpeg(path: &Path) -> Option<Vec<Picture>> {
    let installed = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    if !installed {
        return None;
    }
    let (meta, _) = engine::demux::Demuxer::open(path).expect("open");
    let (w, h) = (meta.width as usize, meta.height as usize);
    let frame = w * h + 2 * (w / 2) * (h / 2);
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg read {} and failed: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = out.stdout;
    assert_eq!(raw.len() % frame, 0, "ffmpeg handed back a partial frame");
    Some(
        raw.chunks_exact(frame)
            .map(|data| {
                let (luma, rest) = data.split_at(w * h);
                let (cb, cr) = rest.split_at((w / 2) * (h / 2));
                Picture {
                    width: w,
                    y: luma.to_vec(),
                    u: cb.to_vec(),
                    v: cr.to_vec(),
                }
            })
            .collect(),
    )
}

fn assert_same_pictures(a: &Picture, b: &Picture, what: &str, index: usize) {
    assert_eq!(a.width, b.width, "{what}: picture {index} width");
    assert_eq!(a.y, b.y, "{what}: picture {index} luma");
    assert_eq!(a.u, b.u, "{what}: picture {index} cb");
    assert_eq!(a.v, b.v, "{what}: picture {index} cr");
}

/// The seat against the outside decoder, byte for byte, on both a CAVLC
/// Baseline and a CABAC High stream with B pictures. Display order on both
/// sides: ffmpeg releases pictures as they play, and so does the seat.
#[test]
fn sw_decode_matches_ffmpeg_byte_for_byte() {
    for name in ["test_baseline.mp4", "test_high.mp4"] {
        let path = asset(name);
        let Some(want) = decode_ffmpeg(&path) else {
            eprintln!("no ffmpeg: skipping the outside decoder's word on {name}");
            continue;
        };
        let got = decode_ec(&access_units(&path), ec_h264::OutputOrder::Display);
        assert_eq!(got.len(), want.len(), "{name}: picture count");
        for (i, (a, b)) in got.iter().zip(&want).enumerate() {
            assert_same_pictures(a, b, name, i);
        }
        eprintln!("{name}: {} pictures agree byte for byte with ffmpeg", got.len());
    }
}

/// Display order is a reordering, not a rewrite: the same multiset of
/// pictures the decode-order run releases, possibly in a different sequence.
/// On the B-picture fixture the sequences genuinely differ -- the reordering
/// is the point of the DPB -- and on the Baseline fixture they cannot (no
/// B pictures, nothing to reorder).
#[test]
fn display_order_releases_the_same_pictures() {
    for name in ["test_baseline.mp4", "test_high.mp4"] {
        let aus = access_units(&asset(name));
        let decode_order = decode_ec(&aus, ec_h264::OutputOrder::Decode);
        let display_order = decode_ec(&aus, ec_h264::OutputOrder::Display);
        assert_eq!(decode_order.len(), display_order.len(), "{name}: count");
        let mut want: Vec<u64> = decode_order.iter().map(Picture::hash).collect();
        let mut got: Vec<u64> = display_order.iter().map(Picture::hash).collect();
        want.sort_unstable();
        got.sort_unstable();
        assert_eq!(want, got, "{name}: the released pictures differ");

        let reordered = decode_order
            .iter()
            .zip(&display_order)
            .any(|(a, b)| a.hash() != b.hash());
        if name == "test_high.mp4" {
            assert!(
                reordered,
                "test_high.mp4 has B pictures: display order must differ \
                 from decode order somewhere, or the reorder is not engaged"
            );
        } else {
            assert!(
                !reordered,
                "a Baseline stream has nothing to reorder: the sequences \
                 must be identical"
            );
        }
        eprintln!(
            "{name}: display order releases the same {} pictures",
            got.len()
        );
    }
}

/// The seat, wired through `DecodeSession`, labels every picture with its
/// timeline position: a full run to EOF delivers `0..frame_count` in order on
/// a B-picture stream -- the contract the transport reads (`frame.index` is a
/// timeline index) and the one the hardware backend already kept.
#[test]
fn the_software_seat_labels_the_timeline_in_display_order() {
    // Pin the software seat: on a box with the plugin this test is about the
    // decoder under swap, not the driver. SAFETY: single-threaded by the
    // documented invocation, and no other test in this binary reads the env.
    unsafe { std::env::set_var("VE_SW", "1") };

    let path = asset("test_high.mp4");
    let (meta, rx) = engine::DecodeSession::open(&path).expect("open");
    let indices: Vec<u32> = rx.into_iter().map(|f| f.index).collect();
    let want: Vec<u32> = (0..u32::try_from(meta.frame_count).expect("frame count")).collect();
    assert_eq!(indices, want, "frames must arrive 0..n in display order");
}
