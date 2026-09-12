//! AV1 import: the third source codec that is not H.264, and the first that
//! arrives in a container of its own -- `mp4 0.14` knows no `av01` sample entry
//! at all, so AV1 comes in as Matroska and the demuxer walks the EBML itself.
//!
//! Decoding has two seats: the project's own `ec-av1` in this process, and the
//! VA-API plugin's (`vainfo | grep AV1`) in front of it. The software seat needs
//! nothing installed, so its witnesses run unignored (`VE_SW` pins it); the
//! hardware twins need a built `libengine_hw.so` and are `#[ignore]`d:
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test av1 -- --include-ignored --nocapture --test-threads=1
//! ```
//!
//! `VE_SW` is process-wide, hence `--test-threads=1`: the software-seat tests
//! set it and put it back so the hardware twins below really are hardware.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::demux::{Codec, Demuxer};
use engine::export::ExportSettings;
use engine::scratch::Scratch;
use engine::{AudioSession, DecodeSession, PlaybackSession, Project};

/// 1280x720@30, 2 s, keyframes 30 apart -- see `scripts/gen_fixtures.sh`.
const FRAMES: u32 = 60;
const KEYFRAME: u32 = 30;
/// The film grain twin is 3 s of the same shape, for the reason its own test
/// gives.
const GRAIN_FRAMES: u32 = 90;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The container half. Matroska indexes neither frames nor rate, so all three
/// numbers here come out of the walk: the count is the blocks of the track, the
/// rate is `DefaultDuration` in nanoseconds (33333333, i.e. 30 fps to seven
/// digits -- the millisecond timestamps beside it would say 30.30).
#[test]
fn the_demuxer_reports_an_av1_track_in_matroska() {
    let (meta, _) = Demuxer::open(&asset("test_av1.mkv")).expect("open test_av1.mkv");
    assert_eq!(meta.codec, Codec::Av1);
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert!(
        (meta.frame_rate - 30.0).abs() < 1e-6,
        "DefaultDuration must not truncate: {}",
        meta.frame_rate
    );
    assert_eq!(meta.frame_count, FRAMES);

    // The invariant, stated as an assert: the mp4 codecs still say themselves.
    let (h264, _) = Demuxer::open(&asset("test_baseline.mp4")).expect("open test_baseline.mp4");
    assert_eq!(h264.codec, Codec::H264);
    let (hevc, _) = Demuxer::open(&asset("test_hevc.mp4")).expect("open test_hevc.mp4");
    assert_eq!(hevc.codec, Codec::Hevc);
    let (vp9, _) = Demuxer::open(&asset("test_vp9.mp4")).expect("open test_vp9.mp4");
    assert_eq!(vp9.codec, Codec::Vp9);
}

/// The depth, which is what picks the surface pool the plugin decodes into: an
/// `av1C` with `high_bitdepth` set reads as 10 and takes the P010 pool HEVC Main
/// 10 already goes through, and the 8-bit fixture beside it still reads as 8.
/// It used to be an outright refusal at open ("10-bit AV1 is not supported").
#[test]
fn a_ten_bit_av1_track_is_read_as_ten_bit() {
    let (meta, ten) = Demuxer::open(&asset("test_av1_10.mkv")).expect("open test_av1_10.mkv");
    assert_eq!(meta.codec, Codec::Av1);
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert_eq!(meta.frame_count, FRAMES);
    assert_eq!(ten.bit_depth(), 10, "what picks the P010 surface pool");

    // The invariant: the 8-bit file is untouched by any of it.
    let (_, eight) = Demuxer::open(&asset("test_av1.mkv")).expect("open test_av1.mkv");
    assert_eq!(eight.bit_depth(), 8);
}

/// The delivered audio scope, as an assert: a Matroska file's AAC track is read
/// like any other file's (`tests/hevc_mkv.rs` is where that lives), and its
/// *picture* being AV1 changes nothing about it.
#[test]
fn matroska_audio_is_read_whatever_the_picture_is() {
    let path = asset("test_av1.mkv");
    let probe = AudioSession::probe(&path, 0)
        .expect("probe must not fail")
        .expect("the fixture has an AAC track");
    assert_eq!((probe.sample_rate, probe.channels), (44_100, 2));
    let streams = AudioSession::probe_streams(&path).expect("streams");
    assert_eq!(streams.len(), 1, "one readable audio stream: {streams:?}");
    assert!(streams[0].decodable);
}

/// Every block comes back, keyframes carry the sequence header, and the sync
/// index is what a seek lands on. An AV1 decoder cannot start without a
/// sequence header, and `mkv` carries it in `CodecPrivate` -- so this is the
/// check that says the `av1C` record was really parsed and re-injected.
#[test]
fn every_block_comes_back_and_keyframes_carry_the_sequence_header() {
    let (_, mut demuxer) = Demuxer::open(&asset("test_av1.mkv")).expect("open test_av1.mkv");
    let first = demuxer
        .next_access_unit()
        .expect("read")
        .expect("a first access unit");
    // OBU header: bits 6..3 are the type, and 1 is a sequence header.
    assert_eq!(
        (first[0] >> 3) & 0xF,
        1,
        "the keyframe leads with the sequence header"
    );
    assert_eq!(first[0] & 0x2, 2, "obu_has_size_field: low-overhead format");

    let mut count = 1;
    while demuxer.next_access_unit().expect("read").is_some() {
        count += 1;
    }
    assert_eq!(count, FRAMES, "every block came back out");

    // The fixture is keyed every 30 frames, and a seek may only land on one:
    // anywhere in the second GOP rewinds to frame 30, anywhere in the first to
    // frame 0.
    assert_eq!(
        demuxer.seek_to_sync_at_or_before(KEYFRAME + 15),
        i64::from(KEYFRAME)
    );
    assert_eq!(
        demuxer.seek_to_sync_at_or_before(KEYFRAME),
        i64::from(KEYFRAME)
    );
    assert_eq!(demuxer.seek_to_sync_at_or_before(KEYFRAME - 1), 0);
    assert_eq!(demuxer.seek_to_sync_at_or_before(0), 0);
    assert_eq!(
        demuxer.seek_to_sync_at_or_before(FRAMES * 10),
        i64::from(KEYFRAME),
        "past the end clamps to the last sync point"
    );
    // And the access unit after a seek is the keyframe's, sequence header and
    // all -- a decoder restarted mid-file needs it exactly as much.
    let after = demuxer.next_access_unit().expect("read").expect("a unit");
    assert_eq!((after[0] >> 3) & 0xF, 1, "{}", after[0]);
}

/// The seat this file used to refuse by name now exists: `ec-av1`, in this
/// process, behind the same `VE_SW` pin the H.264 seat takes. A linear open
/// delivers every frame the container indexes, labelled `0..n` in display
/// order -- the contract the transport reads and the one the hardware seat
/// always kept.
#[test]
fn the_software_seat_decodes_every_frame() {
    // SAFETY: the suite is documented to run with --test-threads=1. The pin
    // stays up until the frames are drained: it is read on the worker thread
    // when the span opens, not only at the door here.
    unsafe { std::env::set_var("VE_SW", "1") };
    let (meta, rx) = DecodeSession::open(asset("test_av1.mkv")).expect("software AV1 open");
    assert_eq!(meta.frame_count, FRAMES);
    let indices: Vec<u32> = rx.into_iter().map(|f| f.index).collect();
    unsafe { std::env::remove_var("VE_SW") };
    assert_eq!(indices, (0..FRAMES).collect::<Vec<_>>(), "frames must arrive 0..n in display order");
}

/// ...and the 10-bit twin, whose samples arrive as `u16` in `0..=1023` and
/// narrow to the same 8-bit planes the plugin's P010 read-back produces.
#[test]
fn the_software_seat_decodes_ten_bit_av1() {
    // SAFETY: the suite is documented to run with --test-threads=1; held
    // through the drain, as above.
    unsafe { std::env::set_var("VE_SW", "1") };
    let (meta, rx) = DecodeSession::open(asset("test_av1_10.mkv")).expect("software AV1 open");
    assert_eq!(meta.frame_count, FRAMES);
    let count = rx.into_iter().count();
    unsafe { std::env::remove_var("VE_SW") };
    assert_eq!(count, FRAMES as usize, "every 10-bit frame came out");
}

/// Seeking may only change *when* a picture arrives, never the picture: a seek
/// onto the software seat lands on the very bytes a linear decode of the same
/// index delivered, on either side of and away from a keyframe. The promise
/// `hw_decode::seek_matches_linear_every_container` makes of the hardware
/// seat, made here of the software one -- which is why this runs unignored.
#[test]
fn seek_lands_on_the_picture_a_linear_software_decode_delivered() {
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let path = asset("test_av1.mkv");
    let (meta, rx) = DecodeSession::open(&path).expect("software open");
    let mut targets = vec![0, 1, 29, 30, 31, 45, meta.frame_count - 1];
    targets.retain(|&t| t < meta.frame_count);
    targets.sort_unstable();
    targets.dedup();
    let linear: Vec<(u32, Vec<u8>)> = rx
        .into_iter()
        .filter(|f| targets.contains(&f.index))
        .map(|f| (f.index, f.bgra))
        .collect();
    assert_eq!(
        linear.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        targets,
        "linear decode never delivered every target"
    );
    for (target, want) in linear {
        let (_, rx, _cancel) = DecodeSession::open_at(&path, target).expect("open_at");
        let seeked = rx
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no frame after seek to {target}"));
        assert_eq!(seeked.index, target, "first frame after seek is not the target");
        assert!(
            seeked.bgra == want,
            "seek to {target} handed back a different picture than a linear decode of {target}"
        );
    }
    unsafe { std::env::remove_var("VE_SW") };
}

/// A timeline is *not* refused a source coded differently -- every clip opens
/// its own decoder, and since the `ec-av1` seat that is true with no plugin
/// and no driver in the room: the no-plugin machine, forced, takes the AV1
/// source beside its H.264 one.
#[test]
fn a_timeline_takes_av1_on_a_machine_with_no_plugin() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av.mp4");
    // SAFETY: the suite is documented to run with --test-threads=1. The pin
    // stays up until the import has been through the seat's door (and the
    // worker it spawned has opened its span on it), then comes back down.
    unsafe { std::env::set_var("VE_SW", "1") };
    let imported = session
        .import(&asset("test_av1.mkv"))
        .is_ok();
    assert!(imported, "the software seat is a seat: the timeline takes AV1");
    assert_eq!(session.sources().len(), 2, "both sources stand");
    unsafe { std::env::remove_var("VE_SW") };
}

// --- the refusal witness ---------------------------------------------------
//
// The minimal EBML this file's refusal fixture is written with -- the same
// builders `mkv_encodings.rs` uses, kept local because test binaries do not
// share code. Every size goes out in the 8-byte long form, legal everywhere
// and arithmetic-free.

const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2AD7B1;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;
const DEFAULT_DURATION: u32 = 0x23E383;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const CLUSTER: u32 = 0x1F43_B675;
const CLUSTER_TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;

fn el(id: u32, body: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = id.to_be_bytes()[(id.leading_zeros() / 8) as usize..].to_vec();
    out.push(0x01);
    out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
    out.extend_from_slice(body);
    out
}

fn uint(id: u32, value: u64) -> Vec<u8> {
    el(id, &value.to_be_bytes())
}

/// A `SimpleBlock` of `track`, `rel` ticks into its cluster; `flags` carries
/// the keyframe bit (0x80).
fn block(track: u8, rel: i16, flags: u8, body: &[u8]) -> Vec<u8> {
    let mut b = vec![0x80 | track];
    b.extend_from_slice(&rel.to_be_bytes());
    b.push(flags);
    b.extend_from_slice(body);
    el(SIMPLE_BLOCK, &b)
}

/// One OBU's total length inside a low-overhead stream: the header byte (plus
/// its extension byte, which this fixture format never writes) and the
/// leb128 size that `obu_has_size_field` promises.
fn obu_len(stream: &[u8]) -> usize {
    assert_eq!(stream[0] & 0x2, 2, "obu_has_size_field: low-overhead format");
    let mut len = 1;
    let mut size: usize = 0;
    loop {
        let byte = stream[len];
        len += 1;
        size = size << 7 | usize::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            break;
        }
    }
    len + size
}

/// A stream whose GOP structure is broken in a way the container cannot see:
/// the second keyframe-flagged block carries an *inter* frame's bytes, as a
/// remux that trusted the wrong key index would write one. The demuxer opens
/// it, injects the sequence header ahead of it, and `ec-av1` refuses it by
/// name -- an inter frame with no key frame behind it finds no saved CDF state
/// to resume from, one of the live refusals its inventory pins -- and the seat
/// fails the span in words instead of a panic or a garbage picture: the frames
/// decoded so far stand, then the channel closes.
#[test]
fn a_broken_gop_is_refused_in_words_and_never_a_panic() {
    // The pieces, off the real fixture: the sequence header OBU the demuxer
    // injects (the `av1C` payload) and a real inter frame's block bytes.
    let (_, mut demuxer) = Demuxer::open(&asset("test_av1.mkv")).expect("open test_av1.mkv");
    let key_au = demuxer.next_access_unit().expect("read").expect("key au");
    let seq_len = obu_len(&key_au);
    let seq_header = &key_au[..seq_len];
    let key_body = &key_au[seq_len..];
    // The very next access unit is frame 1, a real inter frame: no injection
    // ahead of a non-sync sample, so its bytes are the block verbatim.
    let inter_body = demuxer.next_access_unit().expect("read").expect("inter au");

    // The words first, at the decoder's own door: the crafted chunk is
    // refused, and the refusal is a sentence, not a panic.
    let mut broken = seq_header.to_vec();
    broken.extend_from_slice(&inter_body);
    let refused = ec_av1::stream::decode_stream(&broken)
        .expect_err("an inter frame with no key frame behind it is refused");
    assert!(
        refused.to_string().contains("no saved CDF state"),
        "the refusal names its reason: {refused}"
    );

    // The same broken stream as a file, through the seat: open succeeds (the
    // container is valid), the first GOP decodes, the second chunk refuses,
    // and the span ends cleanly with exactly the pictures decoded so far.
    let av1c = [0x81u8, 0x00, 0x0C, 0x00];
    let mut private = av1c.to_vec();
    private.extend_from_slice(seq_header);
    let track = el(
        TRACK_ENTRY,
        &[
            uint(TRACK_NUMBER, 1),
            uint(TRACK_TYPE, 1),
            el(CODEC_ID, b"V_AV1"),
            el(CODEC_PRIVATE, &private),
            uint(DEFAULT_DURATION, 33_333_333),
            el(VIDEO, &[uint(PIXEL_WIDTH, 1280), uint(PIXEL_HEIGHT, 720)].concat()),
        ]
        .concat(),
    );
    let cluster = el(
        CLUSTER,
        &[
            uint(CLUSTER_TIMESTAMP, 0),
            block(1, 0, 0x80, key_body),
            block(1, 33, 0x80, &inter_body),
        ]
        .concat(),
    );
    let segment = [
        el(INFO, &uint(TIMESTAMP_SCALE, 1_000_000)),
        el(TRACKS, &track),
        cluster,
    ]
    .concat();
    let file = [
        el(0x1A45_DFA3, &[]),
        el(SEGMENT, &segment),
    ]
    .concat();
    let scratch = Scratch::file("broken_gop_av1", "mkv");
    std::fs::write(&scratch, &file).expect("write fixture");

    let (meta, mut opened) = Demuxer::open(&scratch).expect("the container itself is valid");
    assert_eq!(meta.codec, Codec::Av1);
    assert_eq!(meta.frame_count, 2);
    // Both blocks are key-flagged, so both lead with the injected sequence
    // header -- which is what makes the second one a chunk boundary, and the
    // refusal the decoder's own answer to what it finds there.
    let first = opened.next_access_unit().expect("read").expect("au");
    assert_eq!((first[0] >> 3) & 0xF, 1);

    // SAFETY: the suite is documented to run with --test-threads=1; the pin
    // held through the drain, as in every seat test above.
    unsafe { std::env::set_var("VE_SW", "1") };
    let (_, rx) = DecodeSession::open(&scratch).expect("open succeeds; the container is fine");
    let frames: Vec<u32> = rx.into_iter().map(|f| f.index).collect();
    unsafe { std::env::remove_var("VE_SW") };
    assert_eq!(frames, vec![0], "the first GOP stands, the broken one refuses");
}

/// The end-to-end user path: opening the file yields pictures, all of them,
/// through the plugin.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with AV1 decode"]
fn the_plugin_decodes_every_av1_frame() {
    let start = Instant::now();
    let (meta, frames) = DecodeSession::open(asset("test_av1.mkv")).expect("open test_av1.mkv");
    assert_eq!(meta.codec, Codec::Av1);
    let frames: Vec<_> = frames.into_iter().collect();
    eprintln!(
        "test_av1.mkv: {} frames in {:?}",
        frames.len(),
        start.elapsed()
    );

    assert_eq!(frames.len() as u32, FRAMES, "every block decoded");
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!((frame.width, frame.height), (1280, 720), "frame {i} dims");
        assert_eq!(frame.index, i as u32, "frames arrive in display order");
        assert_eq!(frame.bgra.len(), 1280 * 720 * 4, "frame {i} size");
    }
    // A picture, not a flat surface: a driver handing back an untouched buffer
    // would satisfy every count above.
    let first = &frames[0].bgra;
    assert!(
        first.chunks_exact(4).any(|px| px != &first[..4]),
        "frame 0 is a single colour -- no picture was decoded"
    );
}

/// The picture's flattest 8x8 block, as a standard deviation of its green
/// channel: 0 where the decoder handed back a smooth picture, well above it
/// where every pixel carries synthesized grain.
fn flattest_block(bgra: &[u8], width: u32, height: u32) -> f64 {
    let (w, h) = (width as usize, height as usize);
    let mut flattest = f64::MAX;
    for y in (0..h - 8).step_by(8) {
        for x in (0..w - 8).step_by(8) {
            let block = (0..8).flat_map(|dy| {
                (0..8).map(move |dx| f64::from(bgra[((y + dy) * w + x + dx) * 4 + 1]))
            });
            let values: Vec<f64> = block.collect();
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let var =
                values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / values.len() as f64;
            flattest = flattest.min(var.sqrt());
        }
    }
    flattest
}

/// The defect none of the fixtures above could see: an AV1 frame that asks for
/// film grain is *displayed* from a second surface, which the driver
/// synthesizes the grain into and which the decoder has to give it. Handing it
/// none gets the picture refused outright -- radeonsi answers `vaEndPicture`
/// with VA_STATUS_ERROR_INVALID_SURFACE -- and that killed a real 1080p AV1
/// film at frame 49, the first of its pictures with `apply_grain` set. Every
/// fixture beside this one is grainless, and every one of them passed.
///
/// Ninety frames, all of them: a grainy picture costs two pool buffers rather
/// than one, so this turns the pool over several times and cycles the eight
/// reference slots -- decoding "the first handful" is exactly what let the
/// defect live.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with AV1 decode"]
fn a_film_grain_av1_file_decodes_whole_and_keeps_its_grain() {
    let (meta, frames) =
        DecodeSession::open(asset("test_av1_grain.mkv")).expect("open test_av1_grain.mkv");
    assert_eq!(meta.codec, Codec::Av1);
    let frames: Vec<_> = frames.into_iter().collect();
    assert_eq!(frames.len() as u32, GRAIN_FRAMES, "every frame decoded");

    // ...and the grain is *in* the picture, which is the half "it did not fail"
    // cannot say: telling the driver `apply_grain = 0` also makes every frame
    // decode, and silently strips the film of the texture it was graded with.
    // Grain is noise on every pixel, so no block of a grainy picture is flat;
    // this source has plenty of flat ones without it (measured on the fixture:
    // 0.00 with the grain off, 1.58 with it on).
    let frame = &frames[GRAIN_FRAMES as usize - 30];
    let flattest = flattest_block(&frame.bgra, frame.width, frame.height);
    assert!(
        flattest > 0.5,
        "the grain never reached the picture: flattest 8x8 block deviation {flattest:.3}"
    );
}

/// ...and the 10-bit file through the very call the window's open door makes: a
/// user opens it and the timeline shows a picture, read back off the P010 pool
/// to the same 8-bit BGRA every other frame arrives in. The whole point of the
/// slice, and the half a container test cannot say anything about.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with 10-bit AV1 decode"]
fn opening_a_ten_bit_av1_file_shows_frames() {
    let mut session =
        PlaybackSession::open(asset("test_av1_10.mkv")).expect("open the 10-bit file");
    assert_eq!(session.meta().codec, Codec::Av1);
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
    // A picture, not a flat surface: a P010 surface read back through the NV12
    // path would come out as noise or as nothing at all.
    assert!(
        frame.bgra.chunks_exact(4).any(|px| px != &frame.bgra[..4]),
        "the first frame is a single colour -- no picture was decoded"
    );
}

/// A seek into the second GOP hands back that very frame, not the keyframe it
/// had to restart from: the demuxer's sync index and the plugin's skip count
/// have to agree, and Matroska is where both are this crate's own doing.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with AV1 decode"]
fn a_seek_lands_on_the_frame_it_asked_for() {
    let (_, frames, _cancel) =
        DecodeSession::open_range(asset("test_av1.mkv"), KEYFRAME + 7, FRAMES)
            .expect("open at a frame inside the second GOP");
    let frames: Vec<_> = frames.into_iter().collect();
    assert_eq!(frames.len() as u32, FRAMES - KEYFRAME - 7, "to the end");
    assert_eq!(frames[0].index, KEYFRAME + 7, "the frame asked for");
}

/// An mp4 export of a Matroska source carries its sound.
///
/// Three lives, this one. It used to *succeed* silently and write picture with
/// no sound, because this engine could not read a Matroska file's audio at all.
/// Then the sound became readable and this became a refusal by name, because
/// the mp4 path could only *copy* AAC out of an mp4's own sample table and a
/// Matroska file has none. Now the copy that cannot be made is a decode and a
/// re-encode (`export::copy_audio` -> `encode_audio`), so the export carries the
/// film's sound instead of naming what it cannot do with it. The picture half of
/// the same export is [`an_av1_export_reopens_through_our_own_demuxer`].
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with AV1 decode"]
fn an_mp4_export_of_a_matroska_source_carries_its_sound() {
    let session = PlaybackSession::open(asset("test_av1.mkv")).expect("open test_av1.mkv");
    let meta = *session.meta();
    // A short one: the picture is re-encoded frame by frame and what is under
    // test here is the sound reaching the file, not H.264 throughput.
    let project = Project::single(asset("test_av1.mkv"), 15);
    let out = Scratch::file("ve_export_av1", "mp4");

    let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(900), "export hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle
        .result()
        .expect("outcome")
        .expect("an mkv's AAC is decoded and encoded again into the mp4");
    let (audio, chunks) = AudioSession::open(&out)
        .expect("reopen the export")
        .expect("the export has an audio track");
    let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    println!("mkv -> mp4: {} Hz, rms {rms:.4}", audio.sample_rate);
    assert!(rms > 0.001, "the film's sound is in the mp4 (rms {rms})");
    std::fs::remove_file(&out).unwrap();
}

/// A short timeline out of the H.264 fixture, which is what the AV1 export tests
/// below write: `rav1e` is built here without its assembly, so the length of the
/// timeline is the length of the test.
fn short_timeline(frames: f64) -> PlaybackSession {
    let mut session = PlaybackSession::open(asset("test_baseline.mp4")).expect("open the fixture");
    assert!(session.cut_at(frames / 30.0), "cut at frame {frames}");
    assert!(
        session.delete_clip(engine::project::Lane::V1, 1),
        "drop everything after it"
    );
    session
}

fn av1_settings() -> ExportSettings {
    ExportSettings {
        format: engine::export::Format::Av1,
        // The software encoder, on every machine: the hardware twin below is
        // where the plugin's AV1 seat is exercised.
        seat: engine::export::EncoderSeat::Software,
        ..Default::default()
    }
}

fn exported(
    session: &PlaybackSession,
    name: &str,
    settings: &ExportSettings,
    limit: Duration,
) -> Scratch {
    let out = Scratch::file(&format!("ve_av1_{name}"), "mkv");
    let started = Instant::now();
    let handle = session.export_to_with(&out, settings);
    while !handle.is_finished() {
        assert!(
            started.elapsed() < limit,
            "export did not finish in {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("outcome").expect("export");
    assert_eq!(handle.progress(), 1.0, "finished at full progress");
    let spent = started.elapsed().as_secs_f64();
    let frames = (session.timeline_duration() * 30.0).round().max(1.0);
    eprintln!(
        "{name}: {frames} frames in {spent:.2} s = {:.2} ms/frame",
        spent * 1000.0 / frames
    );
    out
}

/// The export half of this slice: AV1 out of the software encoder, into a
/// Matroska file this project's own demuxer walks back. Nothing installed is
/// needed -- the file is *read* as a container here, which is where the
/// hand-written EBML either agrees with the hand-written EBML reader or does not.
#[test]
fn an_av1_export_reopens_through_our_own_demuxer() {
    let session = short_timeline(30.0);
    // The software encoder alone costs ~270 s for these 30 frames on the
    // machine of record: 300 s left it one co-running test away from a
    // timeout, so the budget is a hang guard, not a speed claim.
    let out = exported(&session, "sw", &av1_settings(), Duration::from_secs(900));

    let (meta, mut demuxer) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(meta.codec, Codec::Av1, "an AV1 export is AV1");
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert!(
        (meta.frame_rate - 30.0).abs() < 1e-6,
        "DefaultDuration must state the rate exactly: {}",
        meta.frame_rate
    );
    assert_eq!(meta.frame_count, 30, "every timeline frame is a block");

    // The first block is a keyframe and leads with the sequence header -- the
    // demuxer prepends `CodecPrivate` to every keyframe, so this is also the
    // check that the `av1C` record was written and parsed back.
    let first = demuxer.next_access_unit().expect("read").expect("a unit");
    assert_eq!(
        (first[0] >> 3) & 0xF,
        1,
        "the keyframe leads with the sequence header"
    );
    assert_eq!(first[0] & 0x2, 2, "obu_has_size_field: low-overhead format");
    let mut count = 1;
    while demuxer.next_access_unit().expect("read").is_some() {
        count += 1;
    }
    assert_eq!(count, 30, "every block comes back out");
    assert_eq!(
        demuxer.seek_to_sync_at_or_before(29),
        0,
        "one GOP, one sync point"
    );
    std::fs::remove_file(&out).unwrap();
}

/// A timeline with sound, exported as AV1 into **both** containers: the file
/// carries the picture *and* the timeline's audio, and this project's own
/// readers are what say so -- the demuxer for the picture, the audio session for
/// the sound. An AV1 export used to be picture only, which is a file whose sound
/// went missing with nobody told.
///
/// Two frames of 720p: `rav1e` is built here without its assembly, so the length
/// of the timeline is the length of the test, and what is under test is the
/// wiring from the format to the muxer, not the encoder.
#[test]
fn an_av1_export_carries_the_timelines_sound_in_either_container() {
    for (format, ext) in [
        (engine::export::Format::Av1, "mkv"),
        (engine::export::Format::Av1Mp4, "mp4"),
    ] {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av");
        assert!(session.cut_at(2.0 / 30.0), "two frames of it");
        assert!(session.delete_clip(engine::project::Lane::V1, 1));
        let out = Scratch::file("ve_av1_sound", ext);
        let settings = ExportSettings {
            format,
            seat: engine::export::EncoderSeat::Software,
            ..Default::default()
        };
        let handle = session.export_to_with(&out, &settings);
        let started = Instant::now();
        while !handle.is_finished() {
            assert!(started.elapsed() < Duration::from_secs(900), "export hung");
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.result().expect("outcome").expect("an AV1 export");

        // The picture, through the demuxer an import comes in by -- an `av01`
        // sample entry in the mp4 and a `V_AV1` track in the Matroska file, both
        // of them written by hand and read back by hand.
        let (meta, _) = Demuxer::open(&out).expect("reopen the export");
        assert_eq!(meta.codec, Codec::Av1, "{ext}: an AV1 export is AV1");
        assert_eq!((meta.width, meta.height), (1280, 720), "{ext}");
        assert_eq!(meta.frame_count, 2, "{ext}: every timeline frame is there");

        // ...and the sound beside it, which is the half that used to be absent.
        let (audio, chunks) = AudioSession::open(&out)
            .expect("reopen for its sound")
            .expect("an AV1 export has an audio track");
        let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
        println!(
            "{ext}: {} Hz x{}, rms {rms:.4}",
            audio.sample_rate, audio.channels
        );
        assert!(rms > 0.001, "{ext}: the sound is in the file (rms {rms})");
        std::fs::remove_file(&out).unwrap();
    }
}

/// An audio-only timeline is refused an AV1 export by name, exactly as it is
/// refused an mp4: every frame of it is a gap, so the file would be black.
#[test]
fn an_av1_export_of_an_audio_only_timeline_is_refused_by_name() {
    let session = PlaybackSession::open(asset("test_tone.wav")).expect("open the tone");
    let out = Scratch::file("ve_av1_refused", "mkv");
    let handle = session.export_to_with(&out, &av1_settings());
    while !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let refused = handle
        .result()
        .expect("outcome")
        .expect_err("an audio-only timeline has no picture to code")
        .to_string();
    assert!(refused.contains("no picture"), "{refused}");
    assert!(refused.contains("AV1"), "{refused}");
    assert!(!out.exists(), "nothing is written for a refusal");
}

/// The whole loop, and the only place it is closed: the file the export wrote is
/// decoded back into pictures, and those pictures are the source's.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with AV1 decode"]
fn an_av1_export_decodes_back_into_the_pictures_that_went_in() {
    let session = short_timeline(30.0);
    let out = exported(
        &session,
        "roundtrip",
        &av1_settings(),
        Duration::from_secs(900),
    );

    let (_, frames) = DecodeSession::open(&out).expect("decode the export");
    let frames: Vec<_> = frames.into_iter().collect();
    assert_eq!(frames.len(), 30, "every written frame decodes back");
    let (_, source, _) = DecodeSession::open_range(asset("test_baseline.mp4"), 0, 30)
        .expect("open the source again");
    let source: Vec<_> = source.into_iter().collect();
    for (i, (written, original)) in frames.iter().zip(&source).enumerate() {
        let diff: f64 = written
            .bgra
            .iter()
            .zip(&original.bgra)
            .map(|(a, b)| f64::from(a.abs_diff(*b)))
            .sum::<f64>()
            / written.bgra.len() as f64;
        assert!(
            diff < 12.0,
            "frame {i} drifted by {diff:.2} from the source"
        );
    }
    std::fs::remove_file(&out).unwrap();
}

/// The hardware seat of the same pair, which is opt-in: `VE_HW_AV1=1` is what
/// enters it, because the vendored encoder reset the GPU of the box this was
/// written on (see `export::Enc::open_av1`). Without that variable this measures
/// the software encoder again, which is the honest outcome rather than a
/// failure, so what it asserts is the file.
#[test]
#[ignore = "needs libengine_hw.so, VE_HW_AV1=1 and a driver whose AV1 encoder survives it"]
fn an_av1_export_runs_on_the_plugin_where_the_gpu_has_one() {
    let session = short_timeline(60.0);
    let settings = ExportSettings {
        format: engine::export::Format::Av1,
        ..Default::default()
    };
    let out = exported(&session, "hw", &settings, Duration::from_secs(600));
    let (meta, _) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(meta.codec, Codec::Av1);
    assert_eq!(meta.frame_count, 60);
    std::fs::remove_file(&out).unwrap();
}
