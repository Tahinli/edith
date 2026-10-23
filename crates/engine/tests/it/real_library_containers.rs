//! The mp4 demuxer against ffprobe, on the owner's own files. For each stream
//! the packet table `ec-mp4` hands out -- payload bytes, pts, dts, duration,
//! keyframe flag -- is compared packet for packet with
//! `ffprobe -show_packets -show_data_hash md5`, and the stream's codec tag,
//! time base, size or sample format is compared with ffprobe's stream entry.
//!
//! Each file is named by `real_library.toml` (gitignored); a key it does not
//! name, or a path not on this machine, skips. Needs ffprobe.

use crate::reallib;

use std::collections::HashMap;
use std::fs::File;

use ec_core::Demuxer as _;
use reallib::{ffprobe_packets, ffprobe_streams, have, md5_hex};

/// One packet as `ec-mp4` states it, in the stream's own time base.
#[derive(Debug, PartialEq)]
struct Row {
    pts: Option<i64>,
    dts: i64,
    duration: Option<i64>,
    size: usize,
    key: bool,
    md5: String,
}

fn check_mp4(key: &str) {
    let Some(path) = engine::real_library::film(key) else {
        return;
    };
    if !have("ffprobe") {
        eprintln!("skipped: no ffprobe to state the reference packets");
        return;
    }
    let ff_streams = ffprobe_streams(&path);
    let ff_packets = ffprobe_packets(&path);

    let mut demux =
        ec_mp4::Mp4Demuxer::new(File::open(&path).expect("open")).expect("ec-mp4 opens");
    let streams = demux.streams().to_vec();
    let tracks = demux.tracks().to_vec();

    // ec streams are matched to ffprobe streams by their sample-entry tag, the
    // k-th of a tag to the k-th of that tag in track order.
    let mut ff_by_tag: HashMap<String, Vec<&reallib::FfStream>> = HashMap::new();
    for s in &ff_streams {
        ff_by_tag.entry(s.tag.clone()).or_default().push(s);
    }
    let mut taken: HashMap<String, usize> = HashMap::new();
    let mut pairs = Vec::new();
    for (i, info) in streams.iter().enumerate() {
        let track = tracks
            .iter()
            .find(|t| t.stream == Some(i as u32))
            .expect("every ec stream is a track");
        let tag = String::from_utf8_lossy(&track.sample_entry).to_string();
        let n = taken.entry(tag.clone()).or_insert(0);
        let Some(ff) = ff_by_tag.get(&tag).and_then(|v| v.get(*n)) else {
            panic!("{key}: ec stream {i} ({tag}) has no ffprobe stream with that tag");
        };
        *n += 1;
        pairs.push((
            i as u32,
            ff.index,
            (info.time_base, info.params.media.clone()),
            (*ff).clone(),
        ));
    }
    eprintln!(
        "{key}: {} of {} ffprobe streams compared (the rest are not picture or sound tracks ec-mp4 maps)",
        pairs.len(),
        ff_streams.len()
    );

    // Stream metadata against ffprobe.
    for (ec, ff_index, (tb, media), ff) in &pairs {
        let (num, den) = ff
            .time_base
            .split_once('/')
            .map(|(a, b)| (a.parse::<f64>().unwrap(), b.parse::<f64>().unwrap()))
            .expect("ffprobe time base");
        let ff_secs = num / den;
        assert!(
            (tb.as_secs_f64() - ff_secs).abs() < 1e-12,
            "{key}: stream {ec} time base {} vs ffprobe {}",
            tb.as_secs_f64(),
            ff_secs
        );
        match media {
            ec_core::MediaParameters::Video(v) => {
                assert_eq!(
                    (Some(v.width), Some(v.height)),
                    (ff.width, ff.height),
                    "{key}: stream {ec} picture size"
                );
            }
            ec_core::MediaParameters::Audio(a) => {
                assert_eq!(
                    Some(a.sample_rate),
                    ff.sample_rate,
                    "{key}: stream {ec} sample rate"
                );
            }
            ec_core::MediaParameters::Subtitle => {}
        }
        eprintln!(
            "{key}: stream {ec} -> ffprobe {ff_index} {} {} {:?}x{:?} {:?}Hz {:?}ch",
            ff.kind, ff.tag, ff.width, ff.height, ff.sample_rate, ff.channels
        );
    }

    // Packet tables, per stream, in each stream's own order.
    let mut ec_rows: HashMap<u32, Vec<Row>> = HashMap::new();
    loop {
        let p = match demux.next_packet() {
            Ok(p) => p,
            Err(ec_core::Error::Eof) => break,
            Err(e) => panic!("{key}: ec-mp4 packet read: {e}"),
        };
        ec_rows.entry(p.stream).or_default().push(Row {
            pts: p.pts,
            dts: p.dts.or(p.pts).unwrap_or(0),
            duration: p.duration,
            size: p.data.len(),
            key: p.flags.keyframe,
            md5: md5_hex(&p.data),
        });
    }

    for (ec, ff_index, _, _) in &pairs {
        let want: Vec<Row> = ff_packets
            .iter()
            .filter(|p| p.stream == *ff_index)
            .map(|p| Row {
                pts: p.pts,
                dts: p.dts.or(p.pts).unwrap_or(0),
                duration: p.duration,
                size: p.size,
                key: p.key,
                md5: p.md5.clone(),
            })
            .collect();
        let got = ec_rows.remove(ec).unwrap_or_default();
        assert_eq!(got.len(), want.len(), "{key}: stream {ec} packet count");
        let mut differ = [0usize; 6];
        for (g, w) in got.iter().zip(&want) {
            differ[0] += (g.pts != w.pts) as usize;
            differ[1] += (g.dts != w.dts) as usize;
            differ[2] += (g.duration != w.duration) as usize;
            differ[3] += (g.size != w.size) as usize;
            differ[4] += (g.key != w.key) as usize;
            differ[5] += (g.md5 != w.md5) as usize;
        }
        let ec_sum: i64 = got.iter().filter_map(|g| g.duration).sum();
        eprintln!(
            "{key}: stream {ec}: {} packets; differing pts/dts/duration/size/key/md5 = {differ:?}; \
             sum of ec durations {ec_sum}, ffprobe stream duration_ts {:?}",
            got.len(),
            ff_streams
                .iter()
                .find(|s| s.index == *ff_index)
                .map(|s| s.duration_ts)
        );
        assert_eq!(
            differ[..2],
            [0, 0],
            "{key}: stream {ec}: timestamps differ from ffprobe"
        );
        assert_eq!(
            differ[3..],
            [0, 0, 0],
            "{key}: stream {ec}: size/key/payload differ from ffprobe"
        );
        // A packet duration ffprobe states differently is only excused where
        // ffprobe's own figure is unusable (N/A or 1 tick, on every packet of
        // a fragmented VP9 track). ec-mp4 then gives the fragment's trun sample
        // duration, which the file itself states; see the note in the report.
        if differ[2] > 0 {
            let unusable = want
                .iter()
                .filter(|w| matches!(w.duration, None | Some(1)))
                .count();
            assert_eq!(
                unusable, differ[2],
                "{key}: stream {ec}: duration differs from ffprobe on packets it states usably"
            );
            eprintln!(
                "{key}: stream {ec}: {} packets where ffprobe states no usable duration (N/A or 1)",
                unusable
            );
        }
    }
}

/// H.264 + AAC-LC, 640x1138, 44.1 kHz stereo: the plain mp4 case.
#[test]
fn real_mp4_h264_aac_packets_match_ffprobe() {
    check_mp4("mp4_h264_aac");
}

/// A phone recording: H.264 High 720p with full-range (yuvj420p) pixels and
/// AAC-LC at 48 kHz.
#[test]
fn real_mp4_phone_h264_packets_match_ffprobe() {
    check_mp4("mp4_phone_h264");
}

/// An H.264 4K-wide film with an AAC-LC track and a `mov_text` subtitle track;
/// the subtitle packets are not ec-mp4 picture or sound packets, so they are
/// not compared.
#[test]
fn real_mp4_mov_text_packets_match_ffprobe() {
    check_mp4("mp4_mov_text");
}

/// An HE-AAC v1 (SBR) track in mp4: the packet table is the same shape as AAC-LC,
/// the decode is checked in `real_library_audio`.
#[test]
fn real_mp4_heaac_packets_match_ffprobe() {
    check_mp4("mp4_heaac");
}

/// VP9 Profile 0 and Opus in mp4: the packets of both tracks.
#[test]
fn real_mp4_vp9_opus_packets_match_ffprobe() {
    check_mp4("vp9_opus_mp4");
}
