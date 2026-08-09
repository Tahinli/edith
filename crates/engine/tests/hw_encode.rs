//! Hardware encode checks. These need a built `libengine_hw.so` plus a working
//! VA-API driver with an H.264 encode entrypoint, so they are `#[ignore]`d by
//! default. Run them with:
//!
//! ```text
//! cargo build --workspace --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test hw_encode -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The round trip goes out through the plugin and back in through the *software*
//! decoder (`rusty_h264` directly, not `DecodeSession`), so a bug on one side
//! cannot cancel out a bug on the other.

use std::time::{Duration, Instant};

use engine::hw::{HwEncoder, HwSession};

const FPS: u32 = 30;
const BITRATE: u64 = 4_000_000;

/// A moving diagonal gradient: something that actually costs bits, so the
/// encoder cannot make the whole test pass by emitting skip frames.
fn synthetic(index: u32, width: usize, height: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut y = vec![0u8; width * height];
    for row in 0..height {
        for col in 0..width {
            y[row * width + col] = (row + col + index as usize * 5) as u8;
        }
    }
    let u = vec![64u8.wrapping_add(index as u8); cw * ch];
    let v = vec![192u8.wrapping_sub(index as u8); cw * ch];
    (y, u, v)
}

/// Encodes `count` synthetic pictures and returns the whole Annex-B stream plus
/// the number of access units the plugin handed back.
fn encode(width: u32, height: u32, count: u32) -> Option<(Vec<u8>, u32)> {
    let mut encoder = HwEncoder::open(width, height, FPS, 1, BITRATE)?;
    let mut stream = Vec::new();
    let mut units = 0;
    // Timed around the plugin call only: building the synthetic picture is not
    // part of what an export would pay.
    let mut spent = Duration::ZERO;
    for index in 0..count {
        let (y, u, v) = synthetic(index, width as usize, height as usize);
        let started = Instant::now();
        let coded = encoder
            .encode(&y, &u, &v, width, height, false)
            .expect("encode");
        if let Some(au) = coded {
            stream.extend_from_slice(au);
            units += 1;
        }
        spent += started.elapsed();
    }
    while let Some(au) = encoder.drain().expect("drain") {
        stream.extend_from_slice(au);
        units += 1;
    }
    let per_frame = spent.as_secs_f64() * 1000.0 / count as f64;
    println!("{width}x{height}: {count} frames, {units} AUs, {per_frame:.2} ms/frame");
    Some((stream, units))
}

fn decode(stream: &[u8]) -> Vec<rusty_h264::YuvFrame> {
    rusty_h264::Decoder::new()
        .decode_stream(stream)
        .expect("software decode of the hardware-encoded stream")
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y) as f64)
        .sum::<f64>()
        / a.len() as f64
}

#[test]
#[ignore]
fn round_trips_sixty_frames() {
    let (width, height) = (1280u32, 720u32);
    let (stream, units) =
        encode(width, height, 60).expect("no hardware encode plugin/driver available");
    assert_eq!(units, 60, "one access unit per fed picture");

    let frames = decode(&stream);
    assert_eq!(frames.len(), 60, "decoded picture count");
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            (frame.width, frame.height),
            (width as usize, height as usize),
            "frame {index} dimensions"
        );
    }

    // The pictures have to survive the I420 -> NV12 upload, not merely exist:
    // a dropped plane or a swapped U/V would sail past a count-only check.
    // Tolerances are generous because this is lossy CBR, but nowhere near the
    // ~64 a U/V swap costs on these chroma values.
    for index in [0usize, 30, 59] {
        let (y, u, v) = synthetic(index as u32, width as usize, height as usize);
        let frame = &frames[index];
        for (plane, (got, want)) in [(&frame.y, &y), (&frame.u, &u), (&frame.v, &v)]
            .iter()
            .enumerate()
        {
            let diff = mean_abs_diff(got, want);
            println!("frame {index} plane {plane}: mean abs diff {diff:.2}");
            assert!(
                diff < 6.0,
                "frame {index} plane {plane} drifted by {diff:.2}"
            );
        }
    }
}

/// 322x242 is even (NV12 needs that) but neither dimension is a multiple of 16,
/// so the surface is padded and the encoded stream has to crop it back. This is
/// the 1080p case in miniature: 1080 pads to 1088.
#[test]
#[ignore]
fn round_trips_unaligned_dimensions() {
    let (width, height) = (322u32, 242u32);
    let Some((stream, units)) = encode(width, height, 10) else {
        println!("driver refused {width}x{height}, software encode would take over -- skipping");
        return;
    };
    assert_eq!(units, 10);

    let frames = decode(&stream);
    assert_eq!(frames.len(), 10);
    assert_eq!(
        (frames[0].width, frames[0].height),
        (width as usize, height as usize),
        "padding must be cropped away again"
    );
}

/// Decode and encode resolve through two independent symbol tables, so an
/// encoder that cannot load must not cost us the decoder (and vice versa).
///
/// This is the positive half only: the negative half -- a plugin built without
/// `vh_enc_*` -- needs a second build of the `.so` and belongs to the slice's
/// verifier, not here.
#[test]
#[ignore]
fn decode_and_encode_load_independently() {
    let asset =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_baseline.mp4");
    assert!(
        HwEncoder::open(640, 480, FPS, 1, BITRATE).is_some(),
        "encode table"
    );
    assert!(HwSession::open(&asset).is_some(), "decode table");
    // Order reversed: neither table's `OnceLock` may have primed the other.
    assert!(HwSession::open(&asset).is_some());
    assert!(HwEncoder::open(640, 480, FPS, 1, BITRATE).is_some());
}
