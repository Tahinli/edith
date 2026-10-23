//! The zlib-compressed (`ContentCompAlgo` 0) Matroska subtitle tracks of the
//! owner's own films, against ffprobe.
//!
//! Two keys, both named by `real_library.toml` (gitignored):
//!
//! - `mkv_zlib_pgs`: the `S_HDMV/PGS` tracks. Edith's own Matroska subtitle walk
//!   ([`engine::demux::matroska_subtitles`]) reads these, so every payload comes
//!   out of `ec_inflate` through the same path the app uses.
//! - `mkv_zlib_vobsub`: the `S_VOBSUB` tracks. Edith's subtitle walk declares
//!   them but reads no VobSub payload (see `matroska_subtitles`), so there is no
//!   app path for those bytes to take. Their blocks are read here straight out
//!   of the file and handed to `ec_inflate` directly, which is the same decoder
//!   the app's Matroska demuxer calls.
//!
//! Both sides assert that the track really is zlib-compressed, from a walk of the
//! file's own `ContentEncodings` (never from what edith reports), so the witness
//! cannot pass silently on an uncompressed track. Each packet is compared with
//! `ffprobe -show_packets -show_data_hash MD5` for that stream: packet count,
//! pts in microseconds, duration where edith states one, size, and the MD5 of
//! the decompressed bytes.
//!
//! A key the manifest does not name, or a path not on this machine, skips.
//! Needs ffprobe.
//!
//! ```text
//! cargo test -p engine --test real_library_zlib -- --nocapture
//! ```

use crate::reallib;

use std::io::Read as _;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use engine::demux::{matroska_subtitles, PGS};
use reallib::{ffprobe_streams, have, md5_hex, FfPacket, FfStream};

// The element ids a walk of the file reads, as they are written in it.
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const CLUSTER: u32 = 0x1F43_B675;
const CLUSTER_TIMESTAMP: u32 = 0xE7;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const CONTENT_ENCODINGS: u32 = 0x6D80;
const CONTENT_ENCODING: u32 = 0x6240;
const CONTENT_ENCODING_TYPE: u32 = 0x5033;
const CONTENT_COMPRESSION: u32 = 0x5034;
const CONTENT_COMP_ALGO: u32 = 0x4254;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const SIMPLE_BLOCK: u32 = 0xA3;

/// `TrackType` of a subtitle track.
const SUBTITLE: u64 = 0x11;
/// The Matroska codec id of VobSub bitmaps.
const VOBSUB: &str = "S_VOBSUB";
/// The same ceiling the app's Matroska demuxer puts on one inflated block.
const LIMIT: usize = 64 << 20;
/// How much of the file the header walk reads. Tracks come before the first
/// cluster, so this is more than enough for the header of any file written here.
const HEAD: usize = 8 << 20;

/// One `TrackEntry` as the file declares it.
#[derive(Debug, Default, Clone)]
struct Track {
    number: u64,
    kind: u64,
    codec: String,
    /// A `ContentEncoding` element was present.
    encoded: bool,
    /// `ContentEncodingType`; the default 0 is compression.
    enc_kind: Option<u64>,
    /// `ContentCompAlgo`; the default 0 is zlib.
    algo: Option<u64>,
}

impl Track {
    /// Whether the file compresses every block of this track with zlib alone.
    fn zlib(&self) -> bool {
        self.encoded && self.enc_kind.unwrap_or(0) == 0 && self.algo.unwrap_or(0) == 0
    }
}

/// One block as the file stores it: its track, its absolute timestamp in
/// ticks, its lacing bits and the byte range of its payload in the file.
#[derive(Debug)]
struct Block {
    track: u64,
    ticks: i64,
    lacing: u8,
    range: std::ops::Range<usize>,
}

/// What a walk of the file's header and, when asked, its blocks found.
#[derive(Debug, Default)]
struct Walk {
    tracks: Vec<Track>,
    blocks: Vec<Block>,
    timestamp_scale: u64,
}

/// An EBML variable-length integer at `at`: its value with the length marker
/// dropped (kept for an element id), its length in bytes, and where it ends.
fn vint(buf: &[u8], at: usize, keep_marker: bool) -> Option<(u64, usize, usize)> {
    let first = *buf.get(at)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let mut raw = 0u64;
    for &b in buf.get(at..at + len)? {
        raw = (raw << 8) | u64::from(b);
    }
    let value = if keep_marker {
        raw
    } else {
        raw & ((1u64 << (7 * len)) - 1)
    };
    Some((value, len, at + len))
}

/// An element header at `at`: its id, its size (`None` for the unknown size a
/// live muxer writes) and where its body starts.
fn header(buf: &[u8], at: usize) -> Option<(u32, Option<u64>, usize)> {
    let (id, _, next) = vint(buf, at, true)?;
    let (size, len, body) = vint(buf, next, false)?;
    let unknown = size == (1u64 << (7 * len)) - 1;
    Some((id as u32, (!unknown).then_some(size), body))
}

fn uint(data: &[u8]) -> u64 {
    data.iter().fold(0, |acc, &b| (acc << 8) | u64::from(b))
}

fn text(data: &[u8]) -> String {
    String::from_utf8_lossy(data)
        .trim_end_matches('\0')
        .to_string()
}

/// Every header element of `buf`, in file order, descending into the masters
/// this reads and skipping every other body by its size. With `blocks` false the
/// walk stops at the first cluster, which is what a header-only read wants.
///
/// A block's lacing is recorded, not undone: a laced block is one this witness
/// refuses, so the caller can say so.
fn walk(buf: &[u8], blocks: bool) -> Walk {
    let mut w = Walk {
        timestamp_scale: 1_000_000,
        ..Walk::default()
    };
    let mut cluster_ticks = 0i64;
    let mut at = 0usize;
    while let Some((id, size, body)) = header(buf, at) {
        match id {
            SEGMENT | INFO | TRACKS | CONTENT_ENCODINGS | CONTENT_COMPRESSION | BLOCK_GROUP => {
                at = body;
                continue;
            }
            CLUSTER => {
                if !blocks {
                    break;
                }
                at = body;
                continue;
            }
            TRACK_ENTRY => {
                w.tracks.push(Track::default());
                at = body;
                continue;
            }
            CONTENT_ENCODING => {
                if let Some(t) = w.tracks.last_mut() {
                    t.encoded = true;
                }
                at = body;
                continue;
            }
            _ => {}
        }
        // A leaf: its body must be present and sized.
        let Some(size) = size else { break };
        let Some(end) = body.checked_add(size as usize).filter(|&e| e <= buf.len()) else {
            break;
        };
        let data = &buf[body..end];
        let last = w.tracks.last_mut();
        match id {
            TRACK_NUMBER => {
                if let Some(t) = last {
                    t.number = uint(data);
                }
            }
            TRACK_TYPE => {
                if let Some(t) = last {
                    t.kind = uint(data);
                }
            }
            CODEC_ID => {
                if let Some(t) = last {
                    t.codec = text(data);
                }
            }
            CONTENT_ENCODING_TYPE => {
                if let Some(t) = last {
                    t.enc_kind = Some(uint(data));
                }
            }
            CONTENT_COMP_ALGO => {
                if let Some(t) = last {
                    t.algo = Some(uint(data));
                }
            }
            TIMESTAMP_SCALE => w.timestamp_scale = uint(data).max(1),
            CLUSTER_TIMESTAMP => cluster_ticks = uint(data) as i64,
            SIMPLE_BLOCK | BLOCK if blocks => {
                let Some((track, _, after)) = vint(data, 0, false) else {
                    break;
                };
                let (Some(rel), Some(&flags)) = (data.get(after..after + 2), data.get(after + 2))
                else {
                    break;
                };
                let rel = i16::from_be_bytes([rel[0], rel[1]]);
                w.blocks.push(Block {
                    track,
                    ticks: cluster_ticks + i64::from(rel),
                    lacing: flags & 0x06,
                    range: body + after + 3..end,
                });
            }
            _ => {}
        }
        at = end;
    }
    w
}

/// One packet as edith states it, in microseconds from the start of the file.
struct Got {
    start_us: i128,
    duration_us: Option<i128>,
    len: usize,
    md5: String,
}

/// ffprobe's packets of one stream: `-show_packets -show_data_hash MD5`.
fn ffprobe_packets_of(path: &Path, stream: u32) -> Vec<FfPacket> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams"])
        .arg(stream.to_string())
        .args(["-show_packets", "-show_data_hash", "MD5"])
        .args(["-show_entries"])
        .arg("packet=stream_index,pts,dts,duration,size,flags,data_hash")
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed on stream {stream}");
    let opt = |f: &str| f.trim().parse::<i64>().ok();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| {
            let f: Vec<&str> = line.split(',').collect();
            FfPacket {
                stream: f[0].trim().parse().expect("stream index"),
                pts: opt(f[1]),
                dts: opt(f[2]),
                duration: opt(f[3]),
                size: f[4].trim().parse().expect("size"),
                key: f[5].starts_with('K'),
                md5: f[6].trim().trim_start_matches("MD5:").to_string(),
            }
        })
        .collect()
}

/// The `time_base` ffprobe states for a stream, as (numerator, denominator).
fn time_base(streams: &[FfStream], index: u32) -> (i128, i128) {
    let s = streams
        .iter()
        .find(|s| s.index == index)
        .expect("ffprobe names the stream");
    let (num, den) = s.time_base.split_once('/').expect("time_base is a ratio");
    (
        num.parse().expect("time_base numerator"),
        den.parse().expect("time_base denominator"),
    )
}

/// Ticks of a stream's time base as microseconds, exactly.
fn ticks_us(ticks: i64, (num, den): (i128, i128)) -> i128 {
    i128::from(ticks) * num * 1_000_000 / den
}

/// Compares edith's packets with ffprobe's, packet for packet. Returns the
/// number of disagreements, each named on stderr with the first few.
fn compare(key: &str, label: &str, ours: &[Got], ff: &[FfPacket], tb: (i128, i128)) -> usize {
    let mut bad = 0;
    if ours.len() != ff.len() {
        eprintln!(
            "{key} {label}: edith has {} packets, ffprobe {}",
            ours.len(),
            ff.len()
        );
        bad += 1;
    }
    for (i, (o, f)) in ours.iter().zip(ff).enumerate() {
        let start = f.pts.map(|p| ticks_us(p, tb));
        let dur = f.duration.map(|d| ticks_us(d, tb));
        let ok = start == Some(o.start_us)
            && o.duration_us.is_none_or(|d| dur == Some(d))
            && f.size == o.len
            && f.md5 == o.md5;
        if !ok {
            bad += 1;
            if bad <= 5 {
                eprintln!(
                    "{key} {label}: packet {i}: edith start {} dur {:?} len {} md5 {} | ffprobe pts {:?} dur {:?} size {} md5 {}",
                    o.start_us, o.duration_us, o.len, o.md5, start, dur, f.size, f.md5
                );
            }
        }
    }
    bad
}

/// `mkv_zlib_pgs`: every zlib PGS track, its packets read through edith's own
/// Matroska subtitle walk and compared with ffprobe.
fn check_pgs(key: &str) {
    let Some(path) = engine::real_library::film(key) else {
        return;
    };
    if !have("ffprobe") {
        eprintln!("skipped: no ffprobe to state the reference packets");
        return;
    }
    let started = Instant::now();
    let mut head = Vec::new();
    std::fs::File::open(&path)
        .expect("open the file")
        .take(HEAD as u64)
        .read_to_end(&mut head)
        .expect("read the file's head");
    let header = walk(&head, false);
    let ff_streams = ffprobe_streams(&path);
    let subs = matroska_subtitles(&path).expect("edith reads the subtitle tracks");

    let mut tracks = 0;
    let mut packets = 0;
    let mut bad = 0;
    // The index of a track among all the file's tracks is its ffprobe stream
    // index: both count the TrackEntry elements in file order.
    for (pos, t) in header.tracks.iter().enumerate() {
        if t.kind != SUBTITLE || t.codec != PGS || !t.zlib() {
            continue;
        }
        tracks += 1;
        let sub = subs
            .iter()
            .find(|s| s.number == t.number)
            .unwrap_or_else(|| panic!("{key}: edith lists no subtitle track {}", t.number));
        assert!(
            sub.unsupported.is_none(),
            "{key}: edith refuses the zlib track: {:?}",
            sub.unsupported
        );
        let index = pos as u32;
        assert_eq!(
            ff_streams[pos].kind, "subtitle",
            "{key}: ffprobe stream {index} is not a subtitle stream"
        );
        let tb = time_base(&ff_streams, index);
        let ours: Vec<Got> = sub
            .cues
            .iter()
            .map(|c| Got {
                start_us: i128::from(c.start_us),
                duration_us: c.duration_us.map(i128::from),
                len: c.payload.len(),
                md5: md5_hex(&c.payload),
            })
            .collect();
        let ff = ffprobe_packets_of(&path, index);
        assert!(
            !ours.is_empty(),
            "{key}: edith read no cues for track {index}"
        );
        packets += ours.len();
        bad += compare(key, &format!("track {index}"), &ours, &ff, tb);
        eprintln!(
            "{key}: track {index} (PGS, zlib): {} packets compared with ffprobe",
            ours.len()
        );
    }
    assert!(
        tracks > 0,
        "{key}: no zlib-compressed PGS track in the file"
    );
    eprintln!(
        "{key}: {packets} packets over {tracks} zlib PGS track(s), {bad} mismatches, {:.1} s",
        started.elapsed().as_secs_f64()
    );
    assert_eq!(bad, 0, "{key}: edith and ffprobe disagree");
}

/// `mkv_zlib_vobsub`: every zlib VobSub track. Blocks are read straight out of
/// the file (the whole file is read: the VobSub titles are tens of megabytes)
/// and inflated with `ec_inflate`, the decoder the Matroska demuxer calls.
fn check_vobsub(key: &str) {
    let Some(path) = engine::real_library::film(key) else {
        return;
    };
    if !have("ffprobe") {
        eprintln!("skipped: no ffprobe to state the reference packets");
        return;
    }
    let started = Instant::now();
    let buf = std::fs::read(&path).expect("read the file");
    let file = walk(&buf, true);
    let ff_streams = ffprobe_streams(&path);
    let subs = matroska_subtitles(&path).expect("edith reads the subtitle tracks");

    let mut tracks = 0;
    let mut packets = 0;
    let mut bad = 0;
    for (pos, t) in file.tracks.iter().enumerate() {
        if t.kind != SUBTITLE || t.codec != VOBSUB || !t.zlib() {
            continue;
        }
        tracks += 1;
        // Edith declares the track and accepts its encoding. It reads no VobSub
        // payload, so nothing about the blocks is asked of edith here.
        let sub = subs
            .iter()
            .find(|s| s.number == t.number)
            .unwrap_or_else(|| panic!("{key}: edith lists no subtitle track {}", t.number));
        assert_eq!(
            sub.codec, VOBSUB,
            "{key}: edith's codec for track {}",
            t.number
        );
        assert!(
            sub.unsupported.is_none(),
            "{key}: edith refuses the zlib track: {:?}",
            sub.unsupported
        );

        let index = pos as u32;
        assert_eq!(
            ff_streams[pos].kind, "subtitle",
            "{key}: ffprobe stream {index} is not a subtitle stream"
        );
        let tb = time_base(&ff_streams, index);
        let mut ours = Vec::new();
        for b in file.blocks.iter().filter(|b| b.track == t.number) {
            assert_eq!(b.lacing, 0, "{key}: a laced VobSub block is not expected");
            let inflated = ec_inflate::inflate_zlib(&buf[b.range.clone()], LIMIT)
                .unwrap_or_else(|e| panic!("{key}: a block did not inflate: {e}"));
            ours.push(Got {
                // Block ticks are in the segment's TimestampScale (nanoseconds
                // a tick), so microseconds are ticks * scale / 1000.
                start_us: i128::from(b.ticks) * i128::from(file.timestamp_scale) / 1000,
                // A SimpleBlock states no duration; BlockDuration is not read
                // here, so the duration is left for ffprobe to state alone.
                duration_us: None,
                len: inflated.len(),
                md5: md5_hex(&inflated),
            });
        }
        assert!(!ours.is_empty(), "{key}: no blocks for track {index}");
        let ff = ffprobe_packets_of(&path, index);
        packets += ours.len();
        bad += compare(key, &format!("track {index}"), &ours, &ff, tb);
        eprintln!(
            "{key}: track {index} (VobSub, zlib): {} packets compared with ffprobe",
            ours.len()
        );
    }
    assert!(
        tracks > 0,
        "{key}: no zlib-compressed VobSub track in the file"
    );
    eprintln!(
        "{key}: {packets} packets over {tracks} zlib VobSub track(s), {bad} mismatches, {:.1} s",
        started.elapsed().as_secs_f64()
    );
    assert_eq!(bad, 0, "{key}: ec_inflate and ffprobe disagree");
}

#[test]
fn zlib_pgs_tracks_match_ffprobe() {
    check_pgs("mkv_zlib_pgs");
}

#[test]
fn zlib_vobsub_tracks_match_ffprobe() {
    check_vobsub("mkv_zlib_vobsub");
}
