//! HEVC in Matroska, 8- and 10-bit, sound included: the shape a film off a disc
//! arrives in, and the one the AV1 walk was written for one codec too early.
//!
//! Three things meet here and each is checked on its own: the demuxer reads an
//! `hvcC` out of a `CodecPrivate` and reframes Matroska blocks to Annex-B, the
//! plugin decodes Main 10 through a P010 surface pool, and the sound -- 5.1 AAC
//! at 48 kHz -- is read by symphonia's mkv reader, decoded by `rusty_aac` and
//! folded to the stereo one output device carries.
//!
//! The container and audio checks need nothing installed. The decode twins need
//! a built `libengine_hw.so` and a VA-API driver with an HEVC decode entrypoint
//! (`vainfo | grep HEVC`), so they are `#[ignore]`d:
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test hevc_mkv -- --include-ignored --nocapture
//! ```

use std::path::PathBuf;

use engine::demux::{Codec, Demuxer};
use engine::hw::HwSession;
use engine::{AudioSession, DecodeSession};

/// 1280x720@30, 2 s, keyed twice -- see `scripts/gen_fixtures.sh`.
const FRAMES: u32 = 60;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The container half: an HEVC track in an mkv is found, named, measured, and
/// its blocks come back as Annex-B access units with the parameter sets in
/// front of every keyframe. Matroska indexes nothing, so all four numbers come
/// out of the walk.
#[test]
fn the_demuxer_reads_hevc_out_of_a_matroska_file() {
    for (name, depth) in [("test_hevc.mkv", 8), ("test_hevc10.mkv", 10)] {
        let (meta, mut demuxer) = Demuxer::open(&asset(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(meta.codec, Codec::Hevc, "{name}");
        assert_eq!((meta.width, meta.height), (1280, 720), "{name}");
        assert!((meta.frame_rate - 30.0).abs() < 1e-6, "{name}: {}", meta.frame_rate);
        assert_eq!(meta.frame_count, FRAMES, "{name}");
        // What picks the P010 surface pool over the NV12 one.
        assert_eq!(demuxer.bit_depth(), depth, "{name}");

        let first = demuxer
            .next_access_unit()
            .expect("read")
            .expect("a first access unit");
        // A Matroska block is length-prefixed NALs, exactly as an mp4 sample is,
        // and what a decoder has to be handed is Annex-B with the VPS (NAL type
        // 32) out of the `CodecPrivate` in front.
        assert_eq!(&first[..4], [0, 0, 0, 1], "{name}: Annex-B start code");
        assert_eq!(
            (first[4] >> 1) & 0x3f,
            32,
            "{name}: the keyframe leads with the VPS off the CodecPrivate"
        );
        let mut count = 1;
        while demuxer.next_access_unit().expect("read").is_some() {
            count += 1;
        }
        assert_eq!(count, FRAMES, "{name}: every block came back out");

        // The fixture is keyed twice (`-g 30`), and a seek may only ever land on
        // a keyframe. *Which* block that is, is the encoder's business: an HEVC
        // stream with B-frames is stored in decode order and the second IDR
        // lands a couple of blocks before the display frame it shows, so the
        // check is on the rule rather than on the number.
        let second = demuxer.seek_to_sync_at_or_before(FRAMES - 1);
        assert!(
            second > 0 && second < i64::from(FRAMES),
            "{name}: a second keyframe, at {second}"
        );
        assert_eq!(
            demuxer.seek_to_sync_at_or_before(second as u32),
            second,
            "{name}: landing on a keyframe stays on it"
        );
        assert_eq!(
            demuxer.seek_to_sync_at_or_before(second as u32 - 1),
            0,
            "{name}: one block earlier there is only the first keyframe"
        );
    }

    // The invariant: the AV1 mkv and the HEVC mp4 still read as themselves.
    let (av1, _) = Demuxer::open(&asset("test_av1.mkv")).expect("open test_av1.mkv");
    assert_eq!(av1.codec, Codec::Av1);
    let (mp4, hevc) = Demuxer::open(&asset("test_hevc.mp4")).expect("open test_hevc.mp4");
    assert_eq!(mp4.codec, Codec::Hevc);
    assert_eq!(hevc.bit_depth(), 8);
}

/// A Matroska file's sound is read now, and a 5.1 track joins the timeline as
/// the stereo source the fold makes of it.
#[test]
fn matroska_audio_is_read_and_five_one_arrives_as_stereo() {
    let stereo = AudioSession::probe(asset("test_hevc.mkv"), 0)
        .expect("probe")
        .expect("the fixture has an AAC track");
    assert_eq!((stereo.sample_rate, stereo.channels), (44_100, 2));

    let wide = AudioSession::probe(asset("test_hevc10.mkv"), 0)
        .expect("probe")
        .expect("the fixture has a 5.1 AAC track");
    assert_eq!(
        (wide.sample_rate, wide.channels),
        (48_000, 2),
        "5.1 is folded to the layout one output device carries"
    );

    // The picker's row, which is what a user chooses a stream from.
    let streams = AudioSession::probe_streams(asset("test_hevc10.mkv")).expect("streams");
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].codec, "aac");
    assert_eq!(streams[0].channels, 2);
    assert!(streams[0].decodable);

    // ...and nothing is left to explain away: a file whose sound plays is not
    // one the "cannot be decoded" notice may name.
    assert!(
        AudioSession::open(asset("test_hevc10.mkv"))
            .expect("open")
            .is_some()
    );
}

/// The fold, measured on the tones the fixture puts one per channel: every
/// channel but the LFE reaches the stereo pair, the surrounds and the centre at
/// -3 dB of the fronts, and nothing crosses sides.
///
/// Which tone is which *position* is the encoder's business (ffmpeg's AAC
/// encoder does not keep the order they were written in), so the check is on
/// the shape: two full-weight tones, one per side; one tone in both at -3 dB;
/// two more at -3 dB, one per side; and the 60 Hz LFE nowhere.
#[test]
fn the_five_one_fold_keeps_every_channel_but_the_lfe() {
    let (meta, rx) = AudioSession::open(asset("test_hevc10.mkv"))
        .expect("open")
        .expect("the fixture has sound");
    assert_eq!((meta.sample_rate, meta.channels), (48_000, 2));
    let mut left = Vec::new();
    let mut right = Vec::new();
    for chunk in rx {
        for pair in chunk.samples.chunks_exact(2) {
            left.push(pair[0]);
            right.push(pair[1]);
        }
    }
    // Past the encoder priming, and a whole number of cycles of every tone.
    let (left, right) = (&left[9_600..57_600], &right[9_600..57_600]);
    let tones = [60.0, 220.0, 440.0, 880.0, 1320.0, 1760.0];
    let l: Vec<f32> = tones.iter().map(|f| tone(left, *f)).collect();
    let r: Vec<f32> = tones.iter().map(|f| tone(right, *f)).collect();

    // The source tones are at 0.125 (lavfi's `sine`), and the fold divides by
    // the sum of its own coefficients so a full-scale source cannot clip.
    let norm = 1.0 / (1.0 + 2.0 * std::f32::consts::FRAC_1_SQRT_2);
    let full = 0.125 * norm;
    let quiet = full * std::f32::consts::FRAC_1_SQRT_2;
    let near = |got: f32, want: f32| (got - want).abs() < 0.01;

    assert!(
        near(l[0], 0.0) && near(r[0], 0.0),
        "the LFE is dropped: {} {}",
        l[0],
        r[0]
    );
    let both = tones
        .iter()
        .enumerate()
        .filter(|(i, _)| near(l[*i], quiet) && near(r[*i], quiet))
        .count();
    assert_eq!(both, 1, "exactly the centre reaches both sides: {l:?} {r:?}");
    let fronts = tones
        .iter()
        .enumerate()
        .filter(|(i, _)| near(l[*i], full) != near(r[*i], full))
        .count();
    assert_eq!(fronts, 2, "one front channel per side: {l:?} {r:?}");
    let surrounds = tones
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            (near(l[*i], quiet) && near(r[*i], 0.0)) || (near(r[*i], quiet) && near(l[*i], 0.0))
        })
        .count();
    assert_eq!(surrounds, 2, "one surround per side, at -3 dB: {l:?} {r:?}");
}

/// Amplitude of one frequency in `x` (Goertzel), which is what "this channel
/// reached that output" is measured with.
fn tone(x: &[f32], freq: f32) -> f32 {
    let w = 2.0 * std::f32::consts::PI * freq / 48_000.0;
    let (cos, sin) = (w.cos(), w.sin());
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &v in x {
        let s0 = v + 2.0 * cos * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let (re, im) = (s1 - s2 * cos, s2 * sin);
    2.0 * (re * re + im * im).sqrt() / x.len() as f32
}

/// The picture, on the hardware that decodes it: every frame of both files, at
/// the size the container declared. Main 10 goes through the P010 pool and is
/// read back to the same 8-bit I420 the 8-bit path hands out, so the only thing
/// this can tell them apart by is that neither errors.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with an HEVC entrypoint"]
fn the_plugin_decodes_hevc_in_matroska_at_both_depths() {
    for name in ["test_hevc.mkv", "test_hevc10.mkv"] {
        let mut hw = HwSession::open(&asset(name)).expect("no hardware decode plugin available");
        let mut count = 0;
        let mut lit = 0;
        while let Some((y, _, _, w, h)) = hw.next_frame().expect("hardware decode") {
            assert_eq!((w, h), (1280, 720), "{name} frame {count}");
            // Not a black frame: the fixture is a colour pattern, and a P010
            // surface read back through the NV12 path would come out as noise
            // or as nothing at all.
            if y.iter().any(|&s| s > 32) {
                lit += 1;
            }
            count += 1;
        }
        assert_eq!(count, FRAMES as usize, "{name}");
        assert_eq!(lit, count, "{name}: every frame carries picture");
    }
}

/// ...and the same file through the session a clip actually plays from, which
/// is where a codec that only the plugin can decode is refused if it cannot.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with an HEVC entrypoint"]
fn a_ten_bit_matroska_clip_decodes_through_the_ordinary_session() {
    let (meta, frames) = DecodeSession::open(asset("test_hevc10.mkv")).expect("open");
    assert_eq!(meta.codec, Codec::Hevc);
    assert_eq!(frames.iter().count(), FRAMES as usize);
}
