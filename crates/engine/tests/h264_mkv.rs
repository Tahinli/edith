//! H.264 in Matroska: the codec this project has always decoded, in the
//! container that used to refuse it by name. Nothing new decodes here -- the
//! demuxer reframes the `avcC`-prefixed blocks to Annex-B and hands them to the
//! same dispatch an mp4's samples go to -- so the whole file runs with nothing
//! installed, on `rusty_h264` if the plugin is absent.
//!
//! ```text
//! cargo test -p engine --test h264_mkv
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::demux::{Codec, Demuxer};
use engine::{AudioSession, PlaybackSession};

/// 1280x720@30, 2 s, keyframes 30 apart -- see `scripts/gen_fixtures.sh`.
const FRAMES: u32 = 60;
const KEYFRAME: u32 = 30;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The Annex-B NAL types of an access unit, in order: `nal_unit_type` is the low
/// 5 bits of the byte after each start code.
fn nal_types(au: &[u8]) -> Vec<u8> {
    au.windows(4)
        .enumerate()
        .filter(|(_, w)| *w == [0, 0, 0, 1])
        .filter_map(|(i, _)| au.get(i + 4).map(|b| b & 0x1f))
        .collect()
}

/// The container half, and the refusal this slice exists to delete: the file
/// opens, and it says H.264 rather than "not supported".
#[test]
fn the_demuxer_reports_an_h264_track_in_matroska() {
    let (meta, _) = Demuxer::open(&asset("test_h264.mkv")).expect("open test_h264.mkv");
    assert_eq!(meta.codec, Codec::H264);
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert!(
        (meta.frame_rate - 30.0).abs() < 1e-6,
        "DefaultDuration must not truncate: {}",
        meta.frame_rate
    );
    assert_eq!(meta.frame_count, FRAMES);

    // The invariant, stated as an assert: the other containers and codecs read
    // back exactly as they did before this arm existed.
    let (av1, _) = Demuxer::open(&asset("test_av1.mkv")).expect("open test_av1.mkv");
    assert_eq!(av1.codec, Codec::Av1);
    assert_eq!(av1.frame_count, FRAMES);
    let (h264, _) = Demuxer::open(&asset("test_baseline.mp4")).expect("open test_baseline.mp4");
    assert_eq!(h264.codec, Codec::H264);
}

/// Matroska stores H.264 in `avcC` framing -- length prefixes, and the SPS/PPS
/// out of band in `CodecPrivate` -- so an access unit is only decodable if both
/// were handled: reframed to Annex-B, parameter sets ahead of every keyframe.
#[test]
fn keyframes_carry_the_parameter_sets_and_every_block_comes_back() {
    let (_, mut demuxer) = Demuxer::open(&asset("test_h264.mkv")).expect("open test_h264.mkv");
    let first = demuxer
        .next_access_unit()
        .expect("read")
        .expect("a first access unit");
    assert_eq!(first[..4], [0, 0, 0, 1], "Annex-B, not a length prefix");
    let types = nal_types(&first);
    assert_eq!(
        &types[..2],
        &[7, 8],
        "the keyframe leads with SPS and PPS: {types:?}"
    );
    assert!(types.contains(&5), "and then the IDR picture: {types:?}");

    let second = demuxer.next_access_unit().expect("read").expect("a unit");
    let types = nal_types(&second);
    assert!(
        !types.contains(&7) && types.contains(&1),
        "a non-keyframe is a plain slice: {types:?}"
    );

    let mut count = 2;
    while demuxer.next_access_unit().expect("read").is_some() {
        count += 1;
    }
    assert_eq!(count, FRAMES, "every block came back out");

    // Keyed every 30 frames, and a seek may only land on one.
    assert_eq!(
        demuxer.seek_to_sync_at_or_before(KEYFRAME + 15),
        i64::from(KEYFRAME)
    );
    assert_eq!(demuxer.seek_to_sync_at_or_before(KEYFRAME - 1), 0);
    // The unit after a seek carries the parameter sets too: a decoder restarted
    // mid-file needs them exactly as much as one started at frame 0.
    let after = demuxer.next_access_unit().expect("read").expect("a unit");
    assert_eq!(&nal_types(&after)[..2], &[7, 8]);
}

/// The user-facing end of it, through the very call the window's open door makes
/// (`app/src/main.rs:2242`): the file becomes a timeline and that timeline shows
/// a picture. No plugin is required -- H.264 is the one codec with a software
/// decoder here -- so this is what says the reframed access units are really
/// decodable and not just well-shaped.
#[test]
fn opening_an_h264_matroska_file_shows_frames() {
    let mut session = PlaybackSession::open(asset("test_h264.mkv")).expect("open test_h264.mkv");
    assert_eq!(session.meta().codec, Codec::H264);
    assert_eq!(session.meta().frame_count, FRAMES);

    // The decoder is a thread behind the door, so a frame is waited for exactly
    // as the window's own pump waits for one.
    let deadline = Instant::now() + Duration::from_secs(60);
    let frame = loop {
        if let Some(frame) = session.try_frame() {
            break frame;
        }
        assert!(Instant::now() < deadline, "no frame in 60 s");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!((frame.width, frame.height), (1280, 720));
    // A picture, not a flat surface: a decoder handed garbage would satisfy
    // every count above.
    assert!(
        frame.bgra.chunks_exact(4).any(|px| px != &frame.bgra[..4]),
        "the first frame is a single colour -- no picture was decoded"
    );
}

/// The sound comes with it: a Matroska file's audio is read whatever its picture
/// is coded with, so nothing is left for the "cannot be decoded" notice to name.
#[test]
fn the_matroska_audio_path_takes_this_file_too() {
    let stereo = AudioSession::probe(asset("test_h264.mkv"), 0)
        .expect("probe")
        .expect("the fixture has an AAC track");
    assert_eq!((stereo.sample_rate, stereo.channels), (44_100, 2));
    // ...and it really opens, which is what says the sound plays rather than
    // merely being described.
    assert!(
        AudioSession::open(asset("test_h264.mkv"))
            .expect("open")
            .is_some()
    );
}
