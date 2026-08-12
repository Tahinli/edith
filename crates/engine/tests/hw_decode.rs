//! Hardware path checks. These need a built `libengine_hw.so` plus a working
//! VA-API driver, so they are `#[ignore]`d by default. Run them with:
//!
//! ```text
//! cargo build --workspace --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test hw_decode -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The comparison test pins [`DecodeSession`] to the software decoder itself
//! (it sets `VE_SW=1` in-process, hence the required `--test-threads=1`); the
//! hardware side goes through [`HwSession`] directly and can never silently be
//! a fallback.

use std::path::{Path, PathBuf};
use std::time::Instant;

use engine::DecodeSession;
use engine::colorspace::{ColorDescription, Matrix};
use engine::convert::i420_to_bgra;
use engine::demux::{Codec, Demuxer};
use engine::scratch::Scratch;
use engine::hw::HwSession;
use engine::tonemap::{ToneMapper, Transfer};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// What the decode funnel converts this file's planes in, so a conversion done
/// by hand here is the one `DecodeSession` would have done.
fn color_of(path: &Path) -> ColorDescription {
    Demuxer::open(path).expect("open for colour").0.color
}

fn open_hw(path: &Path) -> HwSession {
    HwSession::open(path).expect("no hardware decode plugin/driver available")
}

/// Decodes the whole file through the plugin, returning (frame count, frame 30 as BGRA).
fn decode_all_hw(name: &str) -> (usize, Vec<u8>) {
    let path = asset(name);
    let color = color_of(&path);
    let mut hw = open_hw(&path);
    let meta = hw.meta().expect("meta");
    assert_eq!(
        (meta.width, meta.height),
        (1280, 720),
        "{name} container dims"
    );

    let mut count = 0usize;
    let mut frame_30 = Vec::new();
    let start = Instant::now();
    while let Some((y, u, v, w, h)) = hw.next_frame().expect("hardware decode") {
        assert_eq!((w, h), (1280, 720), "{name} frame {count} dims");
        if count == 30 {
            frame_30 = i420_to_bgra(&color, y, u, v, w as usize, h as usize);
        }
        count += 1;
    }
    eprintln!(
        "{name}: {count} hardware frames in {:?} ({:?}/frame)",
        start.elapsed(),
        start.elapsed() / count.max(1) as u32
    );
    (count, frame_30)
}

/// The plugin must refuse junk by returning null, never by unwinding into us.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn rejects_unopenable_files() {
    assert!(HwSession::open(Path::new("/definitely/not/here.mp4")).is_none());
    assert!(
        HwSession::open(Path::new("/etc/hostname")).is_none(),
        "not an mp4"
    );
}

/// 24 frames cut from a 2160p HDR remux that used to give a black picture and
/// an instant `eof`: at 3840x2160 with CTB 64 the picture is 34 CTB rows, so
/// wavefront slices carry `num_entry_point_offsets == 33` (§7.4.7.1 allows up
/// to PicHeightInCtbsY - 1) and the vendored parser indexed a `[u32; 32]` with
/// 32. Conformant stream, panicking parser -- caught at the plugin edge, which
/// is exactly why it reached the user as a silent eof.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn hardware_decodes_4k_wavefront_slices() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/test_hevc_4k_wpp.mkv");
    let mut hw = open_hw(&path);
    let mut count = 0usize;
    while let Some((_, _, _, w, h)) = hw.next_frame().expect("hardware decode") {
        assert_eq!((w, h), (3840, 2160), "frame {count} dims");
        count += 1;
    }
    assert!(count > 0, "4K wavefront stream decoded no pictures");
}

/// ...and the same file with its slice bytes scrambled -- a stream the plugin
/// opens and then cannot decode a picture out of -- is refused at the door, in
/// words that do not send the user installing a plugin they already have. The
/// case this whole bug arrived as: never a black picture and a silent `eof`.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn an_undecodable_hevc_stream_is_refused_by_name() {
    let good = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/test_hevc_4k_wpp.mkv"),
    )
    .expect("read the 4K fixture");
    // Every SPS in it (`nal_unit_type` 33, so the two header bytes are 42 01)
    // gets `sps_max_sub_layers_minus1 = 7`, which §7.4.3.2.1 does not allow and
    // the parser now refuses instead of indexing a 7-long array with 7.
    let mut broken = good.clone();
    let mut patched = 0;
    for i in 0..broken.len() - 2 {
        if broken[i] == 0x42 && broken[i + 1] == 0x01 {
            broken[i + 2] |= 0x0e;
            patched += 1;
        }
    }
    assert!(patched > 0, "no SPS found in the fixture");
    let path = Scratch::file("undecodable_hevc", "mkv");
    std::fs::write(&path, &broken).expect("write the broken copy");

    let refused = DecodeSession::open(&path)
        .expect_err("a stream with no decodable picture must not open")
        .to_string();
    assert_eq!(refused, Codec::Hevc.undecodable());
}

#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn hardware_decodes_baseline() {
    let (count, _) = decode_all_hw("test_baseline.mp4");
    assert_eq!(count, 150);
}

#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn hardware_decodes_high_profile() {
    // B-frames: the plugin emits display order (DPB bumping happens inside
    // cros-codecs), so a clean run to EOF is the check we can make headlessly.
    let (count, _) = decode_all_hw("test_high.mp4");
    assert_eq!(count, 150);
}

/// Seeking may only change *when* a picture arrives, never the picture: both
/// backends must land on bytes identical to a linear decode of the same index.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn seek_matches_linear() {
    const TARGET: u32 = 60;
    let path = asset("test_baseline.mp4");

    // Software side first, pinned like the test above. SAFETY: --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let (_, rx) = DecodeSession::open(&path).expect("software open");
    let linear = rx
        .into_iter()
        .find(|f| f.index == TARGET)
        .expect("software frame 60")
        .bgra;

    let start = Instant::now();
    let (_, rx, _cancel) = DecodeSession::open_at(&path, TARGET).expect("software open_at");
    let seeked = rx.into_iter().next().expect("no frame after software seek");
    eprintln!("software seek to {TARGET} took {:?}", start.elapsed());
    assert_eq!(seeked.index, TARGET, "first frame after seek is the target");
    assert_eq!(seeked.bgra, linear, "software seek != software linear");

    let start = Instant::now();
    let mut hw = HwSession::open_at(&path, TARGET).expect("no plugin/driver available");
    let (y, u, v, w, h) = hw
        .next_frame()
        .expect("hardware decode")
        .expect("no frame after hardware seek");
    eprintln!("hardware seek to {TARGET} took {:?}", start.elapsed());
    assert_eq!(
        i420_to_bgra(&color_of(&path), y, u, v, w as usize, h as usize),
        linear,
        "hardware seek != software linear"
    );
}

/// Drains a bounded session, returning the indices it delivered. The loop ends
/// only on `RecvError`, i.e. the worker closed the channel by itself.
fn range_indices(path: &Path, start: u32, end: u32) -> Vec<u32> {
    let (_, rx, _cancel) = DecodeSession::open_range(path, start, end).expect("open_range");
    let mut got = Vec::new();
    while let Ok(frame) = rx.recv() {
        got.push(frame.index);
    }
    got
}

/// A bounded range must stop on its own — both backends — with `Frame::index`
/// still the absolute source index, and then disconnect rather than hang.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn range_stops_at_end() {
    const START: u32 = 30;
    const END: u32 = 60;
    let path = asset("test_baseline.mp4");
    let want: Vec<u32> = (START..END).collect();

    // Another test in this binary may already have pinned VE_SW; clear it so
    // the hardware half really is hardware. SAFETY: --test-threads=1.
    unsafe { std::env::remove_var("VE_SW") };
    assert_eq!(range_indices(&path, START, END), want, "hardware range");

    unsafe { std::env::set_var("VE_SW", "1") };
    assert_eq!(range_indices(&path, START, END), want, "software range");

    // Degenerate ranges close cleanly instead of decoding to EOF.
    assert!(range_indices(&path, 30, 30).is_empty(), "empty range");
    assert!(range_indices(&path, 60, 30).is_empty(), "inverted range");
    // Past the end is clamped to the frame count.
    assert_eq!(range_indices(&path, 148, 999).len(), 2, "clamped range");
}

/// The real thing: a 4K HDR10 film through the funnel, against the picture the
/// same planes gave before the tone map was in it (BT.2020 matrix, HDR codes
/// shown as if they were SDR -- flat and grey-washed, which is the complaint
/// this whole path answers). Colour has to come *up*, not down, and the picture
/// has to land somewhere a display can show rather than at either rail.
///
/// Gated on the film being here: it is a 20 GB download, not a fixture. HEVC
/// Main 10, so this is a hardware-only test in every sense.
#[test]
#[ignore = "needs libengine_hw.so, a VA-API driver and the film"]
fn the_hdr_film_renders_tone_mapped() {
    // Two minutes in: past the studio logos, inside lit footage.
    const TARGET: u32 = 3000;
    let Some(path) = engine::real_library::film("hevc_4k_hdr") else {
        return;
    };
    let path = path.as_path();
    // SAFETY: --test-threads=1. There is no software HEVC decoder, so a pinned
    // VE_SW from another test in this binary would refuse the file outright.
    unsafe { std::env::remove_var("VE_SW") };

    // The old picture, by hand off the plugin's own planes: the funnel did
    // exactly this before the tone map, and it is what the comparison needs.
    let mut hw = HwSession::open_at(path, TARGET).expect("no plugin/driver available");
    let (y, u, v, w, h) = hw
        .next_frame()
        .expect("hardware decode")
        .expect("no frame at the seek");
    let (w, h) = (w as usize, h as usize);
    let (y, u, v) = (y.to_vec(), u.to_vec(), v.to_vec());
    let untouched = i420_to_bgra(&color_of(path), &y, &u, &v, w, h);

    // ...and the picture the engine shows now, through the funnel itself.
    let (_, rx, _cancel) = DecodeSession::open_at(path, TARGET).expect("open the film");
    let shown = rx.recv().expect("a frame off the funnel").bgra;
    assert_eq!(shown.len(), untouched.len(), "frame size");

    // Saturation, as the mean distance from grey per pixel...
    let spread = |bgra: &[u8]| -> f64 {
        let total: u64 = bgra
            .chunks_exact(4)
            .map(|px| u64::from(px[..3].iter().max().unwrap() - px[..3].iter().min().unwrap()))
            .sum();
        total as f64 / (bgra.len() / 4) as f64
    };
    // ...and mean luma, in the limited-range code the tone map's anchors are in.
    let luma = |bgra: &[u8]| -> f64 {
        let total: f64 = bgra
            .chunks_exact(4)
            .map(|px| {
                0.2126 * f64::from(px[2]) + 0.7152 * f64::from(px[1]) + 0.0722 * f64::from(px[0])
            })
            .sum();
        16.0 + 219.0 * (total / (bgra.len() / 4) as f64) / 255.0
    };
    let (was, now) = (spread(&untouched), spread(&shown));
    eprintln!(
        "frame {TARGET}: saturation {was:.1} -> {now:.1}, mean luma {:.1} -> {:.1}",
        luma(&untouched),
        luma(&shown)
    );
    assert!(
        now > was,
        "the tone-mapped frame is no more colourful: {now}"
    );
    let mean = luma(&shown);
    assert!(
        (24.0..=200.0).contains(&mean),
        "the tone-mapped frame sits at one end of the scale: mean luma {mean}"
    );

    // The funnel's own budget at this size: the map plus the conversion, which
    // is what an HDR frame costs on top of the decode. Fastest of five, for the
    // reason `tonemap::perf_4k_and_1080p` documents.
    let mapper = ToneMapper::new(Transfer::Pq, engine::tonemap::Preset::default(), None);
    let sdr = ColorDescription {
        matrix: Matrix::Bt709,
        transfer: engine::colorspace::Transfer::Sdr,
        full_range: false,
    };
    let mut best = f64::MAX;
    for _ in 0..5 {
        let (mut my, mut mu, mut mv) = (y.clone(), u.clone(), v.clone());
        let t = Instant::now();
        mapper.map(&mut my, &mut mu, &mut mv, w, h);
        let _ = i420_to_bgra(&sdr, &my, &mu, &mv, w, h);
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    eprintln!("{w}x{h} tone map + convert: {best:.1} ms/frame");
    assert!(best <= 39.0, "{best:.1} ms/frame over the 39 ms budget");
}

/// H.264 8-bit decode is bit-exact by conformance, so the two backends must
/// agree. A mismatch means a driver deviation and is reported, not swallowed.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver"]
fn hardware_matches_software_on_frame_30() {
    let (_, hw) = decode_all_hw("test_baseline.mp4");

    // Pin the comparison side to software ourselves: without this, running the
    // suite without VE_SW=1 would compare hardware against hardware and pass
    // vacuously. SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };

    let start = Instant::now();
    let (_, rx) = DecodeSession::open(asset("test_baseline.mp4")).expect("software open");
    let sw = rx
        .into_iter()
        .find(|f| f.index == 30)
        .expect("software frame 30")
        .bgra;
    eprintln!("software reached frame 30 in {:?}", start.elapsed());

    assert_eq!(hw.len(), sw.len(), "frame size");
    let diff = hw.iter().zip(&sw).filter(|(a, b)| a != b).count();
    eprintln!("frame 30 differing bytes: {diff} / {}", hw.len());
    assert_eq!(diff, 0, "hardware and software decode disagree");
}
