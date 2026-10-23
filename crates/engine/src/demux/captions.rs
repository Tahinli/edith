//! Embedded closed captions (CEA-608 CC1-CC4, CEA-708 service 1) as subtitle tracks.
//!
//! The captions ride inside the *picture* track (H.264/HEVC SEI, MPEG-2 user data), so finding
//! them reads every picture packet: the walk is lazy and cached. [`present`] is the cheap
//! answer for a track list (the first seconds only); [`tracks`] is the full scan an import runs.
//! Both are cached per file (path, size, mtime).
//!
//! A caption track's `number` is synthetic ([`TRACK_BASE`] plus the channel) so it can sit in the
//! same `(file, track)` row a container's own subtitle track does. CC1-CC4 are 1-4, service 1 is 5.
//!
//! corner-cut: QuickTime `c608`/`c708` caption tracks of an mov are not read -- `ec-mp4` exposes
//! no stream for a sample entry it does not know, so their samples are out of reach from here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use super::*;

/// Added to the channel to make a caption track's number in a `(file, track)` row; far above any
/// real container track number.
pub const TRACK_BASE: u64 = 1 << 32;

/// Seconds of picture the presence probe reads.
const PROBE_SECS: i64 = 10;

/// Whether a track number names an embedded caption track.
pub fn is_caption_track(number: u64) -> bool {
    number > TRACK_BASE && number <= TRACK_BASE + 5
}

/// What a list shows for caption track `number`: `CC1 (608)` ... `CC4 (608)`, `Service 1 (708)`.
pub fn track_name(number: u64) -> String {
    match number.saturating_sub(TRACK_BASE) {
        n @ 1..=4 => format!("CC{n} (608)"),
        5 => "Service 1 (708)".to_string(),
        _ => format!("caption track {number}"),
    }
}

/// One embedded caption track: its number, its name and, for a full scan, its cues.
#[derive(Clone, Debug)]
pub struct CaptionTrack {
    pub number: u64,
    pub name: String,
    pub cues: Vec<ec_cc::CaptionCue>,
}

type Key = (std::path::PathBuf, u64, Option<SystemTime>);
type Cache = Mutex<HashMap<Key, Arc<Vec<CaptionTrack>>>>;

fn key(path: &Path) -> Key {
    let meta = std::fs::metadata(path).ok();
    (
        path.to_path_buf(),
        meta.as_ref().map_or(0, |m| m.len()),
        meta.and_then(|m| m.modified().ok()),
    )
}

fn cache(full: bool) -> &'static Cache {
    static FULL: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    static PROBE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    if full { &FULL } else { &PROBE }.get_or_init(Default::default)
}

/// Every picture of `path` as presentation-ordered caption triplets, whichever container.
fn scan(path: &Path, limit_us: Option<i64>) -> crate::Result<Vec<ec_cc::TimedCc>> {
    if is_matroska(path) {
        scan_mkv(path, limit_us)
    } else if ps::is_mpeg_system(path) {
        ts::caption_triplets(path, limit_us)
    } else {
        scan_mp4(path, limit_us)
    }
}

fn scan_mkv(path: &Path, limit_us: Option<i64>) -> crate::Result<Vec<ec_cc::TimedCc>> {
    let (meta, mut mkv) = MkvDemuxer::open(path)?;
    let fmt = |n: u8| ec_cc::extract::NalFormat::LengthPrefixed(n);
    let rate = ((meta.frame_rate * 1000.0).round().max(1.0) as u32, 1000);
    let private = mkv.codec_private().to_vec();
    enum Ex {
        H264(ec_cc::H264Extractor),
        Hevc(ec_cc::HevcExtractor),
    }
    let mut ex = match meta.codec {
        Codec::H264 => {
            let mut e = ec_cc::H264Extractor::new(fmt(4));
            e.set_extradata(&private);
            Ex::H264(e)
        }
        Codec::Hevc => {
            let mut e = ec_cc::HevcExtractor::new(fmt(4));
            e.set_extradata(&private);
            Ex::Hevc(e)
        }
        _ => return Ok(Vec::new()),
    };
    for i in 0..mkv.block_count() {
        let Some(block) = mkv.coded_block(i)? else { break };
        let us = block.ts_ns / 1000;
        if limit_us.is_some_and(|l| us > l) {
            break;
        }
        match &mut ex {
            Ex::H264(e) => e.push(block.bytes, Some(us)),
            Ex::Hevc(e) => e.push(block.bytes, Some(us)),
        }
    }
    Ok(match ex {
        Ex::H264(e) => e.finish(rate),
        Ex::Hevc(e) => e.finish(rate),
    })
}

fn scan_mp4(path: &Path, limit_us: Option<i64>) -> crate::Result<Vec<ec_cc::TimedCc>> {
    let file = File::open(path)?;
    let mut reader = ec_mp4::Mp4Demuxer::new(BufReader::new(file)).map_err(mp4_error)?;
    let Some(info) = reader
        .tracks()
        .iter()
        .find(|t| t.media == Some(ec_core::MediaType::Video) && t.stream.is_some())
        .cloned()
    else {
        return Ok(Vec::new());
    };
    let stream = info.stream.unwrap_or(0);
    let (fourcc, entry) = sample_entry(path, info.track_id)?;
    let (num, den) = reader
        .streams()
        .iter()
        .find(|s| s.index == stream)
        .map_or((1, 1000), |s| (s.time_base.num(), s.time_base.den()));
    let us = move |ticks: i64| (i128::from(ticks) * i128::from(num) * 1_000_000 / i128::from(den.max(1))) as i64;
    let count = reader.samples(stream).len();
    let ticks = reader.samples(stream).iter().map(|s| i64::from(s.duration)).sum::<i64>().max(1);
    let rate = (((count as i128 * i128::from(den.max(1))) / i128::from(ticks.max(1)) * 1000) as u32, 1000);
    let rate = if rate.0 == 0 { (30_000, 1000) } else { rate };
    let body = entry.get(78..).unwrap_or_default();
    enum Ex {
        H264(ec_cc::H264Extractor),
        Hevc(ec_cc::HevcExtractor),
    }
    let mut ex = match &fourcc {
        b"avc1" | b"avc3" => {
            let mut e = ec_cc::H264Extractor::new(ec_cc::extract::NalFormat::LengthPrefixed(4));
            e.set_extradata(child(body, b"avcC").unwrap_or_default());
            Ex::H264(e)
        }
        b"hvc1" | b"hev1" => {
            let mut e = ec_cc::HevcExtractor::new(ec_cc::extract::NalFormat::LengthPrefixed(4));
            e.set_extradata(child(body, b"hvcC").unwrap_or_default());
            Ex::Hevc(e)
        }
        _ => return Ok(Vec::new()),
    };
    for i in 0..count {
        let cts = us(reader.samples(stream)[i].cts);
        if limit_us.is_some_and(|l| cts > l) {
            break;
        }
        let packet = match reader.read_sample(stream, i) {
            Ok(p) => p,
            Err(ec_core::Error::Eof) => break,
            Err(e) => return Err(mp4_error(e)),
        };
        match &mut ex {
            Ex::H264(e) => e.push(packet.data.as_ref(), Some(cts)),
            Ex::Hevc(e) => e.push(packet.data.as_ref(), Some(cts)),
        }
    }
    Ok(match ex {
        Ex::H264(e) => e.finish(rate),
        Ex::Hevc(e) => e.finish(rate),
    })
}

/// The caption tracks of `path` with all their cues: the full scan, once per file. Empty when the
/// picture carries no captions (or is a codec whose captions are not read); never an error for a
/// file the scan cannot walk -- a file's own subtitle tracks must still list.
pub fn tracks(path: &Path) -> Arc<Vec<CaptionTrack>> {
    let k = key(path);
    if let Some(hit) = cache(true).lock().ok().and_then(|c| c.get(&k).cloned()) {
        return hit;
    }
    let found = Arc::new(scan(path, None).map(|t| decode(&t)).unwrap_or_default());
    if let Ok(mut c) = cache(true).lock() {
        c.insert(k, found.clone());
    }
    found
}

fn decode(timed: &[ec_cc::TimedCc]) -> Vec<CaptionTrack> {
    let dec = ec_cc::decode_all(timed);
    let mut out: Vec<CaptionTrack> = Vec::new();
    let all = dec.cea608.into_iter().map(|c| (u64::from(c.channel), c)).chain(dec.cea708.into_iter().map(|c| (5, c)));
    for (channel, c) in all {
        let number = TRACK_BASE + channel;
        match out.iter_mut().find(|t| t.number == number) {
            Some(t) => t.cues.push(c),
            None => out.push(CaptionTrack { number, name: track_name(number), cues: vec![c] }),
        }
    }
    out.sort_by_key(|t| t.number);
    out
}

/// The caption tracks `path` appears to carry, from the first seconds of picture only: number and
/// name, no cues. Cached. A listing aid -- a file whose captions start later lists none here, and
/// the import's full scan ([`tracks`]) is the truth.
pub fn present(path: &Path) -> Arc<Vec<CaptionTrack>> {
    let k = key(path);
    if let Some(hit) = cache(true).lock().ok().and_then(|c| c.get(&k).cloned()) {
        return hit;
    }
    if let Some(hit) = cache(false).lock().ok().and_then(|c| c.get(&k).cloned()) {
        return hit;
    }
    let timed = scan(path, Some(PROBE_SECS * 1_000_000)).unwrap_or_default();
    let mut found: Vec<u64> = decode(&timed).iter().map(|t| t.number).collect();
    // Captions are on the wire but no cue has closed yet: name the channel the pairs are on.
    for t in timed.iter().flat_map(|t| &t.triplets).filter(|t| t.valid && (t.data[0] & 0x7f) != 0) {
        let n = match t.cc_type {
            0 => 1,
            1 => 3,
            2 | 3 => 5,
            _ => continue,
        } + TRACK_BASE;
        if !found.contains(&n) {
            found.push(n);
        }
    }
    found.sort_unstable();
    let out = Arc::new(found.into_iter().map(|number| CaptionTrack { number, name: track_name(number), cues: Vec::new() }).collect());
    if let Ok(mut c) = cache(false).lock() {
        c.insert(k, Arc::clone(&out));
    }
    out
}
