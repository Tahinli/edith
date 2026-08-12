//! The two Matroska features a disc rip and a streaming remux arrive with and
//! this project used to refuse: `ContentEncodings` -- header stripping and zlib
//! -- and lacing, which packs several frames into one block.
//!
//! The fixtures are built here byte by byte rather than by `ffmpeg`: this
//! project's own muxer laces nothing and encodes nothing, and ffmpeg writes
//! neither by default, so the only way to have a file with a lace header in it
//! is to write the header. Every one is a real Matroska file the demuxer walks
//! with no special path.
//!
//! ```text
//! cargo test -p engine --test mkv_encodings
//! ```

use std::path::{Path, PathBuf};

use engine::demux::{Codec, Demuxer, MkvAudio};

// The element ids this writes, as they are written in the file.
const EBML_HEADER: u32 = 0x1A45_DFA3;
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
const CONTENT_ENCODINGS: u32 = 0x6D80;
const CONTENT_ENCODING: u32 = 0x6240;
const CONTENT_ENCODING_TYPE: u32 = 0x5033;
const CONTENT_COMPRESSION: u32 = 0x5034;
const CONTENT_COMP_ALGO: u32 = 0x4254;
const CONTENT_COMP_SETTINGS: u32 = 0x4255;
const CLUSTER: u32 = 0x1F43_B675;
const CLUSTER_TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;

/// A minimal `avcC`: 4-byte NAL length prefixes, one SPS and one PPS of four
/// bytes each. The bytes of the parameter sets are never parsed by the demuxer
/// -- it copies them ahead of every keyframe -- so four of each is enough to
/// see them arrive.
const AVCC: [u8; 19] = [
    1, 0x64, 0, 0x1f, 0xff, 0xe1, 0, 4, 0x67, 0x64, 0, 0x1f, 1, 0, 4, 0x68, 0xee, 0x3c, 0x80,
];
/// The Annex-B the demuxer must put in front of a keyframe, out of `AVCC`.
const SETS: [u8; 16] = [
    0, 0, 0, 1, 0x67, 0x64, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80,
];
/// `zlib.compress(b"Hello there")` -- a fixture cannot compress for itself: the
/// engine keeps the *in*flate half, which is the half a demuxer needs.
const ZLIB_HELLO: [u8; 19] = [
    0x78, 0xda, 0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x28, 0xc9, 0x48, 0x2d, 0x4a, 0x05, 0x00, 0x18,
    0x66, 0x04, 0x2d,
];

/// One EBML element: the id as it is written, then the size in the 8-byte long
/// form -- legal everywhere and it keeps this builder arithmetic-free.
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

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// A `SimpleBlock` of `track`, `rel` ticks into its cluster. `flags` carries the
/// keyframe bit (0x80) and the lacing bits (0x06).
fn block(track: u8, rel: i16, flags: u8, body: &[u8]) -> Vec<u8> {
    let mut b = vec![0x80 | track];
    b.extend_from_slice(&rel.to_be_bytes());
    b.push(flags);
    b.extend_from_slice(body);
    el(SIMPLE_BLOCK, &b)
}

/// A `ContentEncodings` element: the compression algorithm and its settings.
fn compression(algo: u64, settings: &[u8]) -> Vec<u8> {
    let comp = cat(&[
        uint(CONTENT_COMP_ALGO, algo),
        el(CONTENT_COMP_SETTINGS, settings),
    ]);
    el(
        CONTENT_ENCODINGS,
        &el(CONTENT_ENCODING, &el(CONTENT_COMPRESSION, &comp)),
    )
}

/// A whole file: a header, the segment's `TimestampScale` (a millisecond a
/// tick), the tracks and the clusters.
fn mkv(tracks: &[Vec<u8>], clusters: &[Vec<u8>]) -> Vec<u8> {
    let segment = cat(&[
        el(INFO, &uint(TIMESTAMP_SCALE, 1_000_000)),
        el(TRACKS, &tracks.concat()),
        clusters.concat(),
    ]);
    cat(&[el(EBML_HEADER, &[]), el(SEGMENT, &segment)])
}

/// An H.264 track entry, `encoding` being its `ContentEncodings` if it has one.
fn video_track(encoding: &[u8]) -> Vec<u8> {
    let body = cat(&[
        uint(TRACK_NUMBER, 1),
        uint(TRACK_TYPE, 1),
        el(CODEC_ID, b"V_MPEG4/ISO/AVC"),
        el(CODEC_PRIVATE, &AVCC),
        uint(DEFAULT_DURATION, 33_333_333),
        el(
            VIDEO,
            &cat(&[uint(PIXEL_WIDTH, 64), uint(PIXEL_HEIGHT, 64)]),
        ),
        encoding.to_vec(),
    ]);
    el(TRACK_ENTRY, &body)
}

/// An AC-3 track entry -- the codec id [`MkvAudio`] reads blocks for. Its blocks
/// are never decoded here: what is under test is the walk.
fn audio_track(encoding: &[u8]) -> Vec<u8> {
    let body = cat(&[
        uint(TRACK_NUMBER, 2),
        uint(TRACK_TYPE, 2),
        el(CODEC_ID, b"A_AC3"),
        encoding.to_vec(),
    ]);
    el(TRACK_ENTRY, &body)
}

fn subtitle_track(codec: &[u8], encoding: &[u8]) -> Vec<u8> {
    let body = cat(&[
        uint(TRACK_NUMBER, 3),
        uint(TRACK_TYPE, 0x11),
        el(CODEC_ID, codec),
        encoding.to_vec(),
    ]);
    el(TRACK_ENTRY, &body)
}

fn cluster(ts: u64, blocks: &[Vec<u8>]) -> Vec<u8> {
    el(
        CLUSTER,
        &cat(&[uint(CLUSTER_TIMESTAMP, ts), blocks.concat()]),
    )
}

/// The lace header and frames of a Xiph-laced block: every size but the last as
/// a run of 255s and a remainder.
fn xiph(frames: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![(frames.len() - 1) as u8];
    for f in &frames[..frames.len() - 1] {
        let mut left = f.len();
        while left >= 255 {
            out.push(255);
            left -= 255;
        }
        out.push(left as u8);
    }
    out.extend(frames.concat());
    out
}

/// Fixed lacing: the count and nothing else, the frames dividing the rest.
fn fixed(frames: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![(frames.len() - 1) as u8];
    out.extend(frames.concat());
    out
}

/// EBML lacing: the first size as an unsigned vint, the rest as signed
/// differences from the one before -- one byte each here, so the bias is 63.
fn ebml_lace(frames: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![(frames.len() - 1) as u8];
    out.push(0x80 | frames[0].len() as u8);
    for pair in frames[..frames.len() - 1].windows(2) {
        let diff = pair[1].len() as i64 - pair[0].len() as i64;
        out.push(0x80 | (diff + 63) as u8);
    }
    out.extend(frames.concat());
    out
}

/// Under the cargo target directory, never `/tmp`: these are build artefacts.
fn write(name: &str, bytes: &[u8]) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, bytes).expect("write the fixture");
    path
}

/// Bug A, in one file: a video track whose every block was written with its
/// first byte cut off and that byte filed once in `ContentCompSettings`. Read
/// without putting it back, the access unit is a NAL length prefix that lies and
/// the decoder shows nothing -- which is exactly how a BluRay remux used to open
/// here.
#[test]
fn header_stripping_puts_the_cut_byte_back_on_every_block() {
    // One 4-byte-prefixed NAL per block, the leading zero of the prefix stripped
    // off by the muxer.
    let key: [u8; 9] = [0, 0, 0, 5, 0x65, 0xAA, 0xBB, 0xCC, 0xDD];
    let inter: [u8; 9] = [0, 0, 0, 5, 0x41, 1, 2, 3, 4];
    let stripped = mkv(
        &[video_track(&compression(3, &[0]))],
        &[cluster(
            0,
            &[
                block(1, 0, 0x80, &key[1..]),
                block(1, 1, 0x00, &inter[1..]),
            ],
        )],
    );
    let path = write("stripped.mkv", &stripped);
    let (meta, mut demuxer) = Demuxer::open(&path).expect("open a header-stripped file");
    assert_eq!(meta.codec, Codec::H264);
    assert_eq!(meta.frame_count, 2);

    let first = demuxer.next_access_unit().expect("read").expect("a unit");
    let mut want = SETS.to_vec();
    want.extend_from_slice(&[0, 0, 0, 1, 0x65, 0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(first, want, "the keyframe is the parameter sets and the NAL");
    let second = demuxer.next_access_unit().expect("read").expect("a unit");
    assert_eq!(second, [0, 0, 0, 1, 0x41, 1, 2, 3, 4]);
    assert!(demuxer.next_access_unit().expect("read").is_none());

    // The invariant: the same file with nothing stripped and no encoding
    // declared reads back byte for byte the same. Nothing about an ordinary
    // Matroska file changed.
    let plain = mkv(
        &[video_track(&[])],
        &[cluster(0, &[block(1, 0, 0x80, &key), block(1, 1, 0x00, &inter)])],
    );
    let path = write("plain.mkv", &plain);
    let (_, mut demuxer) = Demuxer::open(&path).expect("open the plain file");
    assert_eq!(demuxer.next_access_unit().expect("read").expect("a"), want);
}

/// Bug B: every audio block of a WEB remux is laced. All three headers the spec
/// defines are read, one `Block` per frame -- and the frames of a lace get a
/// timestamp each, interpolated across the gap to the block after, because six
/// E-AC-3 frames stacked on one instant is a sound track that walks away from
/// the picture.
#[test]
fn the_three_lace_headers_come_back_one_frame_per_block() {
    let three: [&[u8]; 3] = [b"aaa", b"bbbb", b"cc"];
    let even: [&[u8]; 3] = [b"aaa", b"bbb", b"ccc"];
    for (name, flags, first, second, frames) in [
        ("xiph", 0x02, xiph(&three), xiph(&three), three),
        ("fixed", 0x04, fixed(&even), fixed(&even), even),
        ("ebml", 0x06, ebml_lace(&three), ebml_lace(&three), three),
    ] {
        let file = mkv(
            &[audio_track(&[])],
            &[cluster(
                0,
                &[
                    block(2, 0, 0x80 | flags, &first),
                    block(2, 30, 0x80 | flags, &second),
                ],
            )],
        );
        let path = write(&format!("lace_{name}.mkv"), &file);
        let mut audio = MkvAudio::open(&path)
            .expect("walk the blocks")
            .expect("an AC-3 track");
        assert_eq!(audio.blocks(), 6, "{name}: two blocks of three frames");
        for i in 0..6 {
            assert_eq!(
                audio.frame(i).expect("read").expect("a frame"),
                frames[i % 3],
                "{name}: frame {i}"
            );
        }
        // 30 ms of block spread over three frames: 10 ms each, and the last
        // lace -- which has no block after it to measure against -- keeps the
        // step the one before it measured.
        for (i, want) in [0.0, 0.010, 0.020, 0.030, 0.040, 0.050].iter().enumerate() {
            assert!(
                (audio.secs(i) - want).abs() < 1e-9,
                "{name}: frame {i} at {} s, wanted {want}",
                audio.secs(i)
            );
        }
        // A seek resolves against those per-frame times, which is the whole
        // point of interpolating them: the block at 25 ms is the third frame.
        assert_eq!(audio.block_at(0.025, 0), 2, "{name}: seek");
    }
}

/// The two features together, which is the file that has both: a stripped and
/// laced audio track. The strip is per *frame*, not per block -- the spec
/// compresses the frames of a lace one by one.
#[test]
fn a_stripped_and_laced_track_gets_both_undone() {
    let frames: [&[u8]; 2] = [b"\x77\x01\x02", b"\x77\x03\x04"];
    let file = mkv(
        &[audio_track(&compression(3, &[0x0B]))],
        &[cluster(0, &[block(2, 0, 0x84, &fixed(&frames))])],
    );
    let path = write("stripped_lace.mkv", &file);
    let mut audio = MkvAudio::open(&path)
        .expect("walk")
        .expect("an AC-3 track");
    assert_eq!(audio.blocks(), 2);
    assert_eq!(audio.frame(0).unwrap().unwrap(), b"\x0B\x77\x01\x02");
    assert_eq!(audio.frame(1).unwrap().unwrap(), b"\x0B\x77\x03\x04");
}

/// zlib is the other encoding a muxer really writes -- mkvmerge compressed
/// subtitle tracks with it by default for years -- and it arrives through the
/// user's own subtitle door, as text.
#[test]
fn a_zlib_subtitle_track_inflates_into_cues() {
    let file = mkv(
        &[subtitle_track(b"S_TEXT/UTF8", &compression(0, &[]))],
        &[cluster(0, &[block(3, 0, 0x80, &ZLIB_HELLO)])],
    );
    let path = write("zlib_subs.mkv", &file);
    let tracks = engine::subtitle::of_matroska(&path).expect("read the tracks");
    let track = tracks.first().expect("one subtitle track");
    assert_eq!(track.refused, None, "a zlib track is readable");
    assert_eq!(track.cues.len(), 1);
    assert_eq!(track.cues[0].text, "Hello there");
}

/// Every encoding this cannot undo is refused *by name* -- the defect these
/// share is a file that opens and shows nothing, and a reader that guesses is
/// how that happens. Where the refusal lands differs by track: a picture refuses
/// the file, a sound track refuses the sound, a subtitle track is listed with
/// the reason beside it.
#[test]
fn what_cannot_be_undone_is_refused_by_name() {
    for (algo, word) in [(1u64, "bzlib"), (2, "lzo1x"), (7, "algorithm 7")] {
        let file = mkv(
            &[video_track(&compression(algo, &[]))],
            &[cluster(0, &[block(1, 0, 0x80, &[0, 0, 0, 1, 0x65])])],
        );
        let path = write(&format!("algo_{algo}.mkv"), &file);
        let Err(refused) = Demuxer::open(&path) else {
            panic!("a picture compressed with {word} must not open");
        };
        assert!(
            refused.to_string().contains(word),
            "the refusal names the algorithm: {refused}"
        );
    }

    // An encrypted sound track: the *sound* refuses, and the picture of the same
    // file still opens -- one unreadable track is not a file nobody can play.
    let encryption = el(
        CONTENT_ENCODINGS,
        &el(CONTENT_ENCODING, &uint(CONTENT_ENCODING_TYPE, 1)),
    );
    let file = mkv(
        &[video_track(&[]), audio_track(&encryption)],
        &[cluster(0, &[block(1, 0, 0x80, &[0, 0, 0, 1, 0x65])])],
    );
    let path = write("encrypted_audio.mkv", &file);
    let Err(refused) = MkvAudio::open(&path) else {
        panic!("an encrypted sound track must not be read as if it were plain");
    };
    assert!(
        refused.to_string().contains("encrypted"),
        "the refusal says what it is: {refused}"
    );
    assert!(Demuxer::open(&path).is_ok(), "the picture still opens");

    // And a subtitle track is listed with its reason, like a bitmap one: a film
    // with one unreadable track opens with the others.
    let file = mkv(
        &[subtitle_track(b"S_TEXT/UTF8", &compression(2, &[]))],
        &[cluster(0, &[block(3, 0, 0x80, b"never read")])],
    );
    let path = write("lzo_subs.mkv", &file);
    let tracks = engine::subtitle::of_matroska(&path).expect("the file still opens");
    let why = tracks[0].refused.as_deref().expect("a refused track");
    assert!(why.contains("lzo1x"), "the row says why: {why}");
}

/// The film this slice was cut for: a BluRay remux whose video track is header
/// stripped. Existence-gated -- it is 1.3 GB of somebody's disc and lives on one
/// machine -- and it goes through the door the window's import makes, so what it
/// proves is a picture on the timeline and not a well-shaped byte string.
#[test]
fn the_stripped_bluray_remux_decodes_a_picture() {
    let Some(path) = engine::real_library::film("h264_dual_audio") else {
        return;
    };
    let path = path.as_path();
    let mut session = engine::PlaybackSession::open(path).expect("open the remux");
    assert_eq!(session.meta().codec, Codec::H264);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let frame = loop {
        if let Some(frame) = session.try_frame() {
            break frame;
        }
        assert!(std::time::Instant::now() < deadline, "no frame in 60 s");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    // Garbage NALs decode to nothing at all or to one flat colour; a picture is
    // the only thing that fails this.
    assert!(
        frame.bgra.chunks_exact(4).any(|px| px != &frame.bgra[..4]),
        "the first frame is a single colour -- the stripped headers were not put back"
    );
    // And it is not a silent film: the sound of this one is AAC, read by
    // symphonia, and the timeline opens both halves or neither.
    assert!(
        engine::AudioSession::open(path)
            .expect("open the sound")
            .is_some(),
        "the remux has an AAC track"
    );
}

/// The other film: 5.1 E-AC-3 whose every block is fixed-laced. The sound is
/// what was missing, so the sound is what is asked for -- through
/// `AudioSession`, which is what the timeline opens it with.
#[test]
fn the_laced_eac3_remux_decodes_sound() {
    let Some(path) = engine::real_library::film("hevc_4k_hdr") else {
        return;
    };
    let path = path.as_path();
    let (meta, rx) = engine::AudioSession::open(path)
        .expect("open the sound")
        .expect("the film has an E-AC-3 track");
    // 5.1 comes down to stereo, as it does off an mp4.
    assert_eq!((meta.sample_rate, meta.channels), (48_000, 2));
    let mut samples = Vec::new();
    for chunk in rx {
        samples.extend_from_slice(&chunk.samples);
        if samples.len() > 48_000 * 2 * 20 {
            break;
        }
    }
    // A film opens on silence often enough that the first second says nothing;
    // twenty of them do not.
    let peak = samples.iter().fold(0f32, |a, s| a.max(s.abs()));
    assert!(peak > 0.01, "twenty seconds of silence: peak {peak}");
}
