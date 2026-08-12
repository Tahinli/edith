//! The capability matrix against real files: what the unit test in
//! `demux::tests` asserts about the dispatch tables, this asserts about the
//! containers themselves — every codec the decoder layer can be handed opens
//! from *both* an mp4 and a Matroska file, and says its own name.
//!
//! The defect class it exists for: VP9 shipped decodable out of an mp4 and
//! refused out of a `.webm` with the VA-API VP9 decoder compiled in, because one
//! dispatch arm was missing while the refusal string claimed the capability was
//! absent. A gap must be a written-down row (`demux::UNSUPPORTED`), never a
//! silent fall-through.
//!
//! Nothing installed is needed: these are container reads, not decodes.

use std::path::PathBuf;

use engine::AudioSession;
use engine::demux::{Codec, Demuxer};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// One row per cell of (codec × container). A cell with no fixture is a hole in
/// the evidence, so every one of them is filled here.
const MATRIX: &[(Codec, &str, &str)] = &[
    (Codec::H264, "test_baseline.mp4", "test_h264.mkv"),
    (Codec::Hevc, "test_hevc.mp4", "test_hevc.mkv"),
    (Codec::Vp9, "test_vp9.mp4", "test_vp9.webm"),
    // `mp4 0.14` writes no `av01` sample entry, so there is no AV1 mp4 fixture
    // to open; the mp4 half of that row is `av01`'s presence in the dispatch
    // table, which `demux::tests` holds.
    (Codec::Av1, "", "test_av1.mkv"),
];

#[test]
fn every_codec_opens_from_every_container_it_arrives_in() {
    for (codec, mp4, mkv) in MATRIX {
        for file in [mp4, mkv].into_iter().filter(|f| !f.is_empty()) {
            let (meta, _) = Demuxer::open(&asset(file))
                .unwrap_or_else(|e| panic!("{file} carries {} and was refused: {e}", codec.name()));
            assert_eq!(meta.codec, *codec, "{file}");
            assert!(meta.frame_count > 0, "{file} came back with no frames");
            assert_eq!((meta.width, meta.height), (1280, 720), "{file}");
        }
    }
}

/// The depth cell of the matrix: 10-bit is a property of the stream, and VP9
/// states it in the one place neither container repeats — the keyframe's
/// uncompressed header. Assumed 8-bit, a profile 2 stream decodes into an NV12
/// pool and comes back as garbage.
#[test]
fn a_ten_bit_vp9_webm_is_read_as_ten_bit() {
    let (meta, demuxer) = Demuxer::open(&asset("test_vp9_10.webm")).expect("open a profile 2 webm");
    assert_eq!(meta.codec, Codec::Vp9);
    assert_eq!(demuxer.bit_depth(), 10, "profile 2 is 10-bit");

    let (_, eight) = Demuxer::open(&asset("test_vp9.webm")).expect("open a profile 0 webm");
    assert_eq!(eight.bit_depth(), 8, "profile 0 is 8-bit by definition");

    // ...and the probe put the read cursor back where it opened: frame 0 is the
    // first picture out, not frame 1.
    let (meta, mut demuxer) = Demuxer::open(&asset("test_vp9.webm")).expect("open");
    let mut count = 0;
    while demuxer.next_access_unit().expect("read").is_some() {
        count += 1;
    }
    assert_eq!(
        count, meta.frame_count,
        "the depth probe ate an access unit"
    );
}

/// The audio half, and the claim a refusal makes: every codec the "cannot be
/// decoded" notice names as readable really opens, per container. It read "AAC
/// and AC-3 only" while FLAC, MP3, Vorbis, ALAC and PCM all decoded out of a
/// Matroska file — a refusal that undersells sends a user re-encoding a film
/// whose sound already plays.
#[test]
fn every_audio_codec_the_refusal_names_really_decodes() {
    // The five in one fixture, in the order `gen_fixtures.sh` muxes them, plus
    // the AAC and AC-3 the older fixtures carry.
    for (stream, codec) in [
        (0, "flac"),
        (1, "mp3"),
        (2, "vorbis"),
        (3, "alac"),
        (4, "pcm"),
    ] {
        let probe = AudioSession::probe(asset("test_mkv_audio.mkv"), stream)
            .unwrap_or_else(|e| panic!("{codec} in a Matroska file: {e}"))
            .unwrap_or_else(|| panic!("{codec} in a Matroska file came back silent"));
        assert_eq!(probe.sample_rate, 44_100, "{codec}");
    }
    // Opus, every container it arrives in: a `.webm` off the web, the `.mka` a
    // 5.1 film soundtrack is remuxed into, and the standalone `.opus`. It was
    // the last codec the notice named as absent while both readers here already
    // *parsed* it -- `A_OPUS` and `OpusHead` have always mapped to symphonia's
    // `CODEC_ID_OPUS`; only the decoder was missing, and it is `ruopus` now.
    for (file, what, channels) in [
        ("test_vp9.webm", "opus in a webm", 1),
        // 5.1 arrives as the stereo fold, exactly as a 5.1 AC-3 track does.
        ("test_opus_51.mka", "5.1 opus in an mka", 2),
        // ...and 7.1, the widest layout the fold has a table for and the one
        // his own remux carries.
        ("test_opus_71.mka", "7.1 opus in an mka", 2),
        ("test_tone.opus", "a standalone opus", 2),
    ] {
        let probe = AudioSession::probe(asset(file), 0)
            .unwrap_or_else(|e| panic!("{what}: {e}"))
            .unwrap_or_else(|| panic!("{what} came back silent"));
        assert_eq!(probe.sample_rate, 48_000, "{what}");
        assert_eq!(probe.channels, channels, "{what}");
    }
    for (file, codec) in [("test_av1.mkv", "aac"), ("test_ac3.mkv", "ac-3")] {
        assert!(
            AudioSession::probe(asset(file), 0).expect(codec).is_some(),
            "{codec} in a Matroska file came back silent"
        );
    }
    for (file, codec) in [("test_av.mp4", "aac-lc"), ("test_ac3.mp4", "ac-3")] {
        assert!(
            AudioSession::probe(asset(file), 0).expect(codec).is_some(),
            "{codec} in an mp4 came back silent"
        );
    }

    // ...and the gap the refusal is really *for*, which is DTS and no longer
    // Opus: no decoder in this tree has it, so the notice names the track and
    // then names what would have worked -- Opus among them, or the refusal is
    // back to underselling the engine.
    let refused = AudioSession::unsupported(asset("test_dts.mkv"))
        .expect("read the header")
        .expect("a DTS track is a reason, not silence");
    assert!(refused.contains("A_DTS"), "{refused}");
    for named in ["FLAC", "MP3", "Opus", "Vorbis"] {
        assert!(
            refused.contains(named),
            "the refusal hides a codec that decodes ({named}): {refused}"
        );
    }
    // The other half of that claim, and the one a stale refusal string cannot
    // fake: a file whose track really does decode has nothing to excuse.
    assert_eq!(
        AudioSession::unsupported(asset("test_vp9.webm")).expect("read the header"),
        None,
        "an Opus track that decodes is not a refusal"
    );
}

/// Which of two video tracks an mp4 opens on may not depend on hash order: it is
/// the first in `moov.traks`, file order, the same box the audio side numbers its
/// streams out of. A `HashMap` walk here made a saved `.edith` reopen on the
/// other picture from one launch to the next.
#[test]
fn the_video_track_of_an_mp4_is_the_first_one_in_file_order() {
    let path = asset("test_two_video.mp4");
    let file = std::fs::File::open(&path).expect("open the fixture");
    let size = file.metadata().expect("stat").len();
    let reader = mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size).expect("header");

    // What file order says, read straight off the box rather than through the
    // map: the test may not share the code it is checking.
    let first = reader
        .moov
        .traks
        .iter()
        .find(|trak| {
            matches!(
                mp4::TrackType::try_from(&trak.mdia.hdlr.handler_type),
                Ok(mp4::TrackType::Video)
            )
        })
        .expect("the fixture has two video tracks");
    let expected = (
        u32::from(first.tkhd.width.value()),
        u32::from(first.tkhd.height.value()),
    );
    assert_eq!(
        expected,
        (1280, 720),
        "the fixture is the one it says it is"
    );

    // Repeated, because the failure this guards against is a *random* one: the
    // hash order that picked the 320x240 track is a different order each run.
    for _ in 0..8 {
        let (meta, _) = Demuxer::open(&path).expect("open the two-track fixture");
        assert_eq!(
            (meta.width, meta.height),
            expected,
            "the demuxer opened the second video track"
        );
    }
}
